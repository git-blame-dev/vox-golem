"""Offline tests for the Wear transport; never contact a device."""
import importlib.util
from pathlib import Path
import unittest
import tempfile
import hashlib
from types import SimpleNamespace
import benchmark as common
from unittest.mock import patch, AsyncMock
import subprocess
import json
import os
import time

SPEC = importlib.util.spec_from_file_location('wear_transport_test', Path(__file__).with_name('wear.py'))
wear = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(wear)


class IdentityTests(unittest.TestCase):
    def test_only_one_advertised_watch_is_selected(self):
        listing = 'phone device model:Pixel_8\nwatch device model:SM_L315F\n'
        self.assertEqual(wear.select_watch(listing), 'watch')
        for value in ('phone device model:Pixel_8', 'watch offline model:SM_L315F',
                      listing + 'another device model:SM_L315F\n'):
            with self.subTest(value=value), self.assertRaises(wear.Blocked):
                wear.select_watch(value)

    def test_response_must_match_command_and_bound_payload(self):
        self.assertEqual(wear.decode_response('{"sequence":1,"status":"ready"}', 1), {'sequence':1, 'status':'ready'})
        for value in ('{"sequence":2,"status":"ready"}',
                      '{"sequence":1,"status":"ok"}',
                      '{"sequence":1,"status":"error","error":"secret payload"}',
                      '{"sequence":1,"sequence":1,"status":"ready"}',
                      'x' * (wear.MAX_RESPONSE_BYTES + 1)):
            with self.subTest(value=value[:100]), self.assertRaises(wear.Blocked) as raised:
                wear.decode_response(value, 1)
            self.assertNotIn('secret payload', str(raised.exception))

    def test_native_failure_only_exposes_fixed_stage_not_payload(self):
        for phase, expected in [('prepare_draft', 'wear_native_prepare_draft_failed'),
                                ('secret payload', 'wear_command_response_invalid_or_failed')]:
            value = json.dumps({'sequence':1, 'status':'failed', 'phase':phase, 'failure_class':'secret payload'})
            with self.subTest(phase=phase), self.assertRaisesRegex(wear.Blocked, expected) as raised:
                wear.decode_response(value, 1)
            self.assertNotIn('secret payload', str(raised.exception))

    def test_readiness_diagnostics_are_allowlisted_and_bounded(self):
        value = json.dumps({'sequence':1, 'status':'failed', 'phase':'send_ready', 'readiness_diagnostics':{
            'send_visible':False, 'latest_acked':True, 'primary_action':'SEND', 'branch_count':0,
            'raw_text':'secret payload', 'send_enabled':'secret payload'}})
        with self.assertRaises(wear.Blocked) as raised:
            wear.decode_response(value, 1)
        self.assertEqual(raised.exception.native_diagnostics,
            {'send_visible':False, 'latest_acked':True, 'primary_action':'SEND', 'branch_count':0})

    def test_installed_optimized_artifact_must_match(self):
        digest = hashlib.sha256(b'synthetic apk').hexdigest()
        class ADB:
            def __init__(self):
                self.model, self.api, self.abi = 'SM-L315F', '36', 'armeabi-v7a'
                self.flags = 'HAS_CODE ALLOW_CLEAR_USER_DATA'
                self.digest = digest
            def run(self, *args):
                if args[1:3] == ('getprop', 'ro.product.model'): return self.model
                if args[1:3] == ('getprop', 'ro.build.version.sdk'): return self.api
                if args[1:3] == ('getprop', 'ro.product.cpu.abi'): return self.abi
                if args[1:4] == ('pm', 'list', 'features'): return 'feature:android.hardware.type.watch'
                if args[1:3] == ('pm', 'path'): return 'package:/data/app/synthetic/base.apk'
                if args[1] == 'sha256sum': return self.digest + '  /data/app/synthetic/base.apk'
                if args[1:3] == ('dumpsys', 'package'): return f'versionCode=1\npkgFlags=[ {self.flags} ]'
                raise AssertionError(args)
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / 'synthetic.apk'
            apk.write_bytes(b'synthetic apk')
            args = SimpleNamespace(apk=apk)
            adb = ADB()
            self.assertFalse(wear.build_identity(adb, args)['debuggable'])
            for field, value in [('model', 'Pixel 8'), ('api', '37'), ('abi', 'arm64-v8a'),
                                 ('flags', 'DEBUGGABLE'), ('digest', '0' * 64)]:
                original = getattr(adb, field)
                setattr(adb, field, value)
                with self.subTest(field=field), self.assertRaises(wear.Blocked):
                    wear.build_identity(adb, args)
                setattr(adb, field, original)


