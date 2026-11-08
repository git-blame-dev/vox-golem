import io
import json
from pathlib import Path
import tempfile
import unittest
from contextlib import redirect_stdout, redirect_stderr
from unittest.mock import patch
from types import SimpleNamespace
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'app-bench'))

import benchmark
import desktop_voice as voice
import voice_contract


class DispatchTest(unittest.TestCase):
    def test_retired_desktop_voice_list_is_unsupported_without_transport(self):
        stdout, stderr = io.StringIO(), io.StringIO()
        with patch('sys.argv', ['benchmark', '--platform', 'desktop', '--workload', 'synthetic-voice', '--list']), \
                redirect_stdout(stdout), redirect_stderr(stderr), \
                patch.object(benchmark.subprocess, 'Popen', side_effect=AssertionError('no transport')), \
                patch.object(benchmark.subprocess, 'run', side_effect=AssertionError('no transport')):
            result = benchmark.main()
        self.assertEqual(result, 2)
        self.assertEqual(json.loads(stdout.getvalue())['status'], 'unsupported')
        self.assertIn('retired', json.loads(stdout.getvalue())['failure_category'])

    def test_retired_execution_fails_before_assets_or_transport(self):
        args = benchmark.parser().parse_args(['--platform', 'desktop', '--workload', 'synthetic-voice'])
        output = io.StringIO()
        with patch.object(voice, 'authorize', side_effect=AssertionError('no asset access')), \
                patch.object(voice, 'launch', side_effect=AssertionError('no transport')), redirect_stdout(output):
            self.assertEqual(voice.main(args, SimpleNamespace()), 2)
        self.assertIn('retired', json.loads(output.getvalue())['failure_category'])

    def test_removed_tts_model_option_is_rejected(self):
        with redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            benchmark.parser().parse_args(['--platform', 'desktop', '--tts-model', '/synthetic/model'])
        self.assertEqual(error.exception.code, 2)

    def test_typed_speech_list_requires_no_runtime_and_rejects_other_platforms(self):
        for platform, expected in (('desktop', 0), ('android', 1), ('wear', 1)):
            stdout, stderr = io.StringIO(), io.StringIO()
            with patch('sys.argv', ['benchmark', '--platform', platform, '--workload', 'typed-speech', '--list']), \
                    redirect_stdout(stdout), redirect_stderr(stderr), \
                    patch.object(voice, 'launch', side_effect=AssertionError('no transport')):
                self.assertEqual(benchmark.main(), expected)
            if platform == 'desktop':
                self.assertEqual(stdout.getvalue(), 'typed-speech-windows arithmetic one-trial diagnostic\n')


def report(platform='windows'):
    return {'schema':4, 'mode':'offline', 'entry':'typed-speech-windows',
        'requested_model':'custom-luna-none', 'diagnostic_only':True, 'qualified_for_live':False,
        'offline_profile_qualified':False, 'output_route':'wasapi-render-diagnostic',
        'binary_sha256':'a'*64, 'harness_sha256':'b'*64,
        'profile_identity':{'profile':'typed-speech-windows-diagnostic-v1',
                            'contract_sha256':voice_contract.contract_sha256()},
        'cleanup':{'app_exit_before_group_termination_observed':True, 'owned_job_retired':True},
        'passed':False, 'workflow_completed':True, 'overall_status':'unverified',
        'private_storage_acl_verified':True, 'live_calls':0,
        'external_shared_service_lifecycle':'unverified'}


class DesktopVoiceTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.checkout = self.root / 'repo'
        self.checkout.mkdir()
        self.tools = self.root / 'bundle/drivers'
        self.python = self.tools / 'venv/bin/python'
        self.python.parent.mkdir(parents=True)
        self.python.write_bytes(b'synthetic-interpreter-not-executed')
        self.common = SimpleNamespace(ROOT=self.checkout)
        self.argv = ['--platform', 'desktop', '--workload', 'typed-speech', '--desktop-platform', 'windows', '--windows-manifest', str(self.root/'manifest.json'), '--max-live-calls', '0',
            '--binary', str(self.root / 'synthetic-app'), '--tools', str(self.tools),
            '--config', str(self.root / 'synthetic.toml'),
            '--output', str(self.root / 'fresh-run')]

    def args(self, extra=()):
        return benchmark.parser().parse_args(self.argv + list(extra))

    def execute(self, args, platform='windows', value=None, code=None, error=None):
        launches = []
        def child(argv, **kwargs):
            launches.append((argv, kwargs))
            if error is not None:
                raise error
            output = Path(argv[argv.index('--output') + 1])
            self.assertFalse(output.exists())
            output.mkdir()
            (output / 'report.json').write_text(json.dumps(report(platform) if value is None else value))
            return SimpleNamespace(returncode=(1 if platform == 'windows' else 0) if code is None else code)
        stdout = io.StringIO()
        def popen(argv, **kwargs):
            result = child(argv, **kwargs)
            return SimpleNamespace(returncode=result.returncode, wait=lambda timeout: result.returncode)
        with patch.object(voice, 'native_platform', return_value=platform), \
                patch.object(voice, 'windows_python', return_value=str(self.python)), \
                patch.object(voice.subprocess, 'Popen', side_effect=popen), \
                patch.object(benchmark, 'preflight_output', side_effect=AssertionError('no Unix preflight')), \
                patch.object(benchmark, 'source_identity', side_effect=AssertionError('no Git requirement')), \
                redirect_stdout(stdout):
            result = voice.main(args, self.common)
        return result, json.loads(stdout.getvalue()), launches

    def typed_args(self, extra=()):
        return self.args(extra)

    def test_typed_speech_dispatch_and_diagnostic_interpretation(self):
        value = report('windows')
        value.update(entry='typed-speech-windows', live_calls=0,
                     external_shared_service_lifecycle='unverified')
        value['profile_identity']['profile'] = 'typed-speech-windows-diagnostic-v1'
        result, summary, launches = self.execute(self.typed_args(), 'windows', value)
        self.assertEqual(result, 2)
        self.assertFalse(summary['qualified_for_live'])
        argv = launches[0][0]
        self.assertEqual(argv[argv.index('--entry')+1], 'typed-speech')
        self.assertNotIn('--wake-phrase', argv)
        self.assertNotIn('--tts-model', argv)
        for update in ({'live_calls':1}, {'live_calls':True}, {'qualified_for_live':True},
                       {'external_shared_service_lifecycle':'retired'}, {'entry':'voice-arithmetic-windows'}):
            with self.assertRaises(benchmark.Blocked):
                voice.interpret(dict(value, **update), 1, 'windows', workload='typed-speech')

    def test_typed_speech_authorization_forbids_live_voice_assets_and_other_platforms(self):
        for extra in (['--live'], ['--wake-phrase', 'synthetic'],
                      ['--max-live-calls', '1'], ['--cases', 'arithmetic']):
            with patch.object(voice, 'native_platform', return_value='windows'), self.assertRaises(benchmark.Blocked):
                voice.authorize(self.typed_args(extra))
        with patch.object(voice, 'native_platform', return_value='linux'), self.assertRaises(benchmark.Blocked):
            voice.authorize(self.typed_args(['--desktop-platform', 'linux']))


    def test_windows_expected_diagnostic_exit_one_is_unverified_not_failure(self):
        args = self.args(['--desktop-platform', 'windows', '--windows-manifest', str(self.root / 'manifest.json')])
        result, summary, launches = self.execute(args, 'windows')
        self.assertEqual(result, 2)
        self.assertTrue(summary['workflow_completed'])
        self.assertFalse(summary['offline_profile_qualified'])
        self.assertFalse(summary['child_passed'])
        self.assertEqual(summary['child_exit_code'], 1)
        argv = launches[0][0]
        self.assertEqual(argv[argv.index('--tools') + 1], str(self.tools))
        self.assertIn('--windows-manifest', argv)
        self.assertIn('--desktop-platform', argv)

    def test_failed_typed_speech_child_keeps_validated_cleanup_and_bounded_failure_receipts(self):
        value = report()
        value['workflow_completed'] = False
        value['cleanup'].update(app_exit_observed=True, forced_group_termination=False, webdriver_quit_error=False)
        value['rows'] = [{'case':'arithmetic', 'passed':False, 'failure':'flow_deadline',
            'ui_state':{'failure_categories':['not configured', 'synthetic-private-payload'], 'runtime_status':'sleeping'},
            'service_metadata':{'generation_requests':0}, 'private':'synthetic-private-payload'}]
        result, summary, launches = self.execute(self.args(), value=value, code=1)
        self.assertEqual(result, 1)
        self.assertFalse(summary['workflow_completed'])
        self.assertEqual(summary['cleanup']['app_exit_before_group_termination_observed'], True)
        self.assertEqual(summary['cleanup']['app_exit_observed'], True)
        self.assertEqual(summary['cleanup']['forced_group_termination'], False)
        self.assertEqual(summary['child_failure_receipts'], [{'failure_category':'flow_deadline',
            'ui_failure_categories':['not_configured'], 'runtime_status':'sleeping', 'fake_generation_requests':0}])
        self.assertNotIn('synthetic-private-payload', json.dumps(summary))
        self.assertEqual(len(launches), 1)

    def test_failed_windows_workflow_keeps_cleanup_but_is_not_diagnostic_success(self):
        args = self.args(['--desktop-platform', 'windows', '--windows-manifest', str(self.root / 'manifest.json')])
        value = report('windows')
        value['workflow_completed'] = False
        result, summary, _ = self.execute(args, 'windows', value=value, code=1)
        self.assertEqual(result, 1)
        self.assertFalse(summary['workflow_completed'])
        self.assertEqual(summary['cleanup']['owned_job_retired'], True)

    def test_unexpected_exit_retains_only_self_reported_facts_not_root_completion(self):
        for platform in ('windows',):
            with self.subTest(platform=platform):
                evidence = {'workflow_completed':False}
                with self.assertRaises(benchmark.Blocked):
                    voice.interpret(report(platform), -9, platform, evidence=evidence)
                self.assertFalse(evidence['workflow_completed'])
                self.assertEqual(evidence['child_evidence'], 'validated-self-reported')
                self.assertTrue(evidence['cleanup']['app_exit_before_group_termination_observed'])
                self.assertNotIn('native_retirement_verified', evidence)

    def test_invalid_child_shape_never_publishes_trusted_cleanup_or_identities(self):
        variants = []
        for changes in ({'schema':True}, {'binary_sha256':'wrong'},
                        {'profile_identity':{'profile':'voice-arithmetic-v1', 'contract_sha256':'a' * 64}},
                        {'cleanup':{'app_exit_before_group_termination_observed':'true'}},
                        {'rows':[{'case':'arithmetic', 'service_metadata':{'generation_requests':False}}]},
                        {'rows':[{'case':'arithmetic', 'ui_state':{'failure_categories':'synthetic-private-payload'}}]}):
            value = report()
            value.update(changes)
            variants.append(value)
        for value in variants:
            with self.subTest(keys=sorted(value)), self.assertRaises(benchmark.Blocked):
                evidence = {}
                try:
                    voice.interpret(value, 1, 'windows', evidence=evidence)
                finally:
                    self.assertEqual(evidence, {})

    def test_missing_or_unknown_failure_receipts_do_not_infer_zero_or_export_content(self):
        value = report()
        value['workflow_completed'] = False
        value['rows'] = [{'case':'arithmetic', 'failure':'synthetic-private-payload',
            'ui_state':{'failure_categories':['synthetic-private-payload'], 'runtime_status':'synthetic-private-payload'}}]
        evidence = {}
        with self.assertRaises(benchmark.Blocked):
            voice.interpret(value, 1, 'windows', evidence=evidence)
        self.assertEqual(evidence['child_failure_receipts'], [])
        self.assertNotIn('synthetic-private-payload', json.dumps(evidence))


    def test_explicit_offline_budget_and_foreign_controls_fail_before_launch(self):
        extras = (['--max-live-calls', '7'], ['--live'], ['--repetitions', '2'], ['--model', 'synthetic-other'],
            ['--allow-device'], ['--device', 'a17'], ['--adb', '/synthetic/adb'], ['--ssh', 'synthetic'],
            ['--remote-adb', '/synthetic/adb'], ['--apk', '/synthetic/app.apk'], ['--helper-apk', '/synthetic/helper.apk'],
            ['--offline-evidence', '/synthetic/old.json'], ['--cases', 'arithmetic'], ['--wear-ui-probe'],
            ['--collect-stages'], ['--history-seed-turns', '1'], ['--port', '9223'], ['--turn-timeout', '180'],
            ['--wake-phrase', 'synthetic'])
        for extra in extras:
            with self.subTest(extra=extra), patch.object(voice, 'native_platform', return_value='windows'):
                with self.assertRaises(benchmark.Blocked):
                    voice.authorize(self.args(extra))
        default_budget = benchmark.parser().parse_args([a for a in self.argv if a not in ('--max-live-calls', '0')])
        with patch.object(voice, 'native_platform', return_value='windows'), self.assertRaises(benchmark.Blocked):
            voice.authorize(default_budget)

    def test_os_mismatch_no_implicit_wsl_to_windows_bridge(self):
        with patch.object(voice, 'native_platform', return_value='linux'):
            with self.assertRaises(benchmark.Blocked):
                voice.authorize(self.args(['--desktop-platform', 'windows', '--windows-manifest', '/synthetic/manifest']))
        with patch.object(voice.sys, 'platform', 'win32'):
            with self.assertRaises(benchmark.Blocked):
                voice.native_platform()

    def test_child_crossprofile_tamper_unknown_text_and_unexpected_exit_fail_closed(self):
        for changes in ({'schema':1}, {'entry':'voice'}, {'qualified_for_live':True},
                        {'passed':'synthetic-private-payload'}, {'offline_profile_qualified':True},
                        {'profile_identity':{'profile':'wrong', 'contract_sha256':'a' * 64}},
                        {'cleanup':{'app_exit_before_group_termination_observed':False}}):
            with self.subTest(changes=changes):
                value = report()
                value.update(changes)
                with self.assertRaises(benchmark.Blocked):
                    voice.interpret(value, 1, 'windows')
        with self.assertRaises(benchmark.Blocked):
            voice.interpret(report('windows'), 0, 'windows')
        with self.assertRaises(benchmark.Blocked):
            voice.interpret(report(), 1, 'linux')

    def test_timeout_is_failure_with_cleanup_unverified_and_no_retry(self):
        result, summary, launches = self.execute(self.args(), error=subprocess.TimeoutExpired('synthetic', 900))
        self.assertEqual(result, 1)
        self.assertFalse(summary['workflow_completed'])
        self.assertEqual(summary['cleanup'], 'unverified')
        self.assertEqual(len(launches), 1)

    def test_owned_child_supervision_bounds_terminate_and_kill_without_foreign_actions(self):
        actions = []
        class Child:
            returncode = None
            def wait(self, timeout):
                actions.append(('wait', timeout))
                raise subprocess.TimeoutExpired('synthetic', timeout)
            def terminate(self):
                actions.append(('terminate',))
            def kill(self):
                actions.append(('kill',))
        with patch.object(voice.subprocess, 'Popen', return_value=Child()) as create:
            with self.assertRaises(subprocess.TimeoutExpired):
                voice.launch(['synthetic-runner'], self.checkout)
        self.assertEqual(actions, [('wait', voice.HOST_OBSERVATION_SECONDS), ('terminate',),
            ('wait', 5), ('kill',), ('wait', 5)])
        self.assertNotIn('start_new_session', create.call_args.kwargs)

    def test_bounded_duplicate_symlink_and_missing_child_report(self):
        output = self.root / 'report-owner'
        output.mkdir()
        path = output / 'report.json'
        for raw in (b'{}' * (128 * 1024), b'{"schema":3,"schema":4}', b'not JSON'):
            path.write_bytes(raw)
            with self.assertRaises(benchmark.Blocked):
                voice.child_report(output)
        path.unlink()
        with self.assertRaises(benchmark.Blocked):
            voice.child_report(output)
        foreign = self.root / 'foreign.json'
        foreign.write_text('{}')
        path.symlink_to(foreign)
        with self.assertRaises(benchmark.Blocked):
            voice.child_report(output)

    def test_output_must_be_fresh_outside_checkout_and_not_precreated(self):
        for output in (self.checkout / 'inside', self.root):
            args = self.args(['--output', str(output)])
            with self.assertRaises(benchmark.Blocked):
                voice.fresh_output(args, self.common)
        output = voice.fresh_output(self.args(), self.common)
        self.assertFalse(output.exists())

    def test_new_options_rejected_for_typed_and_phone_wear(self):
        import wear_voice
        for option, value in (('--desktop-platform', 'linux'),
                              ('--wake-phrase', 'synthetic'), ('--windows-manifest', '/synthetic/manifest')):
            args = benchmark.parser().parse_args(['--platform', 'desktop', option, value])
            with self.assertRaises(benchmark.Blocked):
                benchmark.authorize(args)
            with self.assertRaises(benchmark.Blocked):
                wear_voice.authorize_common(args)

    def test_windows_effects_failure_report_producer_consumer_integration(self):
        import windows_effects
        args = self.args(['--desktop-platform', 'windows', '--windows-manifest', str(self.root / 'manifest.json')])
        args._test_win32 = True
        args.entry = 'typed-speech'
        args.cases = ['arithmetic']
        manifest_path = self.root / 'manifest.json'
        manifest_path.write_text('{"native_source_sha256": "' + ('c' * 64) + '", "driver_versions": {}}')
        args.windows_manifest = manifest_path
        binary_path = self.root / 'binary.exe'
        binary_path.write_bytes(b'binary')
        tauri_driver_path = self.root / 'tauri_driver.exe'
        tauri_driver_path.write_bytes(b'tauri_driver')
        native_driver_path = self.root / 'native_driver.exe'
        native_driver_path.write_bytes(b'native_driver')

        effects = windows_effects.WindowsEffects(args)
        effects.roles = {'binary': binary_path, 'tauri_driver': tauri_driver_path, 'native_driver': native_driver_path}
        effects.manifest = json.loads(manifest_path.read_text())
        effects.profile_identity = {'profile': 'typed-speech-windows-diagnostic-v1', 'contract_sha256': voice_contract.contract_sha256()}
        args.output.mkdir(mode=0o700)
        effects.run_owned = True
        effects.private_storage_verified = True
        effects.stage = 'windows_app_launch'
        effects.last_cleanup = {'owned_job_retired': True, 'app_exit_before_group_termination_observed': True}

        class MockApi:
            __file__ = __file__
            @staticmethod
            def private_json(path, data):
                path.write_text(json.dumps(data))
            @staticmethod
            def prune_owned_reports(parent):
                pass

        class SessionNotCreatedException(Exception):
            def __init__(self):
                super().__init__('session not created: timed out waiting for webview2 to connect')
                self.msg = 'session not created: timed out waiting for webview2 to connect'
                self.native_error_code = 1234

        error = SessionNotCreatedException()
        effects.failure_report(MockApi(), 'windows_app_launch', error)

        report_data = voice.child_report(args.output)
        evidence = {}
        with self.assertRaises(benchmark.Blocked) as ctx:
            voice.interpret(report_data, 1, 'windows', evidence=evidence)
        self.assertEqual(str(ctx.exception), 'desktop_voice_windows_workflow_unverified')
        self.assertEqual(evidence.get('session_creation_cause'), 'webview_attach_timeout')
        self.assertEqual(evidence.get('stage'), 'windows_app_launch')
        self.assertEqual(evidence.get('platform_stage'), 'windows_app_launch')
        self.assertEqual(evidence.get('failure_category'), 'SessionNotCreatedException')
        self.assertEqual(evidence.get('child_failure_category'), 'SessionNotCreatedException')
        self.assertEqual(evidence.get('native_error_code'), 1234)
        self.assertEqual(evidence.get('cleanup', {}).get('owned_job_retired'), True)
        self.assertIn('binary_sha256', evidence.get('child_identities', {}))
