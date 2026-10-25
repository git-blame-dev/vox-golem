import asyncio
import json
import unittest
from pathlib import Path
import tempfile
import os
import time
import sys
from unittest.mock import AsyncMock, patch

import benchmark
import instrumentation
import phone_voice
import test_wear_voice
import voice_contract
import wear_voice


def output():
    raw = test_wear_voice.output().replace(b'fixture_word_count=4', b'fixture_word_count=10')
    raw = raw.replace(b'synthetic_protocol=1', b'synthetic_protocol=2').replace(
        b'cleanup_verified=true', b'cleanup_verified=false')
    status = {'frontend_submit_observed':'1', 'canonical_answer_exact':'true',
              'synthetic_protocol':'2', 'contract_sha256':voice_contract.contract_sha256(),
              'profile_mapping_verified':'true', 'frontend_ask_clicked':'true',
              'canonical_ui_equal':'true', 'ui_correlated_rows':'1'}
    raw = raw.replace(b'INSTRUMENTATION_STATUS_CODE:',
        ''.join('INSTRUMENTATION_STATUS: ' + k + '=' + v + '\n' for k, v in status.items()).encode() +
        b'INSTRUMENTATION_STATUS_CODE:')
    final = {'native_cleanup_verified':'false', 'native_resources_retired':'true',
             'activity_webview_retirement_pending':'true', 'profile_cleanup_pending':'true',
             'contract_sha256':voice_contract.contract_sha256(),
             'cleanup_token':'00000000-0000-4000-8000-000000000000'}
    return raw.replace(b'INSTRUMENTATION_CODE:',
        ''.join('INSTRUMENTATION_RESULT: ' + k + '=' + v + '\n' for k, v in final.items()).encode() +
        b'INSTRUMENTATION_CODE:')


def native_failure(stage='failed', *, status=True, retired=True):
    lines = []
    if status:
        fields = {'stage':stage, 'generation':'0', 'finals':'0', 'profile_mapping_verified':'true',
                  'frontend_ask_clicked':'true', 'FRONTEND_ASK_CLICKED_ms':'12',
                  'private_unknown':'synthetic-private-payload'}
        lines += ['INSTRUMENTATION_STATUS: ' + k + '=' + v for k, v in fields.items()]
        lines.append('INSTRUMENTATION_STATUS_CODE: 0')
    final = {'synthetic_protocol':'2', 'benchmark_result':'failed', 'completed_trials':'0',
             'phase':'trial_1' if status else 'process_reservation',
             'failure_class':'IllegalStateException' if status else 'ProcessReservationFailure',
             'cleanup_verified':'false'}
    if retired:
        final.update(native_cleanup_verified='false', native_resources_retired='true',
                     activity_webview_retirement_pending='true', tts_retired='true', profile_cleanup_pending='true',
                     cleanup_token='00000000-0000-4000-8000-000000000000')
    lines += ['INSTRUMENTATION_RESULT: ' + k + '=' + v for k, v in final.items()]
    lines.append('INSTRUMENTATION_CODE: 0')
    return ('\n'.join(lines) + '\n').encode()


def cleaned_output():
    fields = {'synthetic_protocol':'2', 'benchmark_result':'cleaned', 'cleanup_verified':'true',
              'producer_exit_verified':'true', 'activity_webview_retired':'true', 'storage_cleanup_verified':'true',
              'profile_cleaned':'true', 'completed_trials':'0', 'phase':'profile_cleanup'}
    return ('\n'.join('INSTRUMENTATION_RESULT: ' + k + '=' + v for k, v in fields.items()) +
            '\nINSTRUMENTATION_CODE: -1\n').encode()