class NativeDriverTests(unittest.IsolatedAsyncioTestCase):
    def mailbox(self, mismatch=False, uncertain=False):
        class FakeMailbox:
            model = 'custom-luna-none'
            cleaned = False
            def __init__(self):
                self.commands = []
                self.turn = 0
            async def command(self, operation, **args):
                self.commands.append((operation, args))
                if operation == 'setup_case':
                    return {'status':'configured', 'tts':False, 'configuration':'isolated_policy_not_ui'}
                if operation == 'status':
                    return {'status':'ready', 'busy':uncertain and self.turn > 0 and not self.cleaned, 'failed':False}
                if operation == 'submit':
                    self.turn += 1
                    if uncertain:
                        raise common.Blocked('synthetic_transport_lost')
                    prompt = args['prompt']
                    return {'status':'complete', 'submitted':True, 'execution_valid':True,
                        'request':f'turn-{self.turn}', 'first_answer_ms':10, 'completed_ms':20,
                        'canonical_prompt':prompt, 'projected_prompt':prompt,
                        'canonical_answer':'210', 'projected_answer':'wrong' if mismatch else '210',
                        'projection_equal':not mismatch, 'ui_visible_equal':True, 'ui_visible_status':'pass'}
                raise AssertionError(operation)
            async def close(self):
                self.cleaned = True
        return FakeMailbox()

    def args(self):
        return common.parser().parse_args(['--platform', 'wear', '--cases', 'arithmetic'])

    async def test_shared_suite_preserves_native_projection_and_ui_distinction(self):
        mailbox = self.mailbox()
        report = {'cases':[], 'cleanup':{}, 'status':'fail'}
        driver = wear.NativeSuiteDriver(mailbox, common, report)
        await common.run_suite(None, self.args(), report, driver=driver)
        self.assertEqual(report['admissions']['submitted'], 1)
        self.assertEqual(report['admissions']['observed'], 1)
        self.assertTrue(report['cases'][0]['workflow_completed'])
        self.assertEqual(report['cases'][0]['checks']['canonical_projection_equal'], 'pass')
        self.assertEqual(report['cases'][0]['checks']['single_rendered_ui_row'], 'unverified')
        self.assertEqual(report['status'], 'unverified')
        self.assertTrue(mailbox.cleaned)

    async def test_projection_mismatch_fails_and_retires_private_session(self):
        mailbox = self.mailbox(mismatch=True)
        report = {'cases':[], 'cleanup':{}, 'status':'fail'}
        driver = wear.NativeSuiteDriver(mailbox, common, report)
        with self.assertRaisesRegex(common.Blocked, 'scenario_validation_failed'):
            await common.run_suite(None, self.args(), report, driver=driver)
        self.assertEqual(report['cases'][0]['checks']['canonical_projection_equal'], 'fail')
        self.assertTrue(mailbox.cleaned)
        self.assertEqual(mailbox.turn, 1)

    async def test_uncertain_submission_is_never_retried_and_requires_retirement(self):
        mailbox = self.mailbox(uncertain=True)
        report = {'cases':[], 'cleanup':{}, 'status':'fail'}
        driver = wear.NativeSuiteDriver(mailbox, common, report)
        with self.assertRaisesRegex(common.Blocked, 'synthetic_transport_lost'):
            await common.run_suite(None, self.args(), report, driver=driver)
        self.assertTrue(mailbox.cleaned)
        self.assertEqual(mailbox.turn, 1)
        self.assertEqual(report['admissions']['submitted'], 1)
        self.assertEqual(report['admission_observation'], 'uncertain_after_submit_failure')


