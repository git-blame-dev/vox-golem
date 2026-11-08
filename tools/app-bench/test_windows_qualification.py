"""Synthetic contract tests, never target-runtime qualification evidence."""
import copy
import hashlib
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from fixtures import arithmetic_profile
import runner
import windows_effects as windows
import windows_qualification as qualification
from test_typed_speech import config, descriptor
import test_windows_effects as effects_tests


def typed_report(binary, binding):
    checks = {name:True for name in ('canonical_turn_added_once', 'canonical_ui_answer_match',
        'canonical_sources_retained', 'no_dropped_observations', 'normal_admission_and_commit',
        'tool_reads_completed', 'completed_text_outcome', 'inflight_stream_presented',
        'fixture_answer_exact', 'request_contract_and_history', 'fixture_operations_exact',
        'bounded_stream_gate', 'quick_answer_correct')}
    return dict(schema=2, mode='offline', entry='typed', coverage='typed-workflow',
        diagnostic_only=False, workflow_identity=runner.typed_identity(),
        harness_sha256=runner.harness_identity(), binary_sha256=windows.file_hash(binary),
        cleanup=dict(app_exit_before_group_termination_observed=True, owned_job_retired=True),
        safety_checks={name:True for name in runner.WORKFLOW_SAFETY_CHECKS},
        rows=[dict(case=case, qualification_eligible=True, checks=checks.copy()) for case in runner.CASES],
        windows_profile=qualification.PROFILE, windows_binding=binding,
        private_storage_acl_verified=True, app_job_and_creation_identity_verified=True)


def speech_row(binding):
    checks = typed_report(Path(__file__), binding)['rows'][0]['checks']
    checks.update(profile_answer_exact=True, local_speech_completed_and_retired=True,
                  speech_identity_matches_descriptor=True, ui_sources_preserved=True)
    events = [dict(stage=stage, elapsed_us=index*1000, round=1) for index, stage in enumerate((
        'prompt_command_started', 'prompt_admitted', 'generation_dispatched', 'first_model_text',
        'first_model_word', 'canonical_turn_committed', 'reply_synthesis_requested',
        'reply_synthesis_valid', 'reply_output_submitted', 'reply_output_activated', 'reply_output_completed'))]
    return dict(case='arithmetic', passed=True, checks=checks, native_events=events,
        ui_submit_to_first_text_dom_ms=10, ui_submit_to_first_complete_word_dom_ms=11,
        ui_submit_to_speech_observation_ms=20,
        native_breakdown=runner.stage_breakdown(events),
        reply_speech=dict(admitted=1, synthesis_valid=1, output_submitted=1, output_completed=1, failed=0, retired=True),
        speech_identity=binding['speech_identity'])


