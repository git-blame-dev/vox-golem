import unittest
import asyncio
from pathlib import Path
import tempfile
import hashlib
import io
import json
import sys
from contextlib import redirect_stdout
from unittest.mock import AsyncMock, patch
import benchmark as common
import wear_voice as voice


def output():
    trial = dict.fromkeys(voice.ONE + voice.POSITIVE, '1')
    trial.update(stage='complete', model='custom-luna-none', idle='true', fixture_word_count='4',
                 fixture_sample_rate_hz='16000', fixture_channels='1', fixture_bits_per_sample='16',
                 fixture_bytes='32000', fixture_duration_ms='1000', fixture_pcm_sha256='a' * 64,
                 ACTIVITY_ACTIVITY_CREATED_ms='0')
    final = dict(synthetic_protocol='1', cleanup_verified='true', tts_retired='true',
                 benchmark_result='complete', completed_trials='1', model='custom-luna-none', phase='trial_1')
    return ('\n'.join('INSTRUMENTATION_STATUS: ' + k + '=' + v for k, v in trial.items()) +
            '\nINSTRUMENTATION_STATUS_CODE: 0\n' +
            '\n'.join('INSTRUMENTATION_RESULT: ' + k + '=' + v for k, v in final.items()) +
            '\nINSTRUMENTATION_CODE: -1\n').encode()


