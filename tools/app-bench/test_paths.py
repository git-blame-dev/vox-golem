import os
import sys
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import paths
import runner


class CanonicalPathsTests(unittest.TestCase):
    def git(self, root, *args):
        return subprocess.run(['git', '-C', str(root), *args], check=True,
                              capture_output=True, text=True)

    def primary(self, outer):
        root = outer / 'arbitrary-primary'
        root.mkdir()
        self.git(root, 'init')
        self.git(root, '-c', 'user.name=Synthetic Test', '-c',
                 'user.email=synthetic@example.invalid', 'commit', '--allow-empty', '-m', 'fixture')
        return root

    def test_registered_linked_checkout_and_aliases_share_existing_debit(self):
        # Git writes here are confined to disposable synthetic repositories;
        # no development worktree or real assignment ledger is modified.
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temporary:
            outer = Path(temporary)
            primary = self.primary(outer)
            linked = outer / 'unrelated/deep/linked'
            linked.parent.mkdir(parents=True)
            self.git(primary, 'worktree', 'add', '--detach', str(linked))
            artifacts = outer / 'artifacts'
            artifacts.mkdir()
            ledger = artifacts / 'unified-app-bench-live-budget.json'
            ledger.write_text('{"schema": 1, "reserved": 24}')
            decoy = linked.parent / 'artifacts'
            decoy.mkdir()
            decoy_ledger = decoy / ledger.name
            decoy_ledger.write_text('{"schema": 1, "reserved": 0}')
            for index, checkout in enumerate((primary, linked)):
                alias = outer / ('alias-' + str(index))
                alias.symlink_to(checkout, target_is_directory=True)
                for entry in (checkout, alias):
                    self.assertEqual(paths.artifact_root(entry), artifacts)
                    self.assertEqual(ledger.read_text(), '{"schema": 1, "reserved": 24}')
            run = artifacts / 'private-run'
            run.mkdir(mode=0o700)
            self.assertEqual(runner.reserve_live_budget(
                paths.artifact_root(linked) / ledger.name, run / 'reservation.json'), 36)
            self.assertEqual(decoy_ledger.read_text(), '{"schema": 1, "reserved": 0}')
            argv = ['runner', '--binary', '/synthetic/app', '--tools', '/synthetic/tools',
                    '--config', '/synthetic/config', '--output', str(artifacts/'cli-run'),
                    '--entry', 'typed', '--live', '--shared-suite']
            with patch.object(runner, 'ROOT', linked), \
                    patch.object(runner, 'ARTIFACT_ROOT', None), \
                    patch.object(runner, 'ASSIGNMENT_LEDGER', None), \
                    patch.object(sys, 'argv', argv), \
                    patch.object(runner.shutil, 'which', return_value='/synthetic/tool'), \
                    patch.object(runner.subprocess, 'call', return_value=2):
                self.assertEqual(runner.main(), 2)
                self.assertEqual(runner.ARTIFACT_ROOT, artifacts)
                self.assertEqual(runner.ASSIGNMENT_LEDGER, ledger)
            self.assertIn('36', ledger.read_text())

    def test_non_git_bare_and_separate_git_dir_fail_without_creating_storage(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temporary:
            outer = Path(temporary)
            nongit = outer / 'nongit'
            nongit.mkdir()
            bare = outer / 'bare'
            self.git(outer, 'init', '--bare', str(bare))
            separate = outer / 'separate'
            self.git(outer, 'init', '--separate-git-dir', str(outer/'metadata'), str(separate))
            for checkout in (nongit, bare, separate):
                with self.assertRaises(ValueError):
                    paths.artifact_root(checkout)
            self.assertFalse((outer / 'artifacts').exists())

    def test_ambient_location_overrides_do_not_select_another_repository(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temporary:
            outer = Path(temporary)
            primary = self.primary(outer)
            with patch.dict(os.environ, {'GIT_DIR': '/synthetic/missing',
                    'GIT_WORK_TREE': '/synthetic/other', 'GIT_COMMON_DIR': '/synthetic/common'}):
                self.assertEqual(paths.artifact_root(primary), outer / 'artifacts')

    def test_missing_git_and_malformed_metadata_fail_closed(self):
        with patch.object(paths.subprocess, 'run', side_effect=FileNotFoundError):
            with self.assertRaises(ValueError):
                paths.artifact_root(Path(__file__).parent)
        with patch.object(paths.subprocess, 'run') as command:
            command.return_value.stdout = 'relative/path\n'
            with self.assertRaises(ValueError):
                paths.artifact_root(Path(__file__).parent)

    def test_runner_non_git_fails_before_storage_reservation_or_launch(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temporary:
            root = Path(temporary)
            argv = ['runner', '--binary', '/synthetic/app', '--tools', '/synthetic/tools',
                    '--config', '/synthetic/config', '--output', str(root/'artifacts/run')]
            with patch.object(runner, 'ROOT', root), \
                    patch.object(runner, 'ARTIFACT_ROOT', None), \
                    patch.object(runner, 'ASSIGNMENT_LEDGER', None), \
                    patch.object(sys, 'argv', argv), \
                    patch.object(runner, 'reserve_live_budget') as reserve, \
                    patch.object(runner, 'execute') as execute:
                with self.assertRaises(SystemExit):
                    runner.main()
                reserve.assert_not_called()
                execute.assert_not_called()
            self.assertFalse((root/'artifacts').exists())