class MailboxLifecycleTests(unittest.IsolatedAsyncioTestCase):
    class Process:
        def __init__(self, exited=False, timeout=False):
            self.returncode = 0 if exited else None
            self.timeout = timeout
            self.terminated = False
        def poll(self): return self.returncode
        def wait(self, timeout):
            if self.timeout and not self.terminated:
                raise subprocess.TimeoutExpired('synthetic', timeout)
            self.returncode = 0
            return 0
        def terminate(self): self.terminated = True
        def kill(self): self.terminated = True

    def mailbox(self, output=b'typed_benchmark=cleaned\nINSTRUMENTATION_CODE: -1\n', **process):
        adb = SimpleNamespace(run=lambda *args: '', argv=lambda *args: ['synthetic-adb', *args])
        mailbox = wear.Mailbox(adb, 7, 180, 'custom-luna-none')
        mailbox.process = self.Process(**process)
        mailbox.log = tempfile.TemporaryFile()
        mailbox.log.write(output)
        mailbox.log.flush()
        mailbox.command = AsyncMock(return_value={'status':'cleaned'})
        return mailbox

    async def test_cleanup_requires_native_ack_exit_and_completion_output(self):
        mailbox = self.mailbox()
        await mailbox.close()
        self.assertTrue(mailbox.cleaned)
        self.assertTrue(mailbox.log.closed)
        await mailbox.close()  # Verified cleanup is idempotent, no second device command.
        mailbox.command.assert_awaited_once_with('close')

    async def test_cleanup_rejects_missing_ack_premature_exit_or_timeout(self):
        for kind in ('ack', 'output', 'exited', 'timeout'):
            with self.subTest(kind=kind):
                mailbox = self.mailbox(output=b'' if kind == 'output' else b'typed_benchmark=cleaned\nINSTRUMENTATION_CODE: -1\n',
                    exited=kind == 'exited', timeout=kind == 'timeout')
                if kind == 'ack': mailbox.command = AsyncMock(return_value={'status':'failed'})
                with self.assertRaises(wear.Blocked):
                    await mailbox.close()
                self.assertFalse(mailbox.cleaned)
                self.assertTrue(mailbox.log.closed)

    async def test_premature_instrumentation_exit_fails_start(self):
        adb = SimpleNamespace(run=lambda *args: '', argv=lambda *args: ['synthetic-adb', *args])
        mailbox = wear.Mailbox(adb, 7, 180, 'custom-luna-none')
        with patch.object(wear.subprocess, 'Popen', return_value=self.Process(exited=True)):
            with self.assertRaisesRegex(wear.Blocked, 'start_failed'):
                await mailbox.start()
        mailbox.log.close()

    async def test_existing_app_process_blocks_instrumentation(self):
        class ADB:
            device = None
            def run(self, *args):
                if args == ('devices', '-l'): return 'synthetic-watch device model:SM_L315F'
                if args == ('shell', f'pidof {wear.PACKAGE} || true'): return '123'
                raise AssertionError(args)
        args = SimpleNamespace(adb='synthetic-adb', ssh=None, remote_adb=None)
        report = {}
        with patch.object(common, 'ADB', return_value=ADB()), patch.object(wear, 'build_identity', return_value={}), \
                patch.object(wear.subprocess, 'Popen') as launch:
            with self.assertRaisesRegex(common.Blocked, 'existing_app_process'):
                await wear.run_wear(args, report, common)
        launch.assert_not_called()

    async def test_no_send_probe_does_not_invoke_suite_or_submit(self):
        class ADB:
            device = None
            def run(self, *args):
                if args == ('devices', '-l'): return 'synthetic-watch device model:SM_L315F'
                if args == ('shell', f'pidof {wear.PACKAGE} || true'): return ''
                if args == ('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP'): return ''
                raise AssertionError(args)
        class Mailbox:
            cleaned = False
            def __init__(self, restored): self.commands, self.restored = [], restored
            async def start(self): pass
            async def close(self): self.cleaned = True
            async def command(self, operation, **args):
                self.commands.append(operation)
                if operation == 'setup_case': return {'status':'configured'}
                if operation == 'probe':
                    self.probe_prompt = args['prompt']
                    return {'status':'ready', 'ui_probe':True, 'send_ready':True, 'latest_restored':self.restored}
                raise AssertionError(operation)
        args = SimpleNamespace(adb='synthetic-adb', ssh=None, remote_adb=None,
            max_live_calls=7, turn_timeout=180, model='custom-luna-none', wear_ui_probe=True)
        for restored in (True, False):
            with self.subTest(restored=restored):
                report = {'cases':[], 'cleanup':{}, 'limitations':[]}
                mailbox = Mailbox(restored)
                with patch.object(common, 'ADB', return_value=ADB()), patch.object(wear, 'build_identity', return_value={}), \
                        patch.object(wear, 'Mailbox', return_value=mailbox), patch.object(common, 'run_suite', new_callable=AsyncMock) as suite:
                    if restored:
                        await wear.run_wear(args, report, common)
                    else:
                        with self.assertRaisesRegex(common.Blocked, 'ui_probe_failed'):
                            await wear.run_wear(args, report, common)
                self.assertEqual(mailbox.commands, ['setup_case', 'probe'])
                self.assertGreater(len(mailbox.probe_prompt), 256)
                self.assertLessEqual(len(mailbox.probe_prompt.encode()), 32768)
                self.assertTrue(mailbox.cleaned)
                suite.assert_not_awaited()
                self.assertEqual(report['admissions']['submitted'], 0)
                self.assertEqual(report['suite'], 'wear-native-ui-probe')
                self.assertEqual(report['coverage'], 'probe_only')
                self.assertEqual(report.get('ui_probe') == 'pass', restored)

    def test_probe_cannot_be_applied_to_phone_or_desktop(self):
        for platform in ('android', 'desktop'):
            args = common.parser().parse_args(['--platform', platform, '--wear-ui-probe'])
            with self.subTest(platform=platform), self.assertRaisesRegex(common.Blocked, 'probe_requires_wear'):
                common.authorize(args)

    def test_wear_performance_build_label_is_not_a_phone_or_desktop_variant(self):
        for platform in ('android', 'desktop'):
            permitted = ['--live', '--allow-device', '--device', 'a17', '--adb', '/synthetic/adb'] if platform == 'android' else [
                '--live', '--binary', '/synthetic/binary', '--tools', '/synthetic/tools',
                '--config', '/synthetic/config', '--offline-evidence', '/synthetic/evidence']
            args = common.parser().parse_args(['--platform', platform, '--build-variant', 'performance', *permitted])
            with self.subTest(platform=platform), self.assertRaisesRegex(common.Blocked, 'performance_variant_requires_wear'):
                common.authorize(args)