class VoiceTests(unittest.TestCase):
    def test_unused_remote_transport_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            args.remote_adb = '/synthetic/unused-adb'
            with self.assertRaises(common.Blocked):
                voice.authorize(args, common)
    def test_voice_reports_private_and_separate_from_typed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            typed = common.write_report(root, {'schema':1, 'suite':'typed-model-matrix'})
            voice_report = common.write_report(root, {'schema':1, 'suite':'synthetic-voice-pipeline'})
            self.assertTrue(typed.exists())
            self.assertTrue(voice_report.name.startswith('voice-benchmark-'))
            self.assertEqual(voice_report.stat().st_mode & 0o777, 0o600)

    def test_authorization_fixed_budget_and_unsupported_controls(self):
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / 'synthetic.apk'
            apk.write_bytes(b'synthetic')
            argv = ['--platform', 'wear', '--workload', 'synthetic-voice', '--live', '--allow-device',
                    '--device', 'watch7', '--adb', '/synthetic/adb', '--apk', str(apk),
                    '--helper-apk', str(apk),
                    '--build-variant', 'performance', '--max-live-calls', '1']
            with patch.object(voice.sys, 'argv', ['benchmark']):
                voice.authorize(common.parser().parse_args(argv), common)
                for extra in (['--max-live-calls', '7'], ['--wear-ui-probe'], ['--cases', 'arithmetic'],
                              ['--collect-stages'], ['--history-seed-turns', '2'], ['--binary', '/synthetic/bin'],
                              ['--platform', 'desktop'], ['--repetitions', '2'],
                              ['--turn-timeout', '180'], ['--history-seed-turns', '1'], ['--port', '9223']):
                    with self.subTest(extra=extra), self.assertRaises(common.Blocked):
                        voice.authorize(common.parser().parse_args(argv + extra), common)

    def test_complete_is_not_correctness_and_raw_keys_omitted(self):
        raw = output().replace(b'INSTRUMENTATION_STATUS_CODE:', b'INSTRUMENTATION_STATUS: private=secret\nINSTRUMENTATION_STATUS_CODE:')
        trial, success = voice.parse_output(raw, 0)
        self.assertTrue(success)
        self.assertNotIn('private', trial)

    def test_final_failure_preserves_known_trial_but_never_passes(self):
        trial, success = voice.parse_output(output().replace(b'cleanup_verified=true', b'cleanup_verified=false'), 0)
        self.assertFalse(success)
        self.assertEqual(trial['generation'], 1)

    def test_missing_duplicate_bad_pcm_extra_trial_and_wrong_exit(self):
        for raw in (output().replace(b'finals=1', b'finals=2'),
                    output().replace(b'fixture_bytes=32000', b'fixture_bytes=32001'),
                    output().replace(b'fixture_words_match=1', b'fixture_words_match=no'),
                    output() + b'INSTRUMENTATION_RESULT: model=custom-luna-none\n',
                    output() + b'INSTRUMENTATION_STATUS: synthetic_trial=2\nINSTRUMENTATION_STATUS_CODE: 0\n'):
            with self.subTest(raw=raw[:20]), self.assertRaises(common.Blocked):
                voice.parse_output(raw, 0)
        self.assertFalse(voice.parse_output(output(), 1)[1])
        self.assertFalse(voice.parse_output(output().replace(b'INSTRUMENTATION_CODE: -1', b'INSTRUMENTATION_CODE: 0'), 0)[1])

    def test_old_helper_trial_without_protocol_fails_success(self):
        self.assertFalse(voice.parse_output(output().replace(b'INSTRUMENTATION_RESULT: synthetic_protocol=1\n', b''), 0)[1])

    def test_trial_after_final_is_invalid(self):
        raw = output()
        split = raw.index(b'INSTRUMENTATION_RESULT:')
        with self.assertRaises(common.Blocked):
            voice.parse_output(raw[split:] + raw[:split], 0)

    def test_status_terminator_cannot_follow_final_or_terminal(self):
        raw = output()
        terminator = b'INSTRUMENTATION_STATUS_CODE: 0\n'
        without = raw.replace(terminator, b'')
        for malformed in (
                without.replace(b'INSTRUMENTATION_CODE: -1\n', terminator + b'INSTRUMENTATION_CODE: -1\n'),
                without + terminator):
            with self.subTest(malformed=malformed[-100:]), self.assertRaises(common.Blocked):
                voice.parse_output(malformed, 0)

    def test_terminal_cannot_precede_status_completion_or_final(self):
        raw = output()
        terminal = b'INSTRUMENTATION_CODE: -1\n'
        without = raw.replace(terminal, b'')
        for marker in (b'INSTRUMENTATION_STATUS_CODE: 0\n',
                       b'INSTRUMENTATION_RESULT: synthetic_protocol=1\n'):
            with self.subTest(marker=marker), self.assertRaises(common.Blocked):
                voice.parse_output(without.replace(marker, terminal + marker), 0)

    def test_known_protocol_records_cannot_follow_terminal(self):
        for record in (b'INSTRUMENTATION_STATUS: ignored=unsafe\n',
                       b'INSTRUMENTATION_STATUS_CODE: 0\n',
                       b'INSTRUMENTATION_RESULT: ignored=unsafe\n',
                       b'INSTRUMENTATION_CODE: -1\n'):
            with self.subTest(record=record), self.assertRaises(common.Blocked):
                voice.parse_output(output() + record, 0)
    def test_voice_budget_default_is_not_consent(self):
        import wear_voice as voice
        args = common.parser().parse_args(['--platform', 'wear', '--workload', 'synthetic-voice'])
        with self.assertRaises(common.Blocked):
            voice.authorize(args, common)

    def test_watchdog_is_observation_not_retirement(self):
        import wear_voice as voice
        self.assertEqual(voice.HOST_WATCHDOG_SECONDS, 239 + 2 * 5 + 30 + 21)

    def test_transport_loss_reaps_only_local_process(self):
        async def check():
            process = unittest.mock.Mock()
            process.returncode = None
            process.stdout.read = AsyncMock(side_effect=ConnectionError())
            process.wait = AsyncMock(return_value=0)
            cleanup = {'native_retirement':'unverified'}
            adb = unittest.mock.Mock()
            adb.argv.return_value = ['synthetic-adb']
            with patch.object(voice.asyncio, 'create_subprocess_exec', AsyncMock(return_value=process)) as start:
                with self.assertRaises(ConnectionError):
                    await voice.observe(adb, cleanup)
            process.terminate.assert_called_once()
            process.wait.assert_awaited_once()
            self.assertEqual(start.await_count, 1)
            self.assertEqual(cleanup['native_retirement'], 'unverified')
            self.assertEqual(cleanup['local_transport'], 'reaped')
            command = adb.argv.call_args.args
            self.assertIn('syntheticAudio', command)
            self.assertNotIn('TypedAskInstrumentation', ' '.join(command))
        asyncio.run(check())

    def test_output_is_bounded_during_read(self):
        async def check():
            process = unittest.mock.Mock()
            process.returncode = None
            process.stdout.read = AsyncMock(return_value=b'x' * 4096)
            process.wait = AsyncMock(return_value=0)
            adb = unittest.mock.Mock()
            adb.argv.return_value = ['synthetic-adb']
            with patch.object(voice.asyncio, 'create_subprocess_exec', AsyncMock(return_value=process)):
                with self.assertRaises(common.Blocked):
                    await voice.observe(adb, {})
            self.assertEqual(process.stdout.read.await_count, voice.MAX_OUTPUT_BYTES // 4096 + 1)
            process.terminate.assert_called_once()
        asyncio.run(check())

    def test_watchdog_reaps_transport_without_native_cleanup_claim(self):
        async def check():
            process = unittest.mock.Mock()
            process.returncode = None
            async def hang(size):
                await asyncio.Future()
            process.stdout.read = hang
            process.wait = AsyncMock(return_value=0)
            adb = unittest.mock.Mock()
            adb.argv.return_value = ['synthetic-adb']
            cleanup = {'native_retirement':'unverified'}
            with patch.object(voice, 'HOST_WATCHDOG_SECONDS', .001), patch.object(
                    voice.asyncio, 'create_subprocess_exec', AsyncMock(return_value=process)) as start:
                with self.assertRaises(TimeoutError):
                    await voice.observe(adb, cleanup)
            self.assertEqual(start.await_count, 1)
            process.terminate.assert_called_once()
            self.assertEqual(cleanup['native_retirement'], 'unverified')
        asyncio.run(check())

    def test_old_helper_rejected(self):
        import wear_voice as voice
        with self.assertRaises(common.Blocked):
            voice.parse_output(b'INSTRUMENTATION_RESULT: benchmark_result=complete\nINSTRUMENTATION_CODE: -1\n', 0)


def voice_args(root):
    apk = root / 'synthetic.apk'
    apk.write_bytes(b'synthetic-apk')
    return common.parser().parse_args(['--platform', 'wear', '--workload', 'synthetic-voice',
        '--live', '--allow-device', '--device', 'watch7', '--adb', '/synthetic/adb',
        '--apk', str(apk), '--helper-apk', str(apk), '--build-variant', 'performance',
        '--max-live-calls', '1', '--output', str(root / 'reports')])


class LifecycleTests(unittest.TestCase):
    def test_shared_cli_dispatches_without_typed_authorization(self):
        with patch.object(sys, 'argv', ['benchmark', '--platform', 'wear', '--workload', 'synthetic-voice']), patch.object(
                voice, 'main', return_value=2) as main, patch.object(common, 'authorize') as authorize:
            self.assertEqual(common.main(), 2)
        main.assert_called_once()
        authorize.assert_not_called()

    def fake_adb(self, *, existing='', helper_digest=None):
        digest = helper_digest or hashlib.sha256(b'synthetic-apk').hexdigest()
        def run(*args):
            if args == ('devices', '-l'):
                return 'synthetic-watch device model:SM_L315F'
            if args == ('shell', 'ps', '-A', '-o', 'NAME'):
                return 'NAME\n' + existing
            if args == ('shell', 'pm', 'path', voice.wear.PACKAGE + '.test'):
                return 'package:/synthetic/helper.apk'
            if args == ('shell', 'sha256sum', '/synthetic/helper.apk'):
                return digest + ' /synthetic/helper.apk'
            raise AssertionError('unexpected fake command')
        adb = unittest.mock.Mock()
        adb.run.side_effect = run
        return adb

    def invoke(self, args, **patches):
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            code = voice.main(args, common)
        return code, json.loads(stdout.getvalue())

    def test_stale_helper_and_existing_processes_block_before_observe(self):
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            for adb in (self.fake_adb(helper_digest='b' * 64),
                        self.fake_adb(existing=voice.wear.PACKAGE),
                        self.fake_adb(existing=voice.wear.PACKAGE + ':worker'),
                        self.fake_adb(existing=voice.wear.PACKAGE + '.test')):
                with patch.object(common, 'ADB', return_value=adb), patch.object(
                        voice.wear, 'build_identity', return_value={}), patch.object(voice, 'observe', AsyncMock()) as observe:
                    report = {'cleanup':{}, 'admissions':{'submitted':0, 'observed':0}}
                    with self.assertRaises(common.Blocked):
                        asyncio.run(voice.run(args, report, common))
                    observe.assert_not_awaited()
                    self.assertEqual(report['admissions']['submitted'], 0)

    def test_target_identity_failure_blocks_before_helper_and_observe(self):
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            adb = self.fake_adb()
            with patch.object(common, 'ADB', return_value=adb), patch.object(voice.wear,
                    'build_identity', side_effect=common.Blocked('watch_installed_artifact_mismatch')), patch.object(
                    voice, 'observe', AsyncMock()) as observe:
                with self.assertRaises(common.Blocked):
                    asyncio.run(voice.run(args, {}, common))
                observe.assert_not_awaited()
                self.assertEqual(adb.run.call_count, 1)

    def test_authorized_dispatch_private_report_and_unverified_exit(self):
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            with patch.object(common, 'ADB', return_value=self.fake_adb()), patch.object(
                    voice.wear, 'build_identity', return_value={'debuggable':False}), patch.object(
                    voice, 'observe', AsyncMock(return_value=voice.parse_output(output(), 0))) as observe:
                code, summary = self.invoke(args)
            self.assertEqual(code, 2)
            self.assertEqual(summary['status'], 'unverified')
            self.assertTrue(summary['workflow_completed'])
            self.assertTrue(summary['trial_completed'])
            self.assertEqual(summary['admissions']['observed'], 1)
            observe.assert_awaited_once()
            path = Path(summary['report_path'])
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            report = json.loads(path.read_bytes())
            self.assertEqual(report['helper']['installed_apk_sha256'], report['helper']['provided_apk_sha256'])
            self.assertEqual(report['checks']['answer_correctness'], 'unverified')

    def test_timeout_summary_has_unknown_admissions_and_native_retirement(self):
        async def uncertain(adb, cleanup, admissions):
            admissions.update(submitted=None, observed=None)
            raise TimeoutError()
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            with patch.object(common, 'ADB', return_value=self.fake_adb()), patch.object(
                    voice.wear, 'build_identity', return_value={}), patch.object(voice, 'observe', uncertain):
                code, summary = self.invoke(args)
            self.assertEqual(code, 1)
            self.assertIsNone(summary['admissions']['submitted'])
            self.assertEqual(summary['cleanup']['native_retirement'], 'unverified')
            self.assertFalse(summary['workflow_completed'])

    def test_authorization_failure_report_and_requested_platform(self):
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            args.platform = 'desktop'
            with patch.object(common, 'ADB') as adb:
                code, summary = self.invoke(args)
            adb.assert_not_called()
            self.assertEqual(code, 1)
            report = json.loads(Path(summary['report_path']).read_bytes())
            self.assertEqual(report['platform'], 'desktop')
            self.assertEqual(report['admissions']['submitted'], 0)

    def test_write_failure_sanitized(self):
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            args.live = False
            with patch.object(common, 'write_report', side_effect=OSError('private-payload')):
                code, summary = self.invoke(args)
            self.assertEqual(code, 1)
            self.assertEqual(summary['failure_category'], 'report_writefail')
            self.assertNotIn('private-payload', json.dumps(summary))

    def test_preflight_failure_retries_private_report_without_transport(self):
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            original = common.preflight_output
            calls = []
            def preflight(path):
                calls.append(path)
                if len(calls) == 1:
                    raise common.Blocked('report_preflight_failed')
                return original(path)
            with patch.object(common, 'preflight_output', side_effect=preflight), patch.object(common, 'ADB') as adb:
                code, summary = self.invoke(args)
            self.assertEqual(code, 1)
            self.assertTrue(Path(summary['report_path']).exists())
            adb.assert_not_called()

    def test_cancellation_still_reaps_owned_transport(self):
        async def check():
            process = unittest.mock.Mock()
            process.returncode = None
            entered = asyncio.Event()
            async def hang(size):
                entered.set()
                await asyncio.Future()
            process.stdout.read = hang
            process.wait = AsyncMock(return_value=0)
            adb = unittest.mock.Mock()
            adb.argv.return_value = ['synthetic-adb']
            cleanup = {'native_retirement':'unverified'}
            with patch.object(voice.asyncio, 'create_subprocess_exec', AsyncMock(return_value=process)):
                task = asyncio.create_task(voice.observe(adb, cleanup))
                await entered.wait()
                task.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await asyncio.wait_for(task, 1)
            process.terminate.assert_called_once()
            self.assertEqual(cleanup['local_transport'], 'reaped')
            self.assertEqual(cleanup['native_retirement'], 'unverified')
        asyncio.run(check())

    def test_list_has_no_output_write_or_transport(self):
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            args.list = True
            with patch.object(common, 'preflight_output') as preflight, patch.object(common, 'ADB') as adb, redirect_stdout(io.StringIO()):
                self.assertEqual(voice.main(args, common), 0)
            preflight.assert_not_called()
            adb.assert_not_called()

    def test_helper_required_bounded_regular_and_typed_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            args = voice_args(Path(directory))
            with self.assertRaises(common.Blocked):
                common.authorize(args)
            for helper in (None, Path(directory), Path(directory) / 'missing.apk'):
                args.helper_apk = helper
                with self.assertRaises(common.Blocked):
                    voice.authorize(args, common)
            link = Path(directory) / 'link.apk'
            link.symlink_to(args.apk)
            args.helper_apk = link
            with self.assertRaises(common.Blocked):
                voice.authorize(args, common)

    def test_origin_and_contradictory_final_rejected(self):
        for raw in (output().replace(b'ACTIVITY_ACTIVITY_CREATED_ms=0\n', b''),
                    output().replace(b'ACTIVITY_ACTIVITY_CREATED_ms=0', b'ACTIVITY_ACTIVITY_CREATED_ms=1'),
                    output().replace(b'INSTRUMENTATION_CODE:', b'INSTRUMENTATION_RESULT: failure_class=private\nINSTRUMENTATION_CODE:')):
            with self.assertRaises(common.Blocked):
                voice.parse_output(raw, 0)

    def test_required_fields_missing_or_duplicate_never_qualify(self):
        raw = output()
        for line in raw.splitlines(keepends=True):
            if line.startswith(b'INSTRUMENTATION_STATUS: '):
                with self.subTest(line=line), self.assertRaises(common.Blocked):
                    voice.parse_output(raw.replace(line, b''), 0)
                with self.assertRaises(common.Blocked):
                    voice.parse_output(raw.replace(line, line + line), 0)
            elif line.startswith((b'INSTRUMENTATION_RESULT: ', b'INSTRUMENTATION_CODE: ')):
                self.assertFalse(voice.parse_output(raw.replace(line, b''), 0)[1])
                duplicated = raw.replace(line, line + line)
                if line.startswith(b'INSTRUMENTATION_RESULT: '):
                    with self.assertRaises(common.Blocked):
                        voice.parse_output(duplicated, 0)
                else:
                    with self.assertRaises(common.Blocked):
                        voice.parse_output(duplicated, 0)

    def test_hanging_reap_is_bounded_and_not_claimed(self):
        async def check():
            process = unittest.mock.Mock()
            process.returncode = None
            async def hang(*args):
                await asyncio.Future()
            process.wait = hang
            process.stdout.read = hang
            cleanup = {'native_retirement':'unverified'}
            with patch.object(voice, 'TRANSPORT_REAP_SECONDS', .001):
                await asyncio.wait_for(voice.retire_transport(process, cleanup), 1)
            process.terminate.assert_called_once()
            process.kill.assert_called_once()
            self.assertEqual(cleanup['local_transport'], 'reap_unverified')
            self.assertEqual(cleanup['native_retirement'], 'unverified')
        asyncio.run(check())

    def test_actual_local_subprocess_backpressure_is_reaped(self):
        async def check():
            adb = unittest.mock.Mock()
            adb.argv.return_value = [sys.executable, '-c', 'import os;\nwhile True: os.write(1, b"x" * 4096)']
            cleanup = {'native_retirement':'unverified'}
            admissions = {'submitted':0, 'observed':0}
            with self.assertRaises(common.Blocked):
                await asyncio.wait_for(voice.observe(adb, cleanup, admissions), 5)
            self.assertEqual(cleanup['local_transport'], 'reaped')
            self.assertEqual(cleanup['native_retirement'], 'unverified')
            self.assertIsNone(admissions['submitted'])
        asyncio.run(check())
