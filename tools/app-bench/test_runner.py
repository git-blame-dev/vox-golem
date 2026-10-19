import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import runner


class SafetyTests(unittest.TestCase):
    def test_only_explicitly_deferred_checks_receive_deferred_labels(self):
        rows = [{'checks': {'canonical_sources_retained': False, 'ui_sources_preserved': False,
                            'inflight_stream_presented': False}}]
        deferred, failed = runner.finding_dispositions(rows, {'canonical_history_retained': False},
                                                       {'cancellation_retirement': False})
        self.assertEqual(deferred, ['desktop_history_relaunch', 'ui_source_projection'])
        self.assertEqual(failed, ['canonical_sources_retained', 'inflight_stream_presented', 'safety:cancellation_retirement'])

    def test_cancellation_rechecks_late_completion_after_release(self):
        class Driver:
            def find_element(self, *args): return self
            def find_elements(self, *args): return [self]
            def click(self): pass
        class Service:
            released = False
            def expect_history(self, turns): pass
            def snapshot(self): return {'first_delta_sent': True, 'generation_requests': 1}
            def release_stream(self): self.released = True
            def wait_quiescent(self): return True
        for late_change in [False, True]:
            service = Service()
            generations = 0
            def observe(driver, command, args=None):
                nonlocal generations
                if command == 'benchmark_snapshot':
                    count = generations
                    generations = 1
                    return {'generation': {'generation_attempts': count}}
                if command == 'get_app_snapshot':
                    return {'transcript': [{'synthetic': 'late committed turn'}] if late_change and service.released else [],
                            'active_request': None, 'runtime_phase': 'sleeping'}
                return None
            with patch.object(runner, 'invoke', observe), patch.object(runner, 'fill_composer'), \
                    patch.object(runner, 'CANCEL_POST_RELEASE_SECONDS', 0.01):
                self.assertEqual(runner.typed_cancellation(Driver(), service), not late_change)
            self.assertTrue(service.released)

    def test_typed_profile_defers_display_sources_but_never_canonical_or_stream_integrity(self):
        checks = {'canonical_turn_added_once': True, 'no_dropped_observations': True,
                  'normal_admission_and_commit': True, 'tool_reads_completed': True,
                  'completed_text_outcome': True, 'canonical_ui_answer_match': True, 'canonical_sources_retained': True,
                  'ui_sources_preserved': False, 'inflight_stream_presented': True,
                  'fixture_operations_exact': True}
        self.assertEqual(runner.typed_eligibility(checks), (True, True))
        self.assertEqual(runner.typed_eligibility({**checks, 'canonical_sources_retained': False}), (False, False))
        self.assertEqual(runner.typed_eligibility({**checks, 'fixture_operations_exact': False}), (False, False))
        self.assertEqual(runner.typed_eligibility({**checks, 'inflight_stream_presented': False}), (True, False))
        self.assertEqual(runner.typed_eligibility({}), (False, False))

    def test_typed_live_gate_requires_profile_identity_all_cases_and_real_safety_checks(self):
        import hashlib
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            binary = root/'synthetic-binary'
            binary.write_bytes(b'synthetic typed benchmark executable')
            report = root/'report.json'
            safety = {name: True for name in ['empty_admission', 'duplicate_ui_submission',
                'cancellation_retirement', 'warm_context', 'longer_history', 'relaunch_preferences',
                'relaunch_without_generation']}
            value = {'schema': 2, 'mode': 'offline', 'entry': 'typed', 'coverage': 'typed-workflow',
                'diagnostic_only': False, 'workflow_identity': runner.typed_identity(),
                'harness_sha256': runner.harness_identity(),
                'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                'cleanup': {'app_exit_before_group_termination_observed': True},
                'safety_checks': safety, 'rows': [{'case': case, 'qualification_eligible': True,
                    'checks': {name: True for name in ['canonical_turn_added_once', 'canonical_ui_answer_match',
                        'canonical_sources_retained', 'no_dropped_observations', 'normal_admission_and_commit',
                        'tool_reads_completed', 'completed_text_outcome', 'inflight_stream_presented',
                        'fixture_answer_exact', 'request_contract_and_history', 'fixture_operations_exact',
                        'bounded_stream_gate', 'quick_answer_correct']}}
                    for case in runner.CASES]}
            report.write_text(json.dumps(value))
            runner.validate_offline_evidence(report, binary, {}, {}, 'typed')
            for change in [{'entry': 'typed-control'}, {'coverage': 'voice'}, {'mode': 'live'},
                           {'workflow_identity': {}}, {'diagnostic_only': True},
                           {'safety_checks': {'passed': True}}, {'safety_checks': {**safety, 'cancellation_retirement': False}},
                           {'rows': value['rows'][:-1]},
                           {'rows': [{'case': case, 'qualification_eligible': True} for case in runner.CASES]},
                           {'rows': [{'case': 'quick', 'qualification_eligible': True}]*3},
                           {'rows': [{'case': case, 'qualification_eligible': False} for case in runner.CASES]}]:
                report.write_text(json.dumps({**value, **change}))
                with self.assertRaises(ValueError): runner.validate_offline_evidence(report, binary, {}, {}, 'typed')
            for name in ['fixture_answer_exact', 'request_contract_and_history', 'fixture_operations_exact',
                         'bounded_stream_gate', 'quick_answer_correct']:
                for missing in [True, False]:
                    altered = json.loads(json.dumps(value))
                    if missing: del altered['rows'][0]['checks'][name]
                    else: altered['rows'][0]['checks'][name] = False
                    report.write_text(json.dumps(altered))
                    with self.assertRaises(ValueError): runner.validate_offline_evidence(report, binary, {}, {}, 'typed')
            report.write_text(json.dumps(value))
            with self.assertRaises(ValueError): runner.validate_offline_evidence(report, binary, {}, {})

    def test_native_stage_intervals_use_matching_boundaries_and_never_sum_overlap(self):
        events = [{'stage': stage, 'elapsed_us': at, 'round': turn} for stage, at, turn in [
            ('prompt_command_started', 1000, 0), ('prompt_admitted', 1100, 0),
            ('generation_dispatched', 2000, 1), ('search_started', 5000, 1),
            ('tool_completed', 8000, 1), ('generation_dispatched', 9000, 2),
            ('first_model_text', 12000, 2), ('provider_run_completed', 15000, 2),
            ('canonical_turn_committed', 16000, 2), ('command_result_ready', 17000, 2)]]
        breakdown = runner.stage_breakdown(events)
        self.assertEqual(breakdown['native_admission_ms'], 0.1)
        self.assertEqual(breakdown['admission_to_dispatch_ms'], 0.9)
        self.assertEqual(breakdown['provider_and_tools_ms'], 13)
        self.assertEqual(breakdown['tools'][0]['duration_ms'], 3)
        self.assertEqual(breakdown['model_rounds'][1]['dispatch_to_first_text_ms'], 3)
        self.assertIsNone(breakdown['model_rounds'][0]['dispatch_to_first_text_ms'])

    def test_typed_configuration_does_not_require_or_load_speech_assets(self):
        import tomllib
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            run = root/'run'; run.mkdir(mode=0o700)
            source = root/'source.toml'; source.write_text('')
            env = runner.isolated_environment(run, source, 'http://127.0.0.1:12345/responses', entry='typed')
            config = tomllib.loads(Path(env['VOXGOLEM_CONFIG_PATH']).read_text())
            self.assertNotIn('wake_word_model_path', config)
            self.assertNotIn('parakeet_model_dir', config)
            self.assertNotIn('silero_vad_model', config)
            self.assertEqual(env['VOXGOLEM_BENCH_ENTRY'], 'typed')

    def test_live_answer_heading_does_not_hide_the_final_persisted_footer(self):
        body = 'Synthetic sourced answer.\n\nSources:\nA model-provided explanatory section.'
        canonical = body+'\n\nSources:\n'+'\n'.join(runner.URLS)+'\n'
        integrity = runner.answer_integrity('long_research', canonical, canonical, runner.URLS, False)
        self.assertTrue(integrity['canonical_ui_match'])
        self.assertTrue(integrity['sources_preserved'])
        missing = body+'\n\nSources:\n'+runner.URLS[0]+'\n'
        self.assertFalse(runner.answer_integrity('long_research', missing, missing, runner.URLS, False)['sources_preserved'])
        structured = runner.answer_integrity(
            'long_research',
            body,
            body,
            runner.URLS,
            False,
            canonical_sources=[{'title': 'Doc', 'url': u} for u in runner.URLS],
        )
        self.assertTrue(structured['canonical_ui_match'])
        self.assertTrue(structured['canonical_sources_expected'])
        self.assertTrue(structured['sources_preserved'])

    def test_quick_question_does_not_require_unspecified_answer_only_format(self):
        for answer in ['42', '17 plus 25 equals 42.', 'The answer is 42.']:
            self.assertTrue(runner.quick_answer_correct(answer))
        for answer in ['', '41', '142', 'synthetic verbose text '*20+'42']:
            self.assertFalse(runner.quick_answer_correct(answer))

    def test_delivered_answer_integrity_rejects_text_or_source_loss(self):
        case = 'long_research'
        answer = runner.expected_answer(case)
        canonical = answer+'\n\nSources:\n'+'\n'.join(runner.URLS)+'\n'
        good = runner.answer_integrity(case, canonical, canonical, runner.URLS, True)
        self.assertTrue(good['canonical_ui_match'])
        self.assertTrue(good['sources_preserved'])
        self.assertTrue(good['offline_fixture_match'])
        canonical_sources = [{'title': 'Doc', 'url': url} for url in runner.URLS]
        good_structured = runner.answer_integrity(case, answer, answer, runner.URLS, True, canonical_sources)
        self.assertTrue(good_structured['canonical_ui_match'])
        self.assertTrue(good_structured['sources_preserved'])
        self.assertTrue(good_structured['offline_fixture_match'])
        self.assertFalse(runner.answer_integrity(case, answer, answer, runner.URLS, True, canonical_sources[:1])['canonical_sources_expected'])
        self.assertFalse(runner.answer_integrity(case, canonical, 'Synthetic replaced answer', runner.URLS, True)['canonical_ui_match'])
        self.assertFalse(runner.answer_integrity(case, 'Synthetic replaced answer', 'Synthetic replaced answer', runner.URLS, True)['offline_fixture_match'])
        self.assertFalse(runner.answer_integrity(case, answer[:80], answer[:80], [], True)['offline_fixture_match'])
        for lost in [runner.URLS[:1], runner.URLS[1:], []]:
            self.assertFalse(runner.answer_integrity(case, canonical, canonical, lost, True)['sources_preserved'])
            truncated = answer+'\n\nSources:\n'+'\n'.join(lost)+'\n'
            self.assertFalse(runner.answer_integrity(case, truncated, truncated, lost, True)['sources_preserved'])
    def test_original_default_credential_location_survives_private_gui_home(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            original = root/'original'
            private = root/'private'
            auth = original/'.local/share/opencode/auth.json'
            auth.parent.mkdir(parents=True)
            auth.write_text('{}')
            private.mkdir()
            with patch.dict(os.environ, {'HOME': str(private),
                    'VOXGOLEM_BENCH_SOURCE_HOME': str(original),
                    'VOXGOLEM_BENCH_SOURCE_DATA_HOME': ''}):
                selected, _ = runner.selected_live_authority(root/'config.toml', {'custom_openai': {}})
                self.assertEqual(selected, auth)

    def test_typed_control_can_only_prove_the_non_voice_path(self):
        stages = ['generation_dispatched']*3
        native = {'events': [{'flow': 0, 'stage': stage} for stage in stages],
                  'dropped': 0, 'readable_pages': 2, 'tool_failures': 0}
        service = {'failures': 0, 'transcript_matches': True, 'model_matches': True,
            'terminal': True, 'generation_requests': 3, 'expected_model_rounds': 3,
            'searches': 1, 'expected_searches': 1, 'fetches': 2, 'expected_fetches': 2,
            'readable_sources': 2, 'expected_readable_sources': 2}
        final = {'nonblank': True, 'canonical_ui_match': True, 'offline_fixture_match': True}
        self.assertTrue(runner.accepted_flow('long_research', final, native, True, True, True, service, False))
        self.assertFalse(runner.accepted_flow('long_research', final, native, True, True, True, service))

    def test_private_session_requires_changed_pid_namespace_and_new_bus(self):
        runner.validate_private_session('pid:[101]', 'pid:[102]', '', 'synthetic-private-bus')
        for values in [(None, 'pid:[102]', '', 'synthetic-private-bus'),
                       ('pid:[101]', 'pid:[101]', '', 'synthetic-private-bus'),
                       ('pid:[101]', 'pid:[102]', None, 'synthetic-private-bus'),
                       ('pid:[101]', 'pid:[102]', 'synthetic-old-bus', 'synthetic-old-bus')]:
            with self.assertRaises(ValueError): runner.validate_private_session(*values)

    def test_normal_binary_is_rejected_before_any_application_launch(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            binary = Path(temp)/'synthetic-binary'
            binary.write_bytes(b'synthetic normal executable without benchmark capability')
            with self.assertRaises(ValueError):
                runner.validate_benchmark_binary(binary)
            binary.write_bytes(b'synthetic feature-enabled executable '+runner.CAPABILITY)
            runner.validate_benchmark_binary(binary)

    def test_report_retention_prunes_only_marked_metadata_with_age_and_space_limits(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            for name, created in [('old', 1), ('first', 100*86400-2), ('second', 100*86400-1), ('latest', 100*86400)]:
                directory = root/name
                directory.mkdir()
                (directory/'.app-bench-run.json').write_text(json.dumps({
                    'schema': 1, 'owner': 'voxgolem-app-bench', 'created_epoch_seconds': created}))
                (directory/'report.json').write_bytes(b'{}')
                (directory/'fixture.pcm').write_bytes(b'synthetic fixture')
            foreign = root/'unowned'
            foreign.mkdir()
            (foreign/'report.json').write_bytes(b'preserved user artifact')
            runner.prune_owned_reports(root, now=100*86400, max_files=2, max_bytes=4)
            self.assertFalse((root/'old'/'report.json').exists())
            self.assertFalse((root/'first'/'report.json').exists())
            self.assertTrue((root/'second'/'report.json').exists())
            self.assertTrue((root/'latest'/'report.json').exists())
            self.assertEqual((foreign/'report.json').read_bytes(), b'preserved user artifact')
            self.assertEqual((root/'old'/'fixture.pcm').read_bytes(), b'synthetic fixture')

    def test_live_authority_never_falls_back_from_a_broken_managed_selection(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            source = root/'config.toml'
            legacy = root/'synthetic-auth.json'
            legacy.write_text('{}')
            config = {'custom_openai': {'auth_path': str(legacy), 'auth_source': 'native'}}
            self.assertEqual(runner.selected_live_authority(source, config)[0], legacy)
            managed = root/'chatgpt'/'chatgpt-auth.json'
            managed.parent.mkdir()
            managed.symlink_to(root/'synthetic-missing')
            with self.assertRaises(ValueError):
                runner.selected_live_authority(source, config)
            managed.unlink()
            managed.write_text('{"synthetic":"signed-out marker"}')
            self.assertEqual(runner.selected_live_authority(source, config)[0], managed)
            managed.unlink()
            with self.assertRaises(ValueError):
                runner.selected_live_authority(source, {})

    def test_research_acceptance_requires_real_read_evidence_exact_calls_and_streaming(self):
        stages = ['source_samples_completed', 'wake_detected', 'final_asr_started',
                  'final_asr_completed'] + ['generation_dispatched']*3
        native = {'events': [{'flow': 1, 'stage': stage} for stage in stages],
                  'dropped': 0, 'readable_pages': 2, 'tool_failures': 0}
        service = {'failures': 0, 'transcript_matches': True, 'model_matches': True,
            'terminal': True, 'generation_requests': 3, 'expected_model_rounds': 3,
            'searches': 1, 'expected_searches': 1, 'fetches': 2, 'expected_fetches': 2,
            'readable_sources': 2, 'expected_readable_sources': 2}
        final = {'nonblank': True, 'canonical_ui_match': True, 'offline_fixture_match': True}
        self.assertTrue(runner.accepted_flow('long_research', final, native, True, True, True, service))
        self.assertFalse(runner.accepted_flow('long_research', {}, native, True, True, True, service))
        for change in [{'readable_pages': 0}, {'readable_pages': 1},
                       {'tool_failures': 1}, {'dropped': 1}]:
            self.assertFalse(runner.accepted_flow('long_research', final, {**native, **change}, True, True, True, service))
        self.assertFalse(runner.accepted_flow('long_research', final, native, True, False, True, service))
        self.assertFalse(runner.accepted_flow('long_research', final, native, True, True, False, service),
                         'terminal-only text must not prove incremental streaming')
        self.assertFalse(runner.accepted_flow('long_research', final, native, True, True, True,
                                             {**service, 'generation_requests': 4}))

    def test_offline_namespace_marker_and_loopback_are_required(self):
        runner.validate_offline_namespace('net:[101]', 'net:[102]', ['lo'])
        for original, current, interfaces in [
            (None, 'net:[102]', ['lo']), ('invalid', 'net:[102]', ['lo']),
            ('net:[101]', 'net:[101]', ['lo']), ('net:[101]', 'net:[102]', ['lo', 'eth0']),
        ]:
            with self.assertRaises(ValueError):
                runner.validate_offline_namespace(original, current, interfaces)

    def test_directory_sync_failure_prevents_a_spendable_reservation(self):
        import stat
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            run = root/'run'
            run.mkdir(mode=0o700)
            fsync = os.fsync
            calls = []
            def observed_sync(descriptor):
                directory = stat.S_ISDIR(os.fstat(descriptor).st_mode)
                calls.append('directory' if directory else 'file')
                if directory:
                    self.assertFalse((run/'reservation.json').exists())
                    raise OSError('Synthetic directory durability failure')
                fsync(descriptor)
            with patch.object(os, 'fsync', observed_sync):
                with self.assertRaises(OSError):
                    runner.reserve_live_budget(root/'ledger.json', run/'reservation.json')
            self.assertEqual(calls, ['file', 'directory'])
            self.assertFalse((run/'reservation.json').exists())

    def test_live_gate_requires_matching_successful_offline_flow_evidence(self):
        import hashlib
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            binary = root/'synthetic-binary'
            binary.write_bytes(b'synthetic executable identity')
            report = root/'report.json'
            fixtures = {'quick': {'sha256': 'synthetic-audio-identity'}}
            speech = {'asset_sha256': {'wake': 'synthetic-model-identity'}}
            evidence = {'schema': 1, 'mode': 'offline', 'entry': 'voice', 'diagnostic_only': False,
                'wake_score_diagnostics': False, 'browser_call_probes': False,
                'fixture_metadata': fixtures, 'speech_identity': speech,
                'cleanup': {'app_exit_before_group_termination_observed': True},
                'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                'harness_sha256': runner.harness_identity(),
                'rows': [{'case': case, 'passed': True, 'dropped': 0} for case in runner.CASES]}
            report.write_text(json.dumps(evidence))
            runner.validate_offline_evidence(report, binary, fixtures, speech)
            for change in [{'mode': 'live'}, {'entry': 'typed-control'}, {'diagnostic_only': True},
                           {'wake_score_diagnostics': True}, {'browser_call_probes': True},
                           {'wake_score_diagnostics': None}, {'browser_call_probes': None},
                           {'fixture_metadata': {}}, {'speech_identity': {}},
                           {'cleanup': {'app_exit_before_group_termination_observed': False}},
                           {'binary_sha256': 'synthetic-mismatch'},
                           {'harness_sha256': 'synthetic-mismatch'},
                           {'rows': evidence['rows'][:-1]},
                           {'rows': [{'case': 'quick', 'passed': False, 'dropped': 0}]}]:
                report.write_text(json.dumps({**evidence, **change}))
                with self.assertRaises(ValueError):
                    runner.validate_offline_evidence(report, binary, fixtures, speech)
            for missing in ['wake_score_diagnostics', 'browser_call_probes']:
                incomplete = {**evidence}
                del incomplete[missing]
                report.write_text(json.dumps(incomplete))
                with self.assertRaises(ValueError):
                    runner.validate_offline_evidence(report, binary, fixtures, speech)

    def test_reservations_are_durable_and_cannot_exceed_the_assignment_cap(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            ledger = root/'ledger.json'
            for index in range(8):
                run = root/f'run-{index}'
                run.mkdir(mode=0o700)
                self.assertEqual(runner.reserve_live_budget(ledger, run/'reservation.json'), (index+1)*12)
                self.assertEqual(json.loads((run/'reservation.json').read_text()), {'schema': 1, 'allowance': 12})
            run = root/'last'
            run.mkdir()
            with self.assertRaises(ValueError):
                runner.reserve_live_budget(ledger, run/'reservation.json')
            self.assertEqual(json.loads(ledger.read_text())['reserved'], 96)

    def test_corrupt_and_disposable_or_symlinked_ledgers_block(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            run = root/'run'
            run.mkdir()
            with self.assertRaises(ValueError):
                runner.reserve_live_budget(run/'ledger.json', run/'reservation.json')
            ledger = root/'ledger.json'
            ledger.write_text('invalid')
            with self.assertRaises(ValueError):
                runner.reserve_live_budget(ledger, run/'reservation.json')
            linked = root/'linked.json'
            linked.symlink_to(ledger)
            with self.assertRaises(ValueError):
                runner.reserve_live_budget(linked, run/'reservation.json')

    def test_offline_configuration_uses_only_private_state_and_synthetic_auth(self):
        with tempfile.TemporaryDirectory(dir='/tmp/opencode') as temp:
            root = Path(temp)
            run = root/'run'
            run.mkdir(mode=0o700)
            config = root/'source.toml'
            lines = []
            for key in ['wake_word_model_path', 'parakeet_model_dir', 'silero_vad_model']:
                asset = root/key
                asset.touch()
                lines.append(f'{key} = {json.dumps(str(asset))}')
            config.write_text('\n'.join(lines))
            env = runner.isolated_environment(run, config, 'http://127.0.0.1:12345/responses')
            for key in ['HOME', 'XDG_CONFIG_HOME', 'XDG_DATA_HOME', 'XDG_CACHE_HOME', 'XDG_STATE_HOME']:
                self.assertTrue(Path(env[key]).is_relative_to(run))
                self.assertNotEqual(env[key], os.environ.get(key))
            self.assertEqual(env['VOXGOLEM_BENCH_MODE'], 'offline')
            self.assertEqual(json.loads((run/'synthetic-auth.json').read_text())['openai']['access'], 'synthetic-benchmark-access')


if __name__ == '__main__':
    unittest.main()