class WindowsQualificationTests(unittest.TestCase):
    def test_live_effects_reserve_only_in_wsl_and_emit_a_reference_not_a_windows_allowance(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            manifest, value = effects_tests.WindowsEffectsTests().staged_bundle(root)
            value.update(schema=3, entry='typed-speech-qualified-windows', profile=qualification.PROFILE,
                harness_sha256=runner.windows_harness_identity(),
                wsl=dict(distribution='Synthetic-WSL', source_root='/synthetic/checkout'))
            manifest.write_text(json.dumps(value))
            auth = root/'canonical.toml'
            auth.write_text('synthetic canonical config not read')
            args = SimpleNamespace(entry='typed-speech', live=True, windows_profile='live-qualified-v1',
                cases=['arithmetic'], extended=False, windows_manifest=manifest,
                offline_evidence=root/'proof.json', auth_source_config=auth, _test_win32=True,
                output=root/'private-run', binary=root/value['roles']['binary'], config=root/'speech.toml', tools=root/'drivers')
            effects = windows.WindowsEffects(args)
            identity = windows.neural_descriptor(descriptor(), 'synthetic-a')
            def private(path, create=True):
                if create: path.mkdir()
            with patch.object(windows, 'private_directory', side_effect=private), \
                    patch.object(windows, 'verify_native_driver_version', return_value=value['driver_versions']['edge']), \
                    patch.object(windows, 'probe_neural_service', return_value=identity), \
                    patch.object(qualification, 'speech_evidence') as gate, \
                    patch.object(windows.subprocess, 'run', return_value=SimpleNamespace(returncode=0, stdout=b'{"reserved":24}')) as wsl, \
                    patch.dict(windows.os.environ, {'SystemRoot':str(root/'SyntheticWindows')}, clear=True), \
                    patch.object(runner, 'reserve_live_budget', side_effect=AssertionError('no Windows ledger')):
                effects.prepare(arithmetic_profile(), runner)
                env = effects.environment(None, runner)
            gate.assert_called_once()
            command = wsl.call_args.args[0]
            self.assertEqual(command[1:6], ['--distribution', 'Synthetic-WSL', '--exec', '/usr/bin/python3', '-I'])
            self.assertIn('reserve', command)
            handoff = json.loads((args.output/'reservation.json').read_text())
            self.assertEqual(handoff['schema'], 2)
            self.assertNotIn('allowance', handoff)
            self.assertEqual(handoff['binding'], effects.binding)
            self.assertEqual(effects.assignment_reserved, 24)
            self.assertEqual(env['VOXGOLEM_BENCH_ENTRY'], 'typed-speech-qualified-windows')
            self.assertEqual(env['VOXGOLEM_BENCH_AUTH_SOURCE_CONFIG'], str(auth))
            self.assertEqual(env['VOXGOLEM_BENCH_PERMITS'], str(args.output/'reservation.json'))
            args.auth_source_config = root/'missing-canonical.toml'
            with patch.object(windows.subprocess, 'run', side_effect=AssertionError('invalid reference must not reserve')) as denied, \
                    patch.dict(windows.os.environ, {'SystemRoot':str(root/'SyntheticWindows')}, clear=True):
                with self.assertRaises(ValueError): effects.environment(None, runner)
                denied.assert_not_called()

    def test_schema_four_missing_matrix_changed_identity_and_incomplete_audio_never_qualify(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary, manifest, typed, speech = [root/name for name in ('app.exe', 'manifest.json', 'typed.json', 'speech.json')]
            binary.write_bytes(b'synthetic')
            manifest.write_text('{}')
            args = SimpleNamespace(binary=binary, windows_manifest=manifest)
            identity = windows.neural_descriptor(descriptor(), 'synthetic-a')
            binding = qualification.binding(args, runner, arithmetic_profile(), identity)
            matrix = typed_report(binary, binding)
            typed.write_text(json.dumps(matrix))
            value = dict(schema=5, mode='offline', entry='typed-speech-qualified-windows',
                windows_profile=qualification.PROFILE, windows_binding=binding, diagnostic_only=False,
                qualified_for_live=True, offline_profile_qualified=True, passed=True,
                workflow_completed=True, live_calls=0, private_storage_acl_verified=True,
                app_job_and_creation_identity_verified=True, cleanup=matrix['cleanup'],
                rows=[speech_row(binding)], typed_evidence_path=str(typed), typed_evidence_sha256=windows.file_hash(typed))
            def validate(payload):
                speech.write_text(json.dumps(payload))
                return qualification.speech_evidence(speech, binding, runner, binary)
            validate(value)
            changed = copy.deepcopy(value)
            changed['rows'][0]['native_breakdown']['prompt_to_first_model_word_ms'] += 1
            with self.assertRaises(ValueError): validate(changed)
            for update in ({'schema':4}, {'schema':True}, {'diagnostic_only':True}, {'live_calls':True},
                           {'windows_binding':dict(binding, harness_sha256='0'*64)},
                           {'rows':[]}, {'cleanup':{'owned_job_retired':True}}):
                with self.subTest(update=update), self.assertRaises(ValueError): validate(dict(value, **update))
            for name in ('profile_answer_exact', 'request_contract_and_history', 'local_speech_completed_and_retired'):
                changed = copy.deepcopy(value)
                del changed['rows'][0]['checks'][name]
                with self.assertRaises(ValueError): validate(changed)
            for key in ('ui_submit_to_first_text_dom_ms', 'ui_submit_to_first_complete_word_dom_ms', 'ui_submit_to_speech_observation_ms'):
                for invalid in (None, True, -1, float('inf')):
                    changed = copy.deepcopy(value)
                    changed['rows'][0][key] = invalid
                    with self.assertRaises(ValueError): validate(changed)
            for name in runner.WORKFLOW_SAFETY_CHECKS:
                changed = copy.deepcopy(matrix)
                changed['safety_checks'][name] = False
                typed.write_text(json.dumps(changed))
                with self.assertRaises(ValueError): validate(value)
            typed.write_text(json.dumps(matrix))
            binary.write_bytes(b'changed synthetic binary')
            with self.assertRaises(ValueError): validate(value)

    def test_separate_live_auth_reference_is_never_read_or_copied_by_controller(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = config(root/'synthetic-neural.toml')
            auth_source = root/'canonical-config.toml'
            auth_source.write_text('synthetic private config sentinel')
            run = root/'run'
            run.mkdir()
            original = Path.read_text
            def read(path, *args, **kwargs):
                if path == auth_source: raise AssertionError('controller must not read canonical config')
                return original(path, *args, **kwargs)
            with patch.object(Path, 'read_text', read):
                env = runner.isolated_environment(run, source, None, live=True,
                    reservation=run/'reservation.json', entry='typed-speech',
                    auth_source_config=auth_source, windows_qualified=True, base_environment={})
            self.assertEqual(env['VOXGOLEM_BENCH_AUTH_SOURCE_CONFIG'], str(auth_source))
            self.assertEqual(env['VOXGOLEM_BENCH_MODE'], 'live')
            self.assertFalse(runner.managed_credential(run/'config').exists())
            self.assertNotIn('sentinel', (run/'config/config.toml').read_text())
            self.assertNotIn('auth_path', (run/'config/config.toml').read_text())

    def test_explicit_qualified_manifest_and_controls_preserve_legacy_rejection(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path, value = effects_tests.WindowsEffectsTests().staged_bundle(root)
            value.update(schema=3, entry='typed-speech-qualified-windows', profile=qualification.PROFILE,
                harness_sha256=runner.windows_harness_identity(),
                wsl=dict(distribution='Synthetic-WSL', source_root='/synthetic/checkout'))
            path.write_text(json.dumps(value))
            windows.load_manifest(path, arithmetic_profile(), qualified=True)
            with self.assertRaises(ValueError): windows.load_manifest(path, arithmetic_profile())
            value['harness_sha256'] = '0'*64
            path.write_text(json.dumps(value))
            with self.assertRaises(ValueError): windows.load_manifest(path, arithmetic_profile(), qualified=True)
            args = dict(entry='typed-speech', live=True, windows_profile='live-qualified-v1',
                cases=['arithmetic'], extended=False, windows_manifest=path,
                offline_evidence=root/'proof.json', auth_source_config=root/'canonical.toml')
            windows.validate_options(SimpleNamespace(**args), 'win32')
            for update in ({'windows_profile':'diagnostic'}, {'offline_evidence':None},
                           {'auth_source_config':None}, {'ledger':root/'independent.json'}, {'cases':['quick']}):
                with self.assertRaises(ValueError): windows.validate_options(SimpleNamespace(**dict(args, **update)), 'win32')