class ReportRetentionTests(unittest.TestCase):
    def report(self, index, padding=0):
        return {'schema':1, 'suite':'wear-native-ui-probe' if index % 2 else 'typed-model-matrix',
                'synthetic_padding':'x' * padding}

    def test_expired_probe_and_typed_reports_share_age_retention(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index in range(2):
                path = root / f'typed-benchmark-{index}.json'
                path.write_text(json.dumps(self.report(index)))
                old = time.time() - 91 * 86400
                os.utime(path, (old, old))
            foreign = root / 'typed-benchmark-foreign.json'
            foreign.write_text('{"suite":"synthetic-foreign","schema":1}')
            result = common.write_report(root, self.report(1))
            self.assertTrue(result.is_file())
            self.assertFalse((root / 'typed-benchmark-0.json').exists())
            self.assertFalse((root / 'typed-benchmark-1.json').exists())
            self.assertTrue(foreign.exists())

    def test_probe_and_typed_reports_share_file_and_byte_budgets(self):
        for count, padding in ((100, 0), (80, 120000)):
            with self.subTest(count=count), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                for index in range(count):
                    (root / f'typed-benchmark-{index}.json').write_text(json.dumps(self.report(index, padding)))
                common.write_report(root, self.report(1))
                reports = list(root.glob('typed-benchmark-*.json'))
                self.assertLessEqual(len(reports), 100)
                self.assertLessEqual(sum(path.stat().st_size for path in reports), 8 * 1024 * 1024)


if __name__ == '__main__':
    unittest.main()
