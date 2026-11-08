import ctypes
import hashlib
import json
import os
from pathlib import Path
import tempfile
import struct
from types import SimpleNamespace
import sys
import types
import unittest
from unittest.mock import patch

import runner
from fixtures import arithmetic_profile
import windows_effects as windows


class WindowsEffectsTests(unittest.TestCase):
    def test_webview_capability_binds_the_same_private_folder_as_native_storage_guard(self):
        with tempfile.TemporaryDirectory() as directory:
            effects = windows.WindowsEffects.__new__(windows.WindowsEffects)
            effects.roles = {'binary': Path(directory)/'synthetic.exe'}
            effects.args = SimpleNamespace(output=Path(directory)/'private-run')
            self.assertEqual(effects.application_options(), {
                'application': str(effects.roles['binary'].resolve()),
                'webviewOptions': {'userDataFolder': str((effects.args.output/'webview').resolve())}})

    def test_session_creation_cause_is_conservative_and_never_exports_message(self):
        class SessionNotCreatedException(Exception):
            def __init__(self, message):
                self.msg = message
        cases = [
            ('session not created: This version of Microsoft Edge WebDriver only supports Microsoft Edge version 155', 'version_mismatch'),
            ('session not created: invalid argument: unrecognized capability: synthetic:option', 'unsupported_capability'),
            ('session not created: Microsoft Edge failed to start: exited abnormally.', 'application_launch_failed'),
            ('session not created: timed out waiting for WebView2 to connect', 'webview_attach_timeout'),
            ('session not created: timeout failed syntheticsecret', 'unknown'),
            ('session not created: unsupported syntheticsecret', 'unknown'),
        ]
        suffix = '\nsyntheticsecret C:\\synthetic\\private.exe https://synthetic.invalid/private'
        for message, expected in cases:
            with self.subTest(expected=expected):
                cause = windows.session_creation_cause(SessionNotCreatedException(message + suffix))
                self.assertEqual(cause, expected)
                self.assertNotIn('syntheticsecret', cause)
        ambiguous = cases[0][0] + '\n' + cases[2][0]
        self.assertEqual(windows.session_creation_cause(SessionNotCreatedException(ambiguous)), 'unknown')
        self.assertEqual(windows.session_creation_cause(SessionNotCreatedException(cases[0][0] + 'x' * 4096)), 'unknown')
        self.assertEqual(windows.session_creation_cause(SessionNotCreatedException(None)), 'unknown')
        self.assertEqual(windows.session_creation_cause(SessionNotCreatedException('a timeout while attaching failed')), 'unknown')
        self.assertEqual(windows.session_creation_cause(ValueError(cases[0][0])), 'unknown')

    def test_session_creation_cause_is_persisted_without_payload_or_cleanup_changes(self):
        class SessionNotCreatedException(Exception):
            msg = 'session not created: invalid argument: unrecognized capability: syntheticsecret'
        with tempfile.TemporaryDirectory() as directory:
            effects = windows.WindowsEffects.__new__(windows.WindowsEffects)
            effects.args = SimpleNamespace(output=Path(directory))
            effects.run_owned = True
            effects.stage = 'windows_suspended_driver_assignment'
            effects.last_cleanup = {'owned_job_retired': True, 'app_exit_observed': False}
            api = SimpleNamespace(private_json=lambda path, value: path.write_text(json.dumps(value)),
                                  prune_owned_reports=lambda parent: None)
            effects.failure_report(api, 'app_launch', SessionNotCreatedException())
            report = json.loads((Path(directory)/'report.json').read_text())
            self.assertEqual(report['session_creation_cause'], 'unsupported_capability')
            self.assertEqual(report['failure_category'], 'SessionNotCreatedException')
            self.assertEqual(report['cleanup'], effects.last_cleanup)
            self.assertNotIn('syntheticsecret', json.dumps(report))

    def test_shared_runner_uses_windows_effects_not_linux_launch_or_fixture_synthesis(self):
        class Options:
            def __init__(self):
                self._caps = dict(self.default_capabilities)
            def set_capability(self, key, value):
                self._caps[key] = value
        observed = {}
        driver = SimpleNamespace(set_script_timeout=lambda value: None,
            execute_script=lambda *args: 'custom-luna-none',
            execute_async_script=lambda script, command, *args: {'ok': True, 'value': (
                {'preferences': {'instant': 'custom-luna-none'}, 'model_options': [
                    {'id': 'custom-luna-none', 'available': False}]} if command == 'get_app_snapshot' else
                {'process_id': 1234, 'capture_active': True, 'phase': 'sleeping'})})
        def remote(**kwargs):
            observed.update(kwargs['options'].to_capabilities())
            return driver
        selenium = types.ModuleType('selenium')
        selenium.webdriver = SimpleNamespace(Remote=remote)
        modules = {'selenium': selenium, 'selenium.webdriver': selenium.webdriver,
                   'selenium.webdriver.common': types.ModuleType('selenium.webdriver.common'),
                   'selenium.webdriver.common.options': SimpleNamespace(BaseOptions=Options)}
        with tempfile.TemporaryDirectory() as temporary:
            run = Path(temporary)
            (run/'config').mkdir()
            (run/'config/config.toml').write_text('synthetic = true')
            (run/'synthetic.exe').write_bytes(b'synthetic')
            args = SimpleNamespace(entry='voice-arithmetic', live=False, cases=['arithmetic'],
                tts_model=run/'tts.onnx', binary=run/'synthetic.exe', output=run,
                driver_free_diagnostic=False, hold_native_observer=False, event_loop_diagnostic=False,
                wake_diagnostics=False, poll_interval=0.1, flow_timeout=10)
            effects = windows.WindowsEffects.__new__(windows.WindowsEffects)
            effects.roles = {'binary': args.binary}
            effects.owner = windows.WindowsJob.__new__(windows.WindowsJob)
            effects.owner.job, effects.owner.app = 1, None
            effects.owner.api = SimpleNamespace(
                open_process=lambda pid: observed.update(opened_pid=pid) or 10,
                in_job=lambda *args: True,
                image_and_created=lambda handle: (args.binary.resolve(), 123456),
                wait=lambda *args: False, close=lambda handle: None)
            platform = SimpleNamespace(
                prepare=lambda *args: ({'arithmetic': {'byte_count': 4}}, {}, {}),
                environment=lambda *args: {}, start=lambda *args: object(),
                observe_app=effects.observe_app,
                application_options=lambda: {'application': 'synthetic-owned.exe'},
                shutdown=lambda *args: {'app_exit_before_group_termination_observed': True},
                annotate=lambda summary: summary.update(diagnostic_only=True, offline_profile_qualified=False))
            service = SimpleNamespace(generation_endpoint='http://127.0.0.1:1/responses',
                mcp_endpoint='http://127.0.0.1:1/mcp', __enter__=lambda: None,
                __exit__=lambda *args: None)
            with patch.dict(sys.modules, modules), \
                    patch.object(runner, 'validate_benchmark_binary'), \
                    patch.object(runner, 'generate_fixtures', side_effect=AssertionError('Linux synthesis')), \
                    patch.object(runner.subprocess, 'Popen', side_effect=AssertionError('Linux launch')), \
                    patch.object(runner, 'can_connect', return_value=True), \
                    patch.object(runner, 'FakeServices', return_value=service), \
                    patch.object(runner, 'APP_PID', None), \
                    patch.object(runner, 'exercise', return_value={'passed': True, 'tts_actual_provider': 'cpu'}) as exercise, \
                    patch.object(runner, 'private_json') as reports, patch.object(runner, 'prune_owned_reports'), \
                    patch('builtins.print'):
                self.assertEqual(runner.execute(args, platform), 1)
                exercise.assert_not_called()
                self.assertNotIn('process_id', json.dumps(reports.call_args.args[1]))
            self.assertEqual(observed['tauri:options'], {'application': 'synthetic-owned.exe'})
            self.assertEqual(observed['browserName'], 'wry')
            self.assertEqual(observed['opened_pid'], 1234)
            self.assertTrue(effects.app_observed)

    def staged_bundle(self, root):
        roles = dict(binary='app/vox-golem.exe', tauri_driver='drivers/tauri-driver.exe',
            native_driver='drivers/msedgedriver.exe', config='speech.toml',
            fixture='fixtures/arithmetic.pcm', wake_word_model_path='assets/wake.onnx',
            silero_vad_model='assets/vad.onnx', parakeet_model_dir='assets/asr',
            tts_model='assets/tts.onnx', espeak_data_dir='assets/espeak-ng-data')
        for name in ('app', 'drivers', 'fixtures', 'assets', 'assets/asr', 'assets/espeak-ng-data'):
            (root/name).mkdir(exist_ok=True)
        pe = bytearray(128)
        pe[:2] = b'MZ'
        struct.pack_into('<I', pe, 60, 64)
        pe[64:68] = b'PE\0\0'
        struct.pack_into('<H', pe, 68, 0x8664)
        struct.pack_into('<H', pe, 88, 0x20b)
        for role in ('binary', 'tauri_driver', 'native_driver'):
            (root/roles[role]).write_bytes(pe+runner.CAPABILITY)
        for name in ('assets/wake.onnx', 'assets/vad.onnx', 'assets/asr/synthetic.onnx',
                     'assets/tts.onnx', 'assets/espeak-ng-data/synthetic-data'):
            (root/name).write_bytes(b'synthetic')
        (root/'assets/tts.onnx.json').write_text('{}')
        (root/'speech.toml').write_text('\n'.join(f'{key}={json.dumps(roles[key])}' for key in
            ('wake_word_model_path', 'silero_vad_model', 'parakeet_model_dir')))
        pcm = root/roles['fixture']
        pcm.write_bytes(struct.pack('<4f', 0, 0.1, 0.2, 0))
        metadata = dict(byte_count=16, sample_count=4, duration_seconds=4/16000,
            wake_end_sample=1, question_start_sample=2, question_end_sample=4,
            sha256=windows.file_hash(pcm))
        value = dict(schema=1, contract_sha256=arithmetic_profile().contract_sha256,
            application_id='dev.gitblame.voxgolem.benchmark', native_source_sha256='a'*64,
            driver_versions=dict(tauri='2.1.0', edge='155.0.4283.62', webview='155.0.4283.45'),
            roles=roles, fixture_metadata={'arithmetic': metadata},
            files={file.relative_to(root).as_posix(): windows.file_hash(file)
                   for file in root.rglob('*') if file.is_file()})
        path = root/'manifest.json'
        path.write_text(json.dumps(value))
        return path, value

    def test_wrong_host_and_live_never_launch(self):
        for host, live in [('linux', False), ('win32', True)]:
            args = SimpleNamespace(live=live, entry='voice-arithmetic')
            with self.assertRaises(ValueError):
                windows.validate_options(args, host)

    def test_windows_profile_annotation_keeps_shared_arithmetic_implementation_identity(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest, value = self.staged_bundle(root)
            roles = value['roles']
            effects = windows.WindowsEffects.__new__(windows.WindowsEffects)
            effects.args = SimpleNamespace(windows_manifest=manifest, binary=root/roles['binary'],
                config=root/roles['config'], tts_model=root/roles['tts_model'],
                tools=(root/roles['tauri_driver']).parent, output=root/'run')
            effects.private_storage_verified = effects.app_observed = False
            profile = arithmetic_profile()
            with patch.object(windows, 'private_directory', side_effect=lambda path: path.mkdir()), \
                    patch.object(windows, 'verify_native_driver_version', return_value='155.0.4283.62'):
                _, _, identity = effects.prepare(profile, SimpleNamespace(**vars(runner)))
            self.assertEqual(identity['implementation_sha256'], profile.implementation_sha256)
            summary = {'profile_identity': identity, 'rows': [], 'cleanup': {}}
            effects.annotate(summary)
            self.assertEqual(summary['profile_identity']['implementation_sha256'], profile.implementation_sha256)
            self.assertEqual(summary['native_source_sha256'], value['native_source_sha256'])
            effects.last_cleanup = {'owned_job_retired': True}
            effects.failure_report(SimpleNamespace(**vars(runner)), 'app_launch', ValueError())
            failure = json.loads((effects.args.output/'report.json').read_text())
            self.assertEqual(failure['profile_identity']['implementation_sha256'], profile.implementation_sha256)

    def test_paths_reject_escape_and_foreign_roots(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name in ('../outside', '/absolute', 'C:/foreign', 'a\\b', 'a/../b'):
                with self.assertRaises(ValueError):
                    windows.bundle_path(root, name)

    def test_suspended_process_assignment_precedes_resume(self):
        calls = []
        api = SimpleNamespace(
            create_suspended=lambda *args: (calls.append('create') or (11, 12)),
            assign=lambda *args: calls.append('assign'),
            resume=lambda *args: calls.append('resume'),
            close=lambda *args: calls.append('close'),
            terminate_process=lambda *args: calls.append('terminate'))
        windows.start_assigned(api, 1, ['synthetic.exe'], {}, Path('.'))
        self.assertEqual(calls, ['create', 'assign', 'resume', 'close'])

    def test_assignment_failure_never_resumes_unowned_child(self):
        calls = []
        def fail(*args):
            raise OSError('synthetic_failure')
        api = SimpleNamespace(create_suspended=lambda *args: (11, 12), assign=fail,
            resume=lambda *args: calls.append('resume'), close=lambda *args: calls.append('close'),
            terminate_process=lambda *args: calls.append('terminate'))
        with self.assertRaises(OSError):
            windows.start_assigned(api, 1, ['synthetic.exe'], {}, Path('.'))
        self.assertEqual(calls, ['terminate', 'close', 'close'])

    def test_foreign_app_handle_is_rejected_without_termination(self):
        calls = []
        api = SimpleNamespace(open_process=lambda pid: 10, in_job=lambda *args: False,
                             close=lambda *args: calls.append('close'))
        owner = windows.WindowsJob.__new__(windows.WindowsJob)
        owner.api, owner.job, owner.app = api, 1, None
        with self.assertRaises(ValueError):
            owner.observe_app(99, Path('synthetic.exe'))
        self.assertEqual(calls, ['close'])

    def test_unowned_app_never_receives_webdriver_quit(self):
        calls = []
        driver = SimpleNamespace(quit=lambda: calls.append('unsafe-quit'))
        owner = windows.WindowsJob.__new__(windows.WindowsJob)
        owner.api = SimpleNamespace(active=lambda job: 0, close=lambda handle: None)
        owner.app, owner.driver, owner.job, owner.mutex = None, 2, 3, 4
        result = owner.shutdown(driver)
        self.assertEqual(calls, [])
        self.assertTrue(result['webdriver_quit_skipped_unowned_app'])
        self.assertFalse(result['app_exit_before_group_termination_observed'])

    def test_handshake_reuses_tauri_application_capability(self):
        self.assertEqual(windows.application_capability(Path('synthetic.exe')),
                         {'application': str(Path('synthetic.exe').resolve())})

    def test_staged_asset_hashes_and_pcm_metadata_are_verified(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, value = self.staged_bundle(root)
            loaded, roles = windows.load_manifest(path, arithmetic_profile())
            self.assertEqual(loaded['fixture_metadata'], value['fixture_metadata'])
            (root/'assets/tts.onnx.json').write_text('{"changed":true}')
            with self.assertRaises(ValueError):
                windows.load_manifest(path, arithmetic_profile())

    def test_declared_espeak_assets_reach_shipping_strict_private_appdata_layout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path, manifest = self.staged_bundle(root)
            args = SimpleNamespace(windows_manifest=manifest_path, output=root/'run',
                binary=root/manifest['roles']['binary'], config=root/manifest['roles']['config'],
                tts_model=root/manifest['roles']['tts_model'], tools=root/'drivers')
            effects = windows.WindowsEffects.__new__(windows.WindowsEffects)
            effects.args = args
            def owned_directory(path, create=True):
                if create:
                    path.mkdir()
            with patch.object(windows, 'private_directory', side_effect=owned_directory), \
                    patch.object(windows, 'verify_native_driver_version', return_value='155.0.4283.62'), \
                    patch.dict(os.environ, {'SystemRoot': str(root/'synthetic-system'),
                        'APPDATA': str(root/'forbidden-host-appdata')}, clear=True):
                _, _, identity = effects.prepare(arithmetic_profile(), runner)
                env = effects.environment('http://127.0.0.1:32123/responses', runner)
            # This is the shipping Rust resolver's exact directory contract, not
            # the former staged-model-adjacent override that Windows replaces.
            strict_root = Path(env['APPDATA'])/'VoxGolem/models/tts'
            copied = strict_root/'espeak-ng-data/synthetic-data'
            self.assertTrue(copied.is_file())
            self.assertEqual(windows.file_hash(copied), manifest['files']['assets/espeak-ng-data/synthetic-data'])
            self.assertEqual(Path(env['PIPER_ESPEAKNG_DATA_DIRECTORY']), strict_root)
            self.assertTrue(strict_root.is_relative_to(args.output))
            self.assertIn('espeak_copy_sha256', identity)
            expected = {'synthetic-data': manifest['files']['assets/espeak-ng-data/synthetic-data']}
            self.assertEqual(identity['espeak_copy_sha256'], hashlib.sha256(json.dumps(
                expected, sort_keys=True, separators=(',', ':')).encode()).hexdigest())
            self.assertFalse((root/'forbidden-host-appdata').exists())

    def test_private_espeak_copy_tampering_blocks_before_launch(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            _, manifest = self.staged_bundle(root)
            run = root/'run'
            (run/'appdata').mkdir(parents=True)
            original = windows.shutil.copyfile
            def corrupt(source, destination):
                original(source, destination)
                Path(destination).write_bytes(b'synthetic-corrupted-copy')
            with patch.object(windows.shutil, 'copyfile', side_effect=corrupt):
                with self.assertRaisesRegex(ValueError, 'does not match declared identity'):
                    windows.stage_private_espeak(run, root/'assets/espeak-ng-data', manifest)

    def test_missing_retained_pid_never_opens_a_process(self):
        effects = windows.WindowsEffects.__new__(windows.WindowsEffects)
        effects.owner = SimpleNamespace(observe_app=lambda *args: self.fail('Must not open an unknown process'))
        effects.roles = {'binary': Path('synthetic.exe')}
        for pid in (None, True, 0, -1, {'process_id': 1234}):
            with self.assertRaises(ValueError):
                effects.observe_app(pid)

    def test_unlisted_dll_and_raw_fixture_metadata_block_admission(self):
        for change in ('dll', 'metadata', 'version', 'config'):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary)
                path, value = self.staged_bundle(root)
                if change == 'dll':
                    (root/'app/foreign.dll').write_bytes(b'synthetic')
                elif change == 'metadata':
                    value['fixture_metadata']['arithmetic']['raw_text'] = 'synthetic-forbidden-field'
                elif change == 'version':
                    value['driver_versions']['edge'] = '154.0.4283.62'
                else:
                    (root/'speech.toml').write_text('secret = "synthetic-forbidden"')
                    value['files']['speech.toml'] = windows.file_hash(root/'speech.toml')
                path.write_text(json.dumps(value))
                with self.assertRaises(ValueError):
                    windows.load_manifest(path, arithmetic_profile())

    def test_diagnostic_never_becomes_linux_or_live_qualification(self):
        effects = windows.WindowsEffects.__new__(windows.WindowsEffects)
        effects.private_storage_verified = effects.app_observed = True
        with tempfile.TemporaryDirectory() as temporary:
            path, effects.manifest = self.staged_bundle(Path(temporary))
            effects.args = SimpleNamespace(windows_manifest=path)
            effects.roles = {key: path.parent/value for key, value in effects.manifest['roles'].items()}
            report = dict(rows=[{'passed': True}], cleanup=dict(owned_job_retired=True,
                app_exit_before_group_termination_observed=True))
            effects.annotate(report)
            self.assertTrue(report['workflow_completed'])
            self.assertTrue(report['diagnostic_only'])
            self.assertFalse(report['passed'])
            self.assertFalse(report['offline_profile_qualified'])
            self.assertFalse(report['qualified_for_live'])
            self.assertEqual(report['overall_status'], 'unverified')
            self.assertEqual(report['os_network_containment'], 'unverified')
            evidence = path.parent/'report.json'
            evidence.write_text(json.dumps(report))
            for entry in ('voice', 'typed', 'voice-arithmetic', 'voice-arithmetic-windows'):
                with self.assertRaises(ValueError):
                    runner.validate_offline_evidence(evidence, effects.roles['binary'], {}, {}, entry)

    def test_windows_cannot_open_unix_live_ledger(self):
        with patch.object(runner, 'fcntl', None):
            with self.assertRaises(ValueError):
                runner.reserve_live_budget(Path('synthetic-ledger'), Path('synthetic-reservation'))

    def test_windows_environment_uses_private_roots_without_host_credentials_or_loader_overrides(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, manifest = self.staged_bundle(root)
            effects = windows.WindowsEffects.__new__(windows.WindowsEffects)
            effects.args = SimpleNamespace(output=root/'run')
            effects.args.output.mkdir()
            effects.roles = {key: root/name for key, name in manifest['roles'].items()}
            with patch.dict(os.environ, {'SystemRoot': str(root/'synthetic-system'),
                'OPENAI_API_KEY': 'synthetic-must-not-inherit', 'ORT_DYLIB_PATH': 'synthetic-foreign',
                'WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS': '--synthetic-foreign-debugging'}, clear=True), \
                    patch.object(windows, 'private_directory'):
                env = effects.environment('http://127.0.0.1:32123/responses', runner)
            for key in ('APPDATA', 'LOCALAPPDATA', 'USERPROFILE', 'TEMP', 'TMP', 'WEBVIEW2_USER_DATA_FOLDER'):
                self.assertTrue(Path(env[key]).is_relative_to(root/'run'))
            self.assertNotIn('OPENAI_API_KEY', env)
            self.assertNotIn('ORT_DYLIB_PATH', env)
            self.assertNotIn('WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS', env)
            self.assertNotIn('ALSA_CONFIG_PATH', env)
            self.assertEqual(env['VOXGOLEM_BENCH_ENTRY'], 'voice-arithmetic-windows')

    def test_x64_windows_sdk_structure_layouts(self):
        if ctypes.sizeof(ctypes.c_void_p) != 8:
            self.skipTest('x64 ABI only')
        self.assertEqual(ctypes.sizeof(windows.BasicLimits), 64)
        self.assertEqual(ctypes.sizeof(windows.ExtendedLimits), 144)
        self.assertEqual(ctypes.sizeof(windows.StartupInfo), 104)
        self.assertEqual(ctypes.sizeof(windows.ProcessInfo), 24)
        self.assertEqual(ctypes.sizeof(windows.Accounting), 48)

    def test_acl_failure_does_not_create_run_contents(self):
        with tempfile.TemporaryDirectory() as temporary:
            target = Path(temporary)/'run'
            with patch.object(windows, 'powershell', return_value='not-private'):
                with self.assertRaises(ValueError):
                    windows.private_directory(target)
            self.assertFalse(target.exists())

    def test_actual_edge_file_version_must_match_manifest(self):
        with patch.object(windows, 'powershell', return_value='155.0.4283.62'):
            self.assertEqual(windows.verify_native_driver_version(Path('synthetic.exe'), '155.0.4283.62'),
                             '155.0.4283.62')
            with self.assertRaises(ValueError):
                windows.verify_native_driver_version(Path('synthetic.exe'), '154.0.4283.62')

    def test_cleanup_failure_closes_job_and_preserves_unverified_retirement(self):
        closed = []
        def fail(*args):
            raise OSError('synthetic tracking failure')
        api = SimpleNamespace(wait=fail, close=lambda handle: closed.append(handle))
        owner = windows.WindowsJob.__new__(windows.WindowsJob)
        owner.api, owner.app, owner.driver, owner.job, owner.mutex = api, 1, 2, 3, 4
        result = owner.shutdown(None)
        self.assertEqual(closed, [1, 2, 3, 4])
        self.assertFalse(result['owned_job_retired'])
        self.assertTrue(result['ownership_tracking_failed'])


if __name__ == '__main__':
    unittest.main()
