"""Read-only canonical benchmark storage discovery for ordinary Git worktrees."""
import os
from pathlib import Path
import subprocess


def artifact_root(checkout):
    """Use the primary checkout's outer workspace, never a linked tree's parent.

    Separate Git directories, bare repositories and submodules are unsupported.
    Discovery does not create storage or inspect the assignment ledger.
    """
    env = {key: value for key, value in os.environ.items()
           if not key.startswith('GIT_CONFIG') and key not in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR',
                          'GIT_INDEX_FILE', 'GIT_OBJECT_DIRECTORY',
                          'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_CEILING_DIRECTORIES',
                          'GIT_DISCOVERY_ACROSS_FILESYSTEM')}

    def query(directory, flag):
        value = subprocess.run(['git', '-C', str(directory), 'rev-parse',
                                '--path-format=absolute', flag], env=env,
                               check=True, capture_output=True, text=True).stdout.strip()
        if not value or '\n' in value or not Path(value).is_absolute():
            raise ValueError('Invalid Git workspace metadata')
        return Path(value).resolve(strict=True)

    try:
        checkout = Path(checkout).resolve(strict=True)
        if query(checkout, '--show-toplevel') != checkout:
            raise ValueError('Benchmark source must be a Git checkout root')
        common = query(checkout, '--git-common-dir')
        primary = common.parent
        marker = primary / '.git'
        if (common.name != '.git' or marker.is_symlink() or not marker.is_dir()
                or marker.resolve(strict=True) != common
                or query(primary, '--show-toplevel') != primary
                or query(primary, '--git-common-dir') != common):
            raise ValueError('Unsupported Git workspace layout')
        return primary.parent / 'artifacts'
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        raise ValueError('Canonical benchmark workspace unavailable or unsupported') from error
