import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPT = Path(os.environ.get('LLAMA_RECIPE_UNDER_TEST', Path(__file__).with_name('build-local-llama.sh')))


class LocalRuntimeRecipeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='synthetic-llama-recipe-')
        self.root = Path(self.temporary.name)
        self.source = self.root / 'synthetic-source'
        self.repository = self.root / 'synthetic-vox'
        self.tools = self.root / 'tools'
        self.build = self.root / 'external-build'
        self.runtime = self.root / 'external-runtime'
        self.marker = self.root / 'cmake-called'
        for folder in (self.source, self.repository / 'scripts', self.tools, self.root / 'cuda/bin'):
            folder.mkdir(parents=True)
        (self.source / 'LICENSE').write_text('Synthetic licence fixture\n')
        self.recipe = self.repository / 'scripts/build-local-llama.sh'
        shutil.copy2(SCRIPT, self.recipe)
        self.write_tool('git', '#!/bin/sh\ncase "$*" in *rev-parse*) echo ac2557cb24def295888ef47f1a35b401d978c510;; esac\n')
        self.write_tool('ninja', '#!/bin/sh\nexit 0\n')
        self.write_tool('patchelf', '''#!/bin/sh
set -eu
case "$1" in
  --set-rpath) test "$2" = '$ORIGIN'; test -f "$3";;
  --print-rpath) printf '%s\\n' '$ORIGIN';;
  *) exit 1;;
esac
''')
        self.write_tool('cmake', '''#!/bin/sh
set -eu
touch "$SYNTHETIC_CMAKE_MARKER"
if [ "$1" = --build ]; then
  mkdir -p "$2/bin"
  printf '#!/bin/sh\\nexit 0\\n' > "$2/bin/llama-server"
  chmod 755 "$2/bin/llama-server"
  printf 'synthetic library\\n' > "$2/bin/libsynthetic.so.1"
  ln -s libsynthetic.so.1 "$2/bin/libsynthetic.so"
  if [ "${SYNTHETIC_RACE:-}" != '' ]; then
    mkdir "$LLAMA_RUNTIME_DIR"
    stat -c %i "$LLAMA_RUNTIME_DIR" > "$SYNTHETIC_RACE_INODE"
    if [ "$SYNTHETIC_RACE" = nonempty ]; then touch "$LLAMA_RUNTIME_DIR/sentinel"; fi
  fi
fi
''')
        compiler = self.root / 'cuda/bin/nvcc'
        compiler.write_text('#!/bin/sh\necho synthetic-cuda-version\n')
        compiler.chmod(0o755)
        self.env = dict(os.environ,
                        PATH=str(self.tools) + ':' + os.environ['PATH'],
                        LLAMA_SOURCE=str(self.source), LLAMA_BUILD_DIR=str(self.build),
                        LLAMA_RUNTIME_DIR=str(self.runtime), CUDAToolkit_ROOT=str(self.root / 'cuda'),
                        PATCHELF=str(self.tools / 'patchelf'), SYNTHETIC_CMAKE_MARKER=str(self.marker),
                        SYNTHETIC_RACE_INODE=str(self.root / 'race-inode'))

    def tearDown(self):
        self.temporary.cleanup()

    def write_tool(self, name, body):
        path = self.tools / name
        path.write_text(body)
        path.chmod(0o755)

    def run_recipe(self, **changes):
        return subprocess.run(['bash', str(self.recipe)], env=dict(self.env, **changes),
                              capture_output=True, text=True)

    def test_rejects_checkout_outputs_and_overlapping_directories_before_cmake(self):
        cases = (
            ('LLAMA_BUILD_DIR', 'source', 'build'),
            ('LLAMA_BUILD_DIR', 'alias', 'build'),
            ('LLAMA_RUNTIME_DIR', 'repository', 'runtime'),
            ('LLAMA_BUILD_DIR', 'repository', 'build'),
            ('LLAMA_BUILD_DIR', 'runtime', ''),
            ('LLAMA_BUILD_DIR', 'runtime', 'nested'),
            ('LLAMA_RUNTIME_DIR', 'build', 'nested'),
        )
        for variable, root_name, suffix in cases:
            self.tearDown()
            self.setUp()
            alias = self.root / 'source-alias'
            alias.symlink_to(self.source, target_is_directory=True)
            roots = dict(source=self.source, alias=alias, repository=self.repository, runtime=self.runtime, build=self.build)
            changes = {variable: str(roots[root_name] / suffix)}
            with self.subTest(variable=variable, root=root_name, suffix=suffix):
                result = self.run_recipe(**changes)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.marker.exists())

    def test_refuses_both_empty_and_nonempty_racing_destination(self):
        for kind in ('empty', 'nonempty'):
            self.tearDown()
            self.setUp()
            with self.subTest(kind=kind):
                destination = self.root / ('race-' + kind)
                result = self.run_recipe(LLAMA_RUNTIME_DIR=str(destination), SYNTHETIC_RACE=kind)
                self.assertNotEqual(result.returncode, 0)
                original_inode = int((self.root / 'race-inode').read_text())
                self.assertEqual(destination.stat().st_ino, original_inode)
                self.assertEqual(sorted(p.name for p in destination.iterdir()), ['sentinel'] if kind == 'nonempty' else [])
                self.assertFalse(list(self.root.glob(destination.name + '.stage.*')))

    def test_publishes_external_bundle_with_links_license_and_hashes(self):
        result = self.run_recipe()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.runtime / 'bin/llama-server').is_file())
        self.assertTrue((self.runtime / 'bin/libsynthetic.so').is_symlink())
        self.assertEqual((self.runtime / 'LICENSE').read_text(), 'Synthetic licence fixture\n')
        subprocess.run(['sha256sum', '-c', 'SHA256SUMS'], cwd=self.runtime, check=True, capture_output=True)


if __name__ == '__main__':
    unittest.main()
