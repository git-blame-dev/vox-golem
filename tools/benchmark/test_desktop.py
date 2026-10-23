import asyncio
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import Mock, patch

import desktop


class DesktopTests(unittest.TestCase):
    def test_async_javascript_bridge_awaits_promises_and_omits_errors(self):
        driver = Mock()
        driver.execute_async_script.return_value = {'ok': True, 'value': 42}
        self.assertEqual(asyncio.run(desktop.SeleniumUI(driver).evaluate('Promise.resolve(42)')), 42)
        script, expression = driver.execute_async_script.call_args.args
        self.assertIn('Promise.resolve', script)
        self.assertEqual(expression, 'Promise.resolve(42)')
        driver.execute_async_script.return_value = {'ok': False, 'error': 'private payload'}
        with self.assertRaisesRegex(RuntimeError, '^desktop_javascript_failed_payload_omitted$'):
            asyncio.run(desktop.SeleniumUI(driver).evaluate('throw 1'))
        driver.execute_async_script.side_effect = RuntimeError('private payload')
        with self.assertRaisesRegex(RuntimeError, '^desktop_evaluation_failed_payload_omitted$'):
            asyncio.run(desktop.SeleniumUI(driver).evaluate('1'))

    def run_inside(self, failure=None, status='unverified'):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root/'binary'
            binary.write_bytes(b'synthetic')
            args = SimpleNamespace(binary=binary, config=root/'unresolved'/'..'/'config', output=root,
                                   offline_evidence=root/'qualification', flow_timeout=10)
            events = []
            runner = Mock()
            runner.ASSIGNMENT_LEDGER = root.parent/'fixed-ledger'
            runner.LIVE_BATCH_ALLOWANCE = 12
            for name in ('validate_benchmark_binary', 'validate_offline_evidence', 'reserve_live_budget',
                         'isolated_environment', 'start_typed_app', 'shutdown_owned'):
                getattr(runner, name).side_effect = lambda *a, _name=name, **k: events.append(_name)
            runner.reserve_live_budget.side_effect = lambda *a: events.append('reserve_live_budget') or 12
            runner.start_typed_app.side_effect = lambda *a: events.append('start_typed_app') or (Mock(), Mock(), 1)
            runner.shutdown_owned.side_effect = lambda *a: events.append('shutdown_owned') or {'app_exit_before_group_termination_observed': True}
            runner.harness_identity.return_value = 'synthetic-hash'
            async def suite(ui, safe, report):
                events.append('suite')
                self.assertEqual(safe.model, 'custom-luna-none')
                self.assertEqual(safe.repetitions, 1)
                if failure:
                    raise RuntimeError('private payload')
                report['status'] = status
            result = desktop.execute_inside(args, runner, SimpleNamespace(run_suite=suite))
            report = runner.private_json.call_args.args[1]
            self.assertEqual(events, ['validate_benchmark_binary', 'validate_offline_evidence',
                'reserve_live_budget', 'isolated_environment', 'start_typed_app', 'suite', 'shutdown_owned'])
            runner.validate_offline_evidence.assert_called_once_with(args.offline_evidence, binary, {}, {}, 'typed')
            runner.reserve_live_budget.assert_called_once_with(runner.ASSIGNMENT_LEDGER, root/'reservation.json')
            runner.isolated_environment.assert_called_once_with(root, args.config, None, True,
                                                               root/'reservation.json', entry='typed')
            self.assertNotIn('private payload', json.dumps(report))
            return result, report

    def test_shared_suite_preserves_qualification_reservation_isolation_and_retirement(self):
        result, report = self.run_inside()
        self.assertEqual(result, 2)
        self.assertEqual(report['status'], 'unverified')

    def test_failure_still_retires_owned_app_and_publishes_partial_report(self):
        result, report = self.run_inside(True)
        self.assertEqual(result, 1)
        self.assertEqual(report['status'], 'fail')

    def test_blocked_child_exits_three_after_normal_retirement(self):
        result, report = self.run_inside(status='blocked')
        self.assertEqual(result, 3)
        self.assertEqual(report['status'], 'blocked')

    def run_parent(self, status='unverified', code=2):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)/'repo'
            root.mkdir()
            for name in ('binary', 'config', 'evidence', 'tools/venv/bin/python',
                         'tools/bin/tauri-driver', 'tools/webkit/extracted/usr/bin/WebKitWebDriver'):
                path = root/name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            args = SimpleNamespace(live=True, model='custom-luna-none', repetitions=1,
                turn_timeout=10, max_live_calls=7, binary=root/'binary', config=root/'config',
                offline_evidence=root/'evidence', tools=root/'tools')
            process = Mock(returncode=code)
            def launch(command, **kwargs):
                self.assertIn('--shared-suite', command)
                self.assertIn('--offline-evidence', command)
                self.assertEqual(command[command.index('--config')+1], str(args.config))
                self.assertNotIn('--inside-session', command)
                self.assertEqual(command[1], str(root/'tools/app-bench/runner.py'))
                self.assertTrue(kwargs['start_new_session'])
                output = Path(command[command.index('--output')+1])
                output.mkdir()
                (output/'report.json').write_text(json.dumps({'schema':1,
                    'suite':'typed-model-matrix', 'platform':'desktop', 'status':status,
                    'coverage':'full', 'selected_cases':['page_read'], 'omitted_cases':[],
                    'cases':[], 'cleanup':{'desktop':{'app_exit_before_group_termination_observed':True}}}))
                return process
            report = {'cleanup':{}, 'source':{'revision':'synthetic'}, 'measurement':{'poll_interval_ms':100}}
            with patch.object(desktop, 'ROOT', root), patch.object(desktop.shutil, 'which', return_value='/synthetic/tool'), patch.object(desktop.subprocess, 'Popen', side_effect=launch):
                asyncio.run(desktop.run_desktop(args, report))
            self.assertEqual(report['status'], status)
            self.assertEqual(report['coverage'], 'full')
            self.assertEqual(report['selected_cases'], ['page_read'])
            self.assertEqual(report['source'], {'revision':'synthetic'})
            self.assertEqual(report['measurement']['poll_interval_ms'], 100)
            process.wait.assert_called_once_with(timeout=580)

    def test_parent_launches_only_existing_controller_and_preserves_common_identity(self):
        self.run_parent()

    def test_parent_preserves_blocked_status_and_selection(self):
        self.run_parent('blocked', 3)


if __name__ == '__main__':
    unittest.main()
