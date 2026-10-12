import pathlib
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parent.parent
BUILD_ANCHOR = 'apply(from = "tauri.build.gradle.kts")'
BUILD_HOOK = 'apply(from = "../../../../capture.gradle.kts")'
SETTINGS_ANCHOR = "apply from: 'tauri.settings.gradle'"
SETTINGS_HOOK = "apply from: '../../../capture.settings.gradle'"


class PrepareAndroidCaptureTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.script = self.root / 'scripts/prepare-android-capture.py'
        self.script.parent.mkdir()
        self.script.write_bytes((ROOT / 'scripts/prepare-android-capture.py').read_bytes())
        self.settings = self.root / 'apps/android-tauri/src-tauri/gen/android/settings.gradle'
        self.build = self.settings.parent / 'app/build.gradle.kts'
        self.build.parent.mkdir(parents=True)
        self.settings.write_text("include ':app'\n\n" + SETTINGS_ANCHOR + '\n')
        self.build.write_text('plugins {}\n\n' + BUILD_ANCHOR + '\n')

    def run_hook(self):
        return subprocess.run([sys.executable, str(self.script)], cwd=self.root,
                              capture_output=True, text=True)

    def assert_rejected_without_writes(self):
        before = (self.build.read_bytes(), self.settings.read_bytes())
        result = self.run_hook()
        self.assertNotEqual(0, result.returncode)
        self.assertIn('Cannot', result.stderr)
        self.assertEqual(before, (self.build.read_bytes(), self.settings.read_bytes()))

    def test_clean_scaffold_inserts_both_hooks_before_generator(self):
        result = self.run_hook()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertEqual('plugins {}\n\n' + BUILD_HOOK + '\n' + BUILD_ANCHOR + '\n', self.build.read_text())
        self.assertEqual("include ':app'\n\n" + SETTINGS_HOOK + '\n' + SETTINGS_ANCHOR + '\n', self.settings.read_text())

    def test_repeated_preparation_is_byte_identical(self):
        self.assertEqual(0, self.run_hook().returncode)
        before = (self.build.read_bytes(), self.settings.read_bytes())
        self.assertEqual(0, self.run_hook().returncode)
        self.assertEqual(before, (self.build.read_bytes(), self.settings.read_bytes()))

    def test_preparation_enables_local_build_cache_after_scaffold_generation(self):
        properties = self.settings.parent / 'gradle.properties'
        properties.write_bytes(b'android.useAndroidX=true\r\n')
        self.assertEqual(0, self.run_hook().returncode)
        self.assertEqual(b'android.useAndroidX=true\r\norg.gradle.caching=true\r\n', properties.read_bytes())
        before = properties.read_bytes()
        self.assertEqual(0, self.run_hook().returncode)
        self.assertEqual(before, properties.read_bytes())

    def test_explicit_build_cache_opt_out_is_preserved(self):
        properties = self.settings.parent / 'gradle.properties'
        properties.write_text('org.gradle.caching=false\n')
        before = properties.read_bytes()
        self.assertEqual(0, self.run_hook().returncode)
        self.assertEqual(before, properties.read_bytes())

    def test_crlf_scaffold_preserves_line_endings_and_repeated_bytes(self):
        self.build.write_bytes((BUILD_ANCHOR + '\r\n').encode())
        self.settings.write_bytes((SETTINGS_ANCHOR + '\r\n').encode())
        self.assertEqual(0, self.run_hook().returncode)
        self.assertEqual((BUILD_HOOK + '\r\n' + BUILD_ANCHOR + '\r\n').encode(), self.build.read_bytes())
        self.assertEqual((SETTINGS_HOOK + '\r\n' + SETTINGS_ANCHOR + '\r\n').encode(), self.settings.read_bytes())
        before = (self.build.read_bytes(), self.settings.read_bytes())
        self.assertEqual(0, self.run_hook().returncode)
        self.assertEqual(before, (self.build.read_bytes(), self.settings.read_bytes()))

    def test_existing_app_hook_is_preserved_and_settings_hook_added(self):
        self.build.write_text(BUILD_HOOK + '\n' + BUILD_ANCHOR + '\n')
        before = self.build.read_bytes()
        self.assertEqual(0, self.run_hook().returncode)
        self.assertEqual(before, self.build.read_bytes())
        self.assertIn(SETTINGS_HOOK, self.settings.read_text())

    def test_missing_build_anchor(self):
        self.build.write_text('plugins {}\n')
        self.assert_rejected_without_writes()

    def test_existing_hook_does_not_bypass_anchor_validation(self):
        self.build.write_text(BUILD_HOOK + '\n')
        self.assert_rejected_without_writes()

    def test_missing_build_file_does_not_change_settings(self):
        self.build.unlink()
        before = self.settings.read_bytes()
        result = self.run_hook()
        self.assertNotEqual(0, result.returncode)
        self.assertIn('Cannot', result.stderr)
        self.assertEqual(before, self.settings.read_bytes())

    def test_missing_settings_anchor(self):
        self.settings.write_text("include ':app'\n")
        self.assert_rejected_without_writes()

    def test_duplicate_build_anchor(self):
        self.build.write_text((BUILD_ANCHOR + '\n') * 2)
        self.assert_rejected_without_writes()

    def test_duplicate_settings_anchor(self):
        self.settings.write_text((SETTINGS_ANCHOR + '\n') * 2)
        self.assert_rejected_without_writes()

    def test_duplicate_build_hook(self):
        self.build.write_text((BUILD_HOOK + '\n') * 2 + BUILD_ANCHOR + '\n')
        self.assert_rejected_without_writes()

    def test_duplicate_settings_hook(self):
        self.settings.write_text((SETTINGS_HOOK + '\n') * 2 + SETTINGS_ANCHOR + '\n')
        self.assert_rejected_without_writes()

    def test_conflicting_build_hook_path(self):
        self.build.write_text('apply(from = "wrong/capture.gradle.kts")\n' + BUILD_ANCHOR + '\n')
        self.assert_rejected_without_writes()

    def test_conflicting_settings_hook_path(self):
        self.settings.write_text("apply from: 'wrong/capture.settings.gradle'\n" + SETTINGS_ANCHOR + '\n')
        self.assert_rejected_without_writes()

    def test_build_hook_after_anchor_is_rejected(self):
        self.build.write_text(BUILD_ANCHOR + '\n' + BUILD_HOOK + '\n')
        self.assert_rejected_without_writes()

    def test_settings_hook_after_anchor_is_rejected(self):
        self.settings.write_text(SETTINGS_ANCHOR + '\n' + SETTINGS_HOOK + '\n')
        self.assert_rejected_without_writes()

    def test_generated_module_mapping_conflicts_with_authored_owner(self):
        self.settings.write_text("include ':capture-android'\n" + SETTINGS_ANCHOR + '\n')
        self.assert_rejected_without_writes()

    def test_missing_settings_file_does_not_change_build(self):
        self.settings.unlink()
        before = self.build.read_bytes()
        result = self.run_hook()
        self.assertNotEqual(0, result.returncode)
        self.assertIn('Cannot', result.stderr)
        self.assertEqual(before, self.build.read_bytes())

    def test_authored_hook_and_module_paths_resolve_from_each_root(self):
        phone = ROOT / 'apps/android-tauri/src-tauri/gen/android'
        wear = ROOT / 'apps/wear'
        self.assertEqual(ROOT / 'apps/android-tauri/capture.gradle.kts',
                         (phone / 'app/../../../../capture.gradle.kts').resolve())
        authored = (phone / '../../../capture.settings.gradle').resolve()
        self.assertEqual(ROOT / 'apps/android-tauri/capture.settings.gradle', authored)
        module = ROOT / 'crates/capture-android'
        self.assertEqual(module, (phone / '../../../../../crates/capture-android').resolve())
        self.assertEqual(module, (wear / '../../crates/capture-android').resolve())
        self.assertIn('../../../../../crates/capture-android', authored.read_text())


if __name__ == '__main__':
    unittest.main()