class PhoneTest(unittest.TestCase):
    def test_failure_storage_progress_is_bounded_canonical_and_optional(self):
        fields = {'benchmark_result':'failed', 'failure_class':'IOException'}
        bounds = {'deleted_owned_directory_count':32, 'deleted_preference_count':24,
                  'deleted_session_count':8}
        self.assertFalse(set(bounds) & phone_voice.final_diagnostic(fields).keys())
        for key, maximum in bounds.items():
            for count in (0, maximum):
                self.assertEqual(phone_voice.final_diagnostic(dict(fields, **{key:str(count)}))[key], count)
            for invalid in (None, True, 0, maximum, '-1', '01', str(maximum + 1), '1.0',
                            'synthetic-private-payload'):
                with self.subTest(key=key, invalid=invalid):
                    safe = phone_voice.final_diagnostic(dict(fields, **{key:invalid},
                        raw_message='synthetic-private-payload', recovered_session_count='1'))
                    self.assertNotIn(key, safe)
                    self.assertNotIn('recovered_session_count', safe)
                    self.assertNotIn('synthetic-private-payload', json.dumps(safe))
        safe = phone_voice.final_diagnostic(dict(fields, cleanup_failure_reason='pending_markers_ambiguous'))
        self.assertEqual(safe['cleanup_failure_reason'], 'unknown')

    def test_cleanup_failure_diagnostic_is_bounded_and_does_not_invent_facts(self):
        fields = {'phase':'profile_cleanup_pending', 'benchmark_result':'failed',
                  'failure_class':'IllegalStateException',
                  'cleanup_failure_reason':'marker_schema_invalid',
                  'pending_count_class':'multiple', 'deletion_started':'false'}
        safe = phone_voice.final_diagnostic(fields)
        self.assertEqual(safe['cleanup_failure_reason'], 'marker_schema_invalid')
        self.assertEqual(safe['pending_count_class'], 'multiple')
        self.assertFalse(safe['deletion_started'])
        self.assertNotIn('producer_exit_verified', safe)
        self.assertNotIn('deleted_root_count', safe)
        self.assertEqual(safe['phase'], 'profile_cleanup_pending')
        for invalid in ('synthetic-private-payload', '-1', '4', '00'):
            safe = phone_voice.final_diagnostic(dict(fields,
                cleanup_failure_reason='synthetic-private-payload',
                pending_count_class='synthetic-private-payload',
                deletion_started='synthetic-private-payload', deleted_root_count=invalid))
            self.assertEqual(safe['cleanup_failure_reason'], 'unknown')
            self.assertNotIn('pending_count_class', safe)
            self.assertNotIn('deletion_started', safe)
            self.assertNotIn('deleted_root_count', safe)
            self.assertNotIn('synthetic-private-payload', json.dumps(safe))
        for count in range(4):
            safe = phone_voice.final_diagnostic(dict(fields, deleted_root_count=str(count),
                                                     producer_exit_verified='true'))
            self.assertEqual(safe['deleted_root_count'], count)
            self.assertTrue(safe['producer_exit_verified'])

    def test_optional_offsets_and_failure_diagnostics_are_bounded(self):
        trial, _, _ = instrumentation.records(output(), trial=True)
        for key in phone_voice.PHONE_OFFSETS:
            trial[key] = '42'
        checked = phone_voice.checked_phone_trial(trial)
        for key in phone_voice.PHONE_OFFSETS:
            self.assertEqual(checked[key], 42)
            for invalid in ('-1', '300001', 'true', '01', 'synthetic-private-payload'):
                with self.subTest(key=key, invalid=invalid), self.assertRaises(benchmark.Blocked):
                    phone_voice.checked_phone_trial(dict(trial, **{key:invalid}))
        safe = phone_voice.safe_status(dict(trial, private='synthetic-private-payload',
            FRONTEND_ASK_CLICKED_ms='synthetic-private-payload'))
        self.assertNotIn('synthetic-private-payload', json.dumps(safe))
        self.assertNotIn('FRONTEND_ASK_CLICKED_ms', safe)
        self.assertEqual(phone_voice.final_diagnostic({'phase':'fixture', 'benchmark_result':'failed',
            'failure_class':'synthetic-private-payload'}), {'phase':'fixture', 'result':'failed', 'failure_class':'other'})

    def test_owned_cleanup_requires_actual_complete_native_proof(self):
        _, final, terminal = instrumentation.records(native_failure(), trial='optional')
        digest = voice_contract.contract_sha256()
        self.assertIsNotNone(phone_voice.cleanup_token(final, 0, terminal, digest))
        for changes in ({'model':'other'}, {'contract_sha256':'a' * 64}, {'native_cleanup_verified':'true'},
                        {'native_resources_retired':'false'}, {'activity_webview_retirement_pending':'false'},
                        {'tts_retired':'1'}, {'profile_cleanup_pending':'false'}, {'cleanup_token':'bad'},
                        {'synthetic_protocol':'1'}, {'cleanup_verified':'true'}):
            with self.subTest(changes=changes):
                self.assertIsNone(phone_voice.cleanup_token(dict(final, **changes), 0, terminal, digest))
        self.assertIsNone(phone_voice.cleanup_token(final, 1, terminal, digest))

    def test_old_ambiguous_native_cleanup_proof_is_rejected(self):
        _, final, terminal = instrumentation.records(native_failure(), trial='optional')
        for key in ('native_resources_retired', 'activity_webview_retirement_pending'):
            missing = dict(final)
            missing.pop(key)
            self.assertIsNone(phone_voice.cleanup_token(missing, 0, terminal, voice_contract.contract_sha256()))
        final.pop('native_resources_retired')
        final.pop('activity_webview_retirement_pending')
        final['native_cleanup_verified'] = 'true'
        self.assertIsNone(phone_voice.cleanup_token(final, 0, terminal, voice_contract.contract_sha256()))

    def test_actual_profile_flags_required(self):
        raw, _, _ = instrumentation.records(output(), trial=True)
        raw.pop('profile_mapping_verified')
        raw.pop('frontend_ask_clicked')
        for flags in ({}, {'profile_mapping_verified':'false', 'frontend_ask_clicked':'true'},
                      {'profile_mapping_verified':'true', 'frontend_ask_clicked':'false'}):
            trial = dict(raw, **flags)
            with self.subTest(flags=flags), self.assertRaises(benchmark.Blocked):
                phone_voice.checked_phone_trial(trial)

    def test_incremental_framing_callback_once_across_byte_boundaries(self):
        observed = []
        parser = instrumentation.PhaseParser(trial=True,
            on_trial=lambda trial: observed.append(phone_voice.checked_phone_trial(trial)))
        prefix, suffix = output().split(b'INSTRUMENTATION_RESULT:', 1)
        for byte in prefix:
            parser.feed(bytes([byte]))
        self.assertEqual(len(observed), 1)
        self.assertEqual(observed[0]['generation'], 1)
        self.assertNotIn('contract_sha256', observed[0])
        self.assertNotIn('canonical_answer_exact', observed[0])
        parser.feed(b'INSTRUMENTATION_RESULT:' + suffix)
        self.assertEqual(parser.finish(), instrumentation.records(output(), trial=True))
        self.assertEqual(len(observed), 1)

    def test_invalid_trial_counters_cannot_publish_partial_evidence(self):
        observed = []
        parser = instrumentation.PhaseParser(trial=True,
            on_trial=lambda trial: observed.append(phone_voice.checked_phone_trial(trial)))
        with self.assertRaises(benchmark.Blocked):
            parser.feed(output().replace(b'finals=1', b'finals=2'))
        self.assertEqual(observed, [])

    def test_wear_harness_includes_every_shared_native_fixture(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ('FiniteFixtureAudio.kt', 'BenchmarkVoiceContract.kt',
                         'LocalFixtureSynthesis.kt', 'FixturePcmProvenance.kt'):
                with self.subTest(owner=name):
                    path = root / 'tools/benchmark/android/src/main/java/synthetic' / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(b'synthetic-v1')
                    previous = wear_voice.harness_digest('synthetic-seed', root)
                    path.write_bytes(b'synthetic-v2')
                    self.assertNotEqual(previous, wear_voice.harness_digest('synthetic-seed', root))

    def test_shared_voice_retention_private_schema_two(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old = benchmark.write_report(root, {'schema':1, 'suite':'synthetic-voice-pipeline'})
            os.utime(old, (time.time() - 91 * 86400,) * 2)
            current = benchmark.write_report(root, {'schema':2, 'suite':'synthetic-voice-arithmetic'})
            self.assertFalse(old.exists())
            self.assertEqual(current.stat().st_mode & 0o777, 0o600)
            self.assertTrue(current.name.startswith('voice-benchmark-'))

    def test_exact_proof_no_token_export(self):
        safe, token, success = phone_voice.parse_trial(output(), 0, voice_contract.contract_sha256())
        self.assertTrue(success)
        self.assertEqual(safe['fixture_word_count'], 10)
        self.assertNotIn(token, json.dumps(safe))
        self.assertTrue(safe['canonical_answer_exact'])

    def test_final_failure_keeps_checked_trial_without_cleanup_success(self):
        for raw, code in ((output().replace(b'native_resources_retired=true', b'native_resources_retired=false'), 0),
                          (output().replace(b'INSTRUMENTATION_CODE: -1', b'INSTRUMENTATION_CODE: 0'), 0),
                          (output(), 1)):
            safe, token, success = phone_voice.parse_trial(raw, code, voice_contract.contract_sha256())
            self.assertEqual(safe['generation'], 1)
            self.assertIsNone(token)
            self.assertFalse(success)

    def test_harness_mutation_each_critical_source_owner(self):
        paths = [
            'tools/benchmark/instrumentation.py',
            'tools/benchmark/phone_voice.py',
            'tools/benchmark/voice_contract.py',
            'tools/benchmark/scenarios.json',
            'crates/assistant-plugin/android/src/main/java/PhoneActivityBindings.kt',
            'crates/assistant-plugin/android/src/benchmark/java/PhoneSyntheticVoiceBenchmark.kt',
            'crates/assistant-plugin/android/build.gradle.kts',
            'crates/assistant-plugin/android/benchmark-rules.pro',
            'apps/android-tauri/src-tauri/gen/android/app/build.gradle.kts',
            'apps/android-tauri/src-tauri/gen/android/app/src/main/java/MainActivity.kt',
            'apps/android-tauri/src-tauri/gen/android/app/src/main/AndroidManifest.xml',
            'tools/benchmark/android/src/main/java/FiniteFixtureAudio.kt',
            'apps/wear/voice-benchmark/build.gradle.kts',
            'apps/wear/voice-benchmark/src/main/java/PhoneSyntheticVoiceInstrumentation.java',
            'apps/wear/voice-benchmark/src/main/AndroidManifest.xml']
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in paths:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'synthetic-v1')
            baseline = wear_voice.harness_digest('synthetic-seed', root, phone=True)
            for name in paths:
                with self.subTest(owner=name):
                    path = root / name
                    path.write_bytes(b'synthetic-v2')
                    self.assertNotEqual(baseline, wear_voice.harness_digest('synthetic-seed', root, phone=True))
                    path.write_bytes(b'synthetic-v1')
            transport = root / paths[0]
            legacy = wear_voice.harness_digest('synthetic-seed', root)
            transport.write_bytes(b'synthetic-v2')
            self.assertNotEqual(legacy, wear_voice.harness_digest('synthetic-seed', root))

    def test_phone_benchmark_proguard_rules_keep_all_reflected_facade_methods(self):
        rules = (benchmark.ROOT / 'crates/assistant-plugin/android/benchmark-rules.pro').read_text(encoding='utf-8')
        runner = (benchmark.ROOT /
            'apps/wear/voice-benchmark/src/main/java/dev/gitblame/voxgolem/android/speechtest/PhoneSyntheticVoiceInstrumentation.java'
        ).read_text(encoding='utf-8')
        self.assertIn('getMethod("reserveProcess", Instrumentation.class, String.class)', runner)
        for signature in (
            'public static void reserveProcess(android.app.Instrumentation);',
            'public static void reserveProcess(android.app.Instrumentation, java.lang.String);',
            'public static android.os.Bundle install(android.app.Instrumentation, android.os.Bundle);',
            'public static android.os.Bundle prepareTrial(android.app.Instrumentation);',
            'public static android.os.Bundle status(android.app.Instrumentation);',
            'public static android.os.Bundle close(android.app.Instrumentation);',
            'public static android.os.Bundle cleanup(android.app.Instrumentation, android.os.Bundle);',
        ):
            with self.subTest(signature=signature):
                self.assertIn(signature, rules)

    def test_version_hash_duplicate_order_and_counters(self):
        for raw in (output().replace(b'INSTRUMENTATION_RESULT: synthetic_protocol=2', b'INSTRUMENTATION_RESULT: synthetic_protocol=1'),
                    output().replace(b'INSTRUMENTATION_RESULT: contract_sha256=' + voice_contract.contract_sha256().encode(),
                                     b'INSTRUMENTATION_RESULT: contract_sha256=' + b'a' * 64)):
            safe, token, success = phone_voice.parse_trial(raw, 0, voice_contract.contract_sha256())
            self.assertFalse(success)
            self.assertIsNone(token)
            self.assertNotIn('contract_sha256', safe)
        for raw in (output().replace(b'finals=1', b'finals=2'),
                    output().replace(b'INSTRUMENTATION_CODE: -1', b'INSTRUMENTATION_CODE: -01'),
                    output().replace(b'frontend_submit_observed=1', b'frontend_submit_observed=true'),
                    output() + b'INSTRUMENTATION_RESULT: cleanup_verified=true\n',
                    output().replace(b'fixture_word_count=10', b'fixture_word_count=4')):
            with self.subTest(raw=raw[:20]), self.assertRaises(benchmark.Blocked):
                phone_voice.parse_trial(raw, 0, voice_contract.contract_sha256())

    def test_cleanup_only_cannot_emit_status(self):
        raw = b'INSTRUMENTATION_RESULT: synthetic_protocol=2\nINSTRUMENTATION_CODE: -1\n'
        self.assertEqual(instrumentation.bundles(raw, 0, trial=False)[0], {})
        with self.assertRaises(benchmark.Blocked):
            instrumentation.bundles(output(), 0, trial=False)


class GuardTest(unittest.IsolatedAsyncioTestCase):
    async def test_output_hook_precedes_status_callback_in_combined_chunk(self):
        payload = (b'INSTRUMENTATION_STATUS: inventory_ready=true\nINSTRUMENTATION_STATUS_CODE: 0\n'
                   b'INSTRUMENTATION_RESULT: category=inventory\nINSTRUMENTATION_CODE: 0\n')
        class LocalOnly:
            def argv(self, *args): return ['synthetic']
        class Stream:
            chunks = [payload, b'']
            async def read(self, size): return self.chunks.pop(0)
        class Process:
            stdout = Stream()
            returncode = 0
            async def wait(self): return 0
        events = []
        async def retire(process, cleanup): cleanup['local_transport'] = 'reaped'
        with patch.object(asyncio, 'create_subprocess_exec', AsyncMock(return_value=Process())):
            raw, rc = await instrumentation.observe(LocalOnly(), [], {}, 1, retire=retire,
                on_output=lambda chunk: events.append(('output', chunk)),
                on_trial=lambda bundle: events.append(('status', bundle)))
        self.assertEqual([event[0] for event in events], ['output', 'status'])
        self.assertEqual(events[0][1], payload)
        self.assertEqual((raw, rc), (payload, 0))

    async def test_output_hook_failure_retires_transport_and_preserves_unknown_admissions(self):
        class LocalOnly:
            def argv(self, *args): return ['synthetic']
        class Stream:
            async def read(self, size): return b'synthetic-private-payload'
        class Process:
            stdout = Stream()
            returncode = 0
            async def wait(self): return 0
        cleanup, admissions = {}, {}
        def fail(chunk): raise RuntimeError('synthetic-callback-failure')
        async def retire(process, result): result['local_transport'] = 'reaped'
        with patch.object(asyncio, 'create_subprocess_exec', AsyncMock(return_value=Process())):
            with self.assertRaisesRegex(RuntimeError, 'synthetic-callback-failure'):
                await instrumentation.observe(LocalOnly(), [], cleanup, 1, admissions,
                    retire=retire, on_output=fail)
        self.assertEqual(cleanup['local_transport'], 'reaped')
        self.assertEqual(admissions, {'submitted':None, 'observed':None})

    async def test_output_hook_never_receives_over_budget_chunk(self):
        class LocalOnly:
            def argv(self, *args): return ['synthetic']
        class Stream:
            async def read(self, size): return b'x' * (instrumentation.MAX_OUTPUT_BYTES + 1)
        class Process:
            stdout = Stream()
            returncode = 0
            async def wait(self): return 0
        seen, cleanup = [], {}
        async def retire(process, result): result['local_transport'] = 'reaped'
        with patch.object(asyncio, 'create_subprocess_exec', AsyncMock(return_value=Process())):
            with self.assertRaises(benchmark.Blocked):
                await instrumentation.observe(LocalOnly(), [], cleanup, 1, retire=retire, on_output=seen.append)
        self.assertFalse(seen)
        self.assertEqual(cleanup['local_transport'], 'reaped')

    async def drive_native_failure(self, raw, phase_two=None, absence_failure=False):
        import hashlib
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / 'synthetic.apk'
            apk.write_bytes(b'synthetic')
            args = SimpleNamespace(adb='/synthetic/adb', ssh=None, remote_adb=None, helper_apk=apk)
            class ADB:
                def __init__(self, *args):
                    pass
                def run(self, *args):
                    if args == ('devices', '-l'):
                        return 'synthetic device model:SM_A176B'
                    if args[:3] == ('shell', 'pm', 'path'):
                        return 'package:/synthetic/helper.apk'
                    if args[:2] == ('shell', 'sha256sum'):
                        return hashlib.sha256(b'synthetic').hexdigest() + ' file'
                    raise AssertionError(args)
                def argv(self, *args):
                    return args
            class Process:
                returncode = None
                def __init__(self, raw):
                    # Separate STATUS from FINAL so a throwing callback would
                    # prevent consumption of the helper's finally/close result.
                    split = raw.find(b'INSTRUMENTATION_RESULT:')
                    chunks = [raw[:split], raw[split:], b''] if split > 0 else [raw, b'']
                    class Stream:
                        async def read(self, count):
                            return chunks.pop(0) if chunks else b''
                    self.stdout = Stream()
                async def wait(self):
                    self.returncode = 0
                    return 0
                def terminate(self):
                    self.returncode = -15
                kill = terminate
            create = AsyncMock(side_effect=[Process(raw), Process(cleaned_output() if phase_two is None else phase_two)])
            report = {'cleanup':{}, 'checks':{}, 'admissions':{'submitted':0, 'observed':0},
                      'trial_completed':False, 'workflow_completed':False}
            with patch.object(benchmark, 'ADB', ADB), patch.object(benchmark, 'build_identity', return_value={}), \
                    patch.object(benchmark, 'select_a17', return_value='synthetic'), \
                    patch.object(phone_voice, 'process_absence', AsyncMock(side_effect=[None, None,
                        benchmark.Blocked('preexisting_target_process_refused')] if absence_failure else None)), \
                    patch.object(asyncio, 'create_subprocess_exec', create):
                with self.assertRaises(benchmark.Blocked):
                    await phone_voice.run(args, report, benchmark)
            self.assertFalse(report['workflow_completed'])
            self.assertNotIn('synthetic-private-payload', json.dumps(report))
            self.assertNotIn('00000000-0000-4000-8000-000000000000', json.dumps(report))
            return report, create

    async def test_resource_proof_stays_separate_when_process_survives(self):
        report, create = await self.drive_native_failure(output(), absence_failure=True)
        self.assertEqual(create.await_count, 1)
        self.assertEqual(report['checks']['native_resources_retirement'], 'pass')
        self.assertEqual(report['checks']['tts_retirement'], 'pass')
        self.assertEqual(report['cleanup']['native_resources_retirement'], 'verified')
        self.assertEqual(report['cleanup']['activity_webview_retirement'], 'pending')
        self.assertEqual(report['checks']['native_cleanup'], 'pending')
        self.assertNotEqual(report['cleanup'].get('native_retirement'), 'verified')
        self.assertNotIn('profile_cleanup', report['checks'])
        self.assertTrue(report['trial_completed'])
        self.assertEqual(report['admissions'], {'submitted':1, 'observed':1})

    async def test_failed_and_running_status_finish_native_cleanup_and_owned_profile_cleanup(self):
        for stage in ('failed', 'running'):
            with self.subTest(stage=stage):
                report, create = await self.drive_native_failure(native_failure(stage))
                self.assertEqual(create.await_count, 2)
                self.assertEqual(report['native_status']['stage'], stage)
                self.assertEqual(report['native_status']['generation'], 0)
                self.assertEqual(report['native_status']['FRONTEND_ASK_CLICKED_ms'], 12)
                self.assertEqual(report['native_final'], {'phase':'trial_1', 'result':'failed',
                    'failure_class':'IllegalStateException'})
                self.assertEqual(report['checks']['native_cleanup'], 'pass')
                self.assertEqual(report['checks']['profile_cleanup'], 'pass')
                self.assertFalse(report['trial_completed'])
                self.assertEqual(report['admissions'], {'submitted':None, 'observed':None})
                self.assertNotEqual(report['checks'].get('answer_correctness'), 'pass')
                self.assertNotIn('trial', report)

    async def test_failure_only_final_is_diagnostic_without_refunded_budget_or_cleanup_claim(self):
        report, create = await self.drive_native_failure(native_failure(status=False, retired=False))
        self.assertEqual(create.await_count, 1)
        self.assertEqual(report['native_final'], {'phase':'process_reservation', 'result':'failed',
            'failure_class':'ProcessReservationFailure'})
        self.assertEqual(report['admissions'], {'submitted':None, 'observed':None})
        self.assertNotIn('profile_cleanup', report['checks'])

    async def test_fixture_failure_without_status_can_cleanup_only_with_native_retirement_proof(self):
        raw = native_failure(status=False).replace(b'phase=process_reservation', b'phase=fixture')
        report, create = await self.drive_native_failure(raw)
        self.assertEqual(create.await_count, 2)
        self.assertEqual(report['native_final']['phase'], 'fixture')
        self.assertEqual(report['checks']['profile_cleanup'], 'pass')
        self.assertFalse(report['trial_completed'])
        self.assertNotIn('trial', report)
        self.assertEqual(report['admissions'], {'submitted':None, 'observed':None})

    async def test_complete_status_then_failed_final_keeps_known_one_without_observed_contract_claim(self):
        prefix = output().split(b'INSTRUMENTATION_RESULT:', 1)[0]
        failed = native_failure(status=False).replace(b'phase=process_reservation', b'phase=trial_1')
        report, create = await self.drive_native_failure(prefix + failed)
        self.assertEqual(create.await_count, 2)
        self.assertTrue(report['trial_completed'])
        self.assertEqual(report['admissions'], {'submitted':1, 'observed':1})
        self.assertNotIn('contract_sha256', report['trial'])
        self.assertNotEqual(report['checks'].get('answer_correctness'), 'pass')
        self.assertEqual(report['checks']['profile_cleanup'], 'pass')

    async def test_semantically_bad_complete_status_still_allows_proven_owned_cleanup(self):
        for raw in (output().replace(b'profile_mapping_verified=true', b'profile_mapping_verified=false'),
                    output().replace(b'frontend_ask_clicked=true', b'frontend_ask_clicked=false'),
                    output().replace(b'generation=1', b'generation=2')):
            with self.subTest(raw=raw[:30]):
                report, create = await self.drive_native_failure(raw)
                self.assertEqual(create.await_count, 2)
                self.assertEqual(report['checks']['profile_cleanup'], 'pass')
                self.assertFalse(report['trial_completed'])
                self.assertEqual(report['admissions'], {'submitted':None, 'observed':None})

    async def test_cleanup_final_failure_class_cannot_claim_success(self):
        failed_cleanup = cleaned_output().replace(b'INSTRUMENTATION_CODE:',
            b'INSTRUMENTATION_RESULT: failure_class=IllegalStateException\nINSTRUMENTATION_CODE:')
        report, create = await self.drive_native_failure(native_failure(), failed_cleanup)
        self.assertEqual(create.await_count, 2)
        self.assertNotIn('profile_cleanup', report['checks'])
        self.assertEqual(report['profile_final'], {'phase':'profile_cleanup', 'failure_class':'IllegalStateException',
                                                  'producer_exit_verified':True})

    async def test_phase_two_requires_exit_and_activity_proof_before_combined_cleanup(self):
        for key in ('producer_exit_verified', 'activity_webview_retired', 'storage_cleanup_verified'):
            for value in (None, 'false'):
                line = ('INSTRUMENTATION_RESULT: ' + key + '=true\n').encode()
                replacement = b'' if value is None else line.replace(b'=true', b'=false')
                with self.subTest(key=key, value=value):
                    report, create = await self.drive_native_failure(
                        native_failure(), cleaned_output().replace(line, replacement))
                    self.assertEqual(create.await_count, 2)
                    self.assertEqual(report['checks']['native_resources_retirement'], 'pass')
                    self.assertEqual(report['cleanup']['process_retirement'], 'verified')
                    self.assertEqual(report['cleanup']['activity_webview_retirement'], 'verified')
                    self.assertEqual(report['checks']['native_cleanup'], 'pending')
                    self.assertEqual(report['cleanup']['native_retirement'], 'pending')
                    self.assertNotIn('profile_cleanup', report['checks'])

    async def test_complete_status_survives_missing_malformed_or_hanging_final(self):
        import hashlib
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / 'synthetic.apk'
            apk.write_bytes(b'synthetic')
            args = SimpleNamespace(adb='/synthetic/adb', ssh=None, remote_adb=None, helper_apk=apk)
            class ADB:
                def __init__(self, *args):
                    pass
                def run(self, *args):
                    if args == ('devices', '-l'):
                        return 'synthetic device model:SM_A176B'
                    if args[:3] == ('shell', 'pm', 'path'):
                        return 'package:/synthetic/helper.apk'
                    if args[:2] == ('shell', 'sha256sum'):
                        return hashlib.sha256(b'synthetic').hexdigest() + ' file'
                    raise AssertionError(args)
                def argv(self, *args):
                    return ['synthetic']
            for ending in ('missing', 'malformed', 'hanging'):
                with self.subTest(ending=ending):
                    class Process:
                        returncode = None
                        def __init__(self):
                            self.stdout = asyncio.StreamReader()
                            prefix = output().split(b'INSTRUMENTATION_RESULT:', 1)[0]
                            self.stdout.feed_data(prefix)
                            if ending == 'malformed':
                                self.stdout.feed_data(b'INSTRUMENTATION_RESULT: synthetic-private-payload\n')
                            if ending != 'hanging':
                                self.stdout.feed_eof()
                        async def wait(self):
                            self.returncode = 0
                            return 0
                        def terminate(self):
                            self.returncode = -15
                            self.stdout.feed_eof()
                        kill = terminate
                    process = Process()
                    report = {'cleanup':{}, 'admissions':{'submitted':0, 'observed':0},
                              'checks':{}, 'trial_completed':False, 'workflow_completed':False}
                    create = AsyncMock(return_value=process)
                    with patch.object(benchmark, 'ADB', ADB), patch.object(benchmark, 'build_identity', return_value={}), \
                            patch.object(benchmark, 'select_a17', return_value='synthetic'), \
                            patch.object(phone_voice, 'process_absence', AsyncMock()), \
                            patch.object(wear_voice, 'HOST_WATCHDOG_SECONDS', .05), \
                            patch.object(asyncio, 'create_subprocess_exec', create):
                        with self.assertRaises((benchmark.Blocked, TimeoutError)):
                            await phone_voice.run(args, report, benchmark)
                    self.assertTrue(report['trial_completed'])
                    self.assertEqual(report['admissions'], {'submitted':1, 'observed':1})
                    self.assertFalse(report['workflow_completed'])
                    self.assertEqual(report['trial']['generation'], 1)
                    self.assertNotIn('contract_sha256', report['trial'])
                    for check in ('answer_correctness', 'canonical_ui_equality', 'native_cleanup', 'profile_cleanup'):
                        self.assertNotEqual(report['checks'].get(check), 'pass')
                    self.assertNotEqual(report['cleanup'].get('native_retirement'), 'verified')
                    self.assertNotIn('synthetic-private-payload', json.dumps(report))
                    self.assertEqual(create.await_count, 1)

    async def test_startup_consumes_single_observation_allowance(self):
        class LocalOnly:
            def argv(self, *args):
                return ['synthetic']
        class Process:
            returncode = 0
            async def wait(self):
                return 0
        process = Process()
        class Stream:
            async def read(self, size):
                await asyncio.sleep(.18)
                return b''
        process.stdout = Stream()
        async def create(*args, **kwargs):
            await asyncio.sleep(.18)
            return process
        cleanup = {}
        with patch.object(asyncio, 'create_subprocess_exec', create):
            with self.assertRaises(TimeoutError):
                await instrumentation.observe(LocalOnly(), [], cleanup, .25)
        self.assertEqual(cleanup['local_transport'], 'reaped')

    async def test_shared_transport_overflow_reaps_and_keeps_admission_unknown(self):
        class LocalOnly:
            def argv(self, *args):
                return [sys.executable, '-c', 'import os\nwhile True: os.write(1,b"x"*4096)']
        cleanup, admissions = {}, {'submitted':0, 'observed':0}
        with self.assertRaises(benchmark.Blocked):
            await instrumentation.observe(LocalOnly(), [], cleanup, 3, admissions)
        self.assertEqual(cleanup['local_transport'], 'reaped')
        self.assertEqual(admissions, {'submitted':None, 'observed':None})

    async def test_shared_transport_timeout_reaps_without_retry(self):
        class LocalOnly:
            def argv(self, *args):
                return [sys.executable, '-c', 'import time; time.sleep(60)']
        cleanup, admissions = {}, {}
        with self.assertRaises(TimeoutError):
            await instrumentation.observe(LocalOnly(), [], cleanup, .05, admissions)
        self.assertEqual(cleanup['local_transport'], 'reaped')
        self.assertEqual(admissions, {'submitted':None, 'observed':None})

    async def test_two_phase_cleanup_failure_preserves_admission(self):
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / 'synthetic.apk'
            apk.write_bytes(b'synthetic')
            args = benchmark.parser().parse_args(['--platform', 'android', '--workload', 'synthetic-voice',
                '--device', 'a17', '--apk', str(apk), '--helper-apk', str(apk), '--adb', '/synthetic/adb'])
            import hashlib
            class ADB:
                def __init__(self, *args):
                    pass
                def run(self, *args):
                    if args == ('devices', '-l'):
                        return 'synthetic device model:SM_A176B'
                    if args[:3] == ('shell', 'pm', 'path'):
                        return 'package:/synthetic/helper.apk'
                    if args[:2] == ('shell', 'sha256sum'):
                        return hashlib.sha256(b'synthetic').hexdigest() + ' file'
                    raise AssertionError(args)
                def argv(self, *args):
                    return args
            report = {'cleanup':{}, 'admissions':{'submitted':0, 'observed':0}, 'checks':{}}
            observe = AsyncMock(side_effect=[(output(), 0), benchmark.Blocked('cleanup_failed')])
            with patch.object(benchmark, 'ADB', ADB), patch.object(benchmark, 'build_identity', return_value={}), \
                    patch.object(benchmark, 'select_a17', return_value='synthetic'), \
                    patch.object(phone_voice, 'process_absence', AsyncMock()), \
                    patch.object(instrumentation, 'observe', observe):
                with self.assertRaises(benchmark.Blocked):
                    await phone_voice.run(args, report, benchmark)
            self.assertEqual(report['admissions'], {'submitted':1, 'observed':1})
            self.assertEqual(report['checks']['native_cleanup'], 'pending')
            self.assertNotIn('workflow_completed', report)
            self.assertNotIn('00000000-0000-4000-8000-000000000000', json.dumps(report))
            self.assertIn('cleanupToken', observe.call_args_list[1].args[1])
            # A complete pre-cleanup STATUS remains evidence even if native
            # retirement or its terminal code subsequently fails.
            report = {'cleanup':{}, 'admissions':{'submitted':0, 'observed':0}, 'checks':{}}
            failed = output().replace(b'native_resources_retired=true', b'native_resources_retired=false')
            failed = failed.replace(b'INSTRUMENTATION_CODE: -1', b'INSTRUMENTATION_CODE: 0')
            observe = AsyncMock(return_value=(failed, 0))
            with patch.object(benchmark, 'ADB', ADB), patch.object(benchmark, 'build_identity', return_value={}), \
                    patch.object(benchmark, 'select_a17', return_value='synthetic'), \
                    patch.object(phone_voice, 'process_absence', AsyncMock()), \
                    patch.object(instrumentation, 'observe', observe):
                with self.assertRaises(benchmark.Blocked):
                    await phone_voice.run(args, report, benchmark)
            self.assertEqual(report['admissions'], {'submitted':1, 'observed':1})
            self.assertTrue(report['trial_completed'])
            self.assertNotIn('native_cleanup', report['checks'])
            self.assertEqual(observe.await_count, 1)
            report = {'cleanup':{}, 'admissions':{'submitted':0, 'observed':0}, 'checks':{}}
            async def unreaped_profile(adb, arguments, cleanup, timeout, admissions=None, **kwargs):
                if admissions is not None:
                    cleanup['local_transport'] = 'reaped'
                    return output(), 0
                cleanup['local_transport'] = 'reap_unverified'
                return (b'INSTRUMENTATION_RESULT: synthetic_protocol=2\n'
                    b'INSTRUMENTATION_RESULT: benchmark_result=cleaned\n'
                    b'INSTRUMENTATION_RESULT: cleanup_verified=true\n'
                    b'INSTRUMENTATION_RESULT: profile_cleaned=true\n'
                    b'INSTRUMENTATION_RESULT: producer_exit_verified=true\n'
                    b'INSTRUMENTATION_RESULT: activity_webview_retired=true\n'
                    b'INSTRUMENTATION_RESULT: storage_cleanup_verified=true\n'
                    b'INSTRUMENTATION_RESULT: completed_trials=0\nINSTRUMENTATION_CODE: -1\n'), 0
            with patch.object(benchmark, 'ADB', ADB), patch.object(benchmark, 'build_identity', return_value={}), \
                    patch.object(benchmark, 'select_a17', return_value='synthetic'), \
                    patch.object(phone_voice, 'process_absence', AsyncMock()), \
                    patch.object(instrumentation, 'observe', unreaped_profile):
                with self.assertRaises(benchmark.Blocked):
                    await phone_voice.run(args, report, benchmark)
            self.assertEqual(report['cleanup']['native_retirement'], 'pending')
            self.assertEqual(report['cleanup']['profile_transport']['local_transport'], 'reap_unverified')
            self.assertNotIn('profile_cleanup', report['checks'])
            self.assertNotIn('workflow_completed', report)

    async def test_foreign_process_blocks_without_kill(self):
        class ADB:
            def run(self, *args):
                self.args = args
                return 'NAME\n' + benchmark.PACKAGE + ':foreign\n'
        adb = ADB()
        with self.assertRaises(benchmark.Blocked):
            await phone_voice.process_absence(adb)
        self.assertEqual(adb.args, ('shell', 'ps', '-A', '-o', 'NAME'))
