from pathlib import Path
import tempfile
import unittest
import tomllib
import contextlib
import io
import json
import sys
from types import SimpleNamespace
from unittest.mock import patch

import fixtures
import runner
from test_fixtures import request, events


class ArithmeticProfileTests(unittest.TestCase):
    def test_readiness_diagnostic_rejects_live_windows_and_long_flows_before_execution(self):
        base = ['runner.py', '--binary', 'synthetic-app', '--tools', 'synthetic-tools',
                '--config', 'synthetic-config', '--output', 'synthetic-output',
                '--entry', 'voice-arithmetic', '--tts-model', 'synthetic-model',
                '--wake-phrase', 'synthetic wake', '--readiness-diagnostic']
        for flags in (['--flow-timeout', '180'], ['--flow-timeout', '30', '--live'],
                      ['--flow-timeout', '30', '--desktop-platform', 'windows']):
            with patch.object(sys, 'argv', base+flags), patch.object(runner, 'execute') as execute, \
                    contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    runner.main()
                self.assertEqual(error.exception.code, 2)
                execute.assert_not_called()
    def test_model_readiness_requires_canonical_and_enabled_ui_not_just_selected_value(self):
        snapshot = {'preferences': {'instant': 'custom-luna-none'},
                    'model_options': [{'id': 'custom-luna-none', 'available': True}]}
        for enabled in (False, True):
            driver = SimpleNamespace(execute_script=lambda *args: {'selected': True, 'enabled': enabled})
            with patch.object(runner, 'invoke', return_value=snapshot):
                evidence = runner.voice_model_readiness(driver, timeout=0)
            self.assertEqual(evidence['ready'], enabled)
            self.assertTrue(evidence['canonical_model_available'])
        snapshot['model_options'][0]['available'] = False
        driver = SimpleNamespace(execute_script=lambda *args: self.fail('Unavailable model must not receive UI actions'))
        with patch.object(runner, 'invoke', return_value=snapshot):
            self.assertEqual(runner.voice_model_readiness(driver, timeout=0)['failure'], 'requested_model_not_configured')
    def test_internal_dispatch_cannot_reserve_legacy_budget(self):
        args = SimpleNamespace(entry='voice-arithmetic', live=True, cases=['arithmetic'],
                               tts_model=Path('synthetic.onnx'), binary=Path('synthetic-app'))
        with patch.object(runner, 'validate_benchmark_binary', side_effect=AssertionError('unsafe live preflight reached')) as validate:
            with self.assertRaises(ValueError):
                runner.execute(args)
            validate.assert_not_called()

    def test_contract_profile_is_separate_and_strict(self):
        profile = fixtures.arithmetic_profile()
        self.assertEqual(set(profile.cases), {'arithmetic'})
        prompt = profile.cases['arithmetic']
        self.assertTrue(profile.matches('arithmetic', prompt))
        self.assertFalse(profile.matches('arithmetic', prompt + ' extra'))
        self.assertFalse(profile.matches('quick', prompt))
        self.assertEqual(fixtures.expected_answer('quick'), '42')
        self.assertEqual(len(fixtures.CASES), 3)

    def test_arithmetic_live_and_foreign_evidence_fail_closed(self):
        with self.assertRaises(ValueError):
            runner.validate_arithmetic_options(True, ['arithmetic'], Path('model.onnx'))
        with self.assertRaises(ValueError):
            runner.validate_arithmetic_options(False, ['quick'], Path('model.onnx'))
        with self.assertRaises(ValueError):
            runner.validate_arithmetic_options(False, ['arithmetic'], None)
        with tempfile.TemporaryDirectory() as temporary:
            report = Path(temporary)/'report.json'
            report.write_text('{"schema":3,"entry":"voice-arithmetic","mode":"offline"}')
            binary = Path(temporary)/'binary'
            binary.write_bytes(b'synthetic')
            for entry in ('voice', 'typed'):
                with self.assertRaises(ValueError):
                    runner.validate_offline_evidence(report, binary, {}, {}, entry)
            report.write_text('{"schema":1,"entry":"voice","mode":"offline"}')
            with self.assertRaises(ValueError):
                runner.validate_offline_evidence(report, binary, {}, {}, 'voice-arithmetic')

    def test_private_null_configuration_and_audio_environment(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            env = runner.alsa_null_environment(root, {'PULSE_SERVER': 'synthetic',
                'PIPEWIRE_REMOTE': 'synthetic', 'ALSA_CONFIG_PATH': 'foreign', 'HOME': 'kept'})
            self.assertEqual(Path(env['ALSA_CONFIG_PATH']).parent, root)
            self.assertEqual(Path(env['ALSA_CONFIG_PATH']).read_text(), 'pcm.!default { type null }\n')
            self.assertNotIn('PULSE_SERVER', env)
            self.assertNotIn('PIPEWIRE_REMOTE', env)
            self.assertEqual(env['HOME'], 'kept')
            socket = root/'synthetic-audio-socket'
            socket.touch()
            masks = runner.audio_socket_masks({'PULSE_SERVER': 'unix:'+str(socket)})
            self.assertIn(str(socket), masks)
            self.assertIn('/dev/null', masks)

    def test_speech_requires_success_not_idle(self):
        good = dict(admitted=1, synthesis_valid=1, output_submitted=1, output_completed=1,
                    failed=0, retired=True)
        self.assertTrue(runner.arithmetic_speech_passed(good))
        for key in good:
            bad = dict(good)
            bad.pop(key)
            self.assertFalse(runner.arithmetic_speech_passed(bad), key)
        for key in ('admitted', 'synthesis_valid', 'output_submitted', 'output_completed'):
            self.assertFalse(runner.arithmetic_speech_passed(dict(good, **{key: 2})))
        self.assertFalse(runner.arithmetic_speech_passed(dict(good, failed=1)))

    def test_real_admitted_prompt_contract_and_exact_correlated_answer(self):
        profile = fixtures.arithmetic_profile()
        body = {'model': fixtures.MODEL, 'reasoning': {'effort': 'none'}, 'input': [
            {'role': 'user', 'content': profile.cases['arithmetic']}]}
        with fixtures.FakeServices(profile=profile) as service:
            status, data = request(service.generation_endpoint, body)
            self.assertEqual(status, 200)
            answer = ''.join(event['delta'] for event in events(data)
                             if event['type'] == 'response.output_text.delta')
            self.assertEqual(answer, profile.answer)
            self.assertEqual(service.snapshot()['searches'], 0)
            self.assertTrue(runner.answer_integrity('arithmetic', answer, answer, [], True, [], profile)['offline_fixture_match'])
            self.assertFalse(runner.answer_integrity('arithmetic', answer+'0', answer, [], True, [], profile)['offline_fixture_match'])
        body['input'][0]['content'] += ' extra'
        with fixtures.FakeServices(profile=profile) as service:
            self.assertEqual(request(service.generation_endpoint, body)[0], 400)
            self.assertEqual(service.snapshot()['failures'], 1)

    def test_rejected_arithmetic_exports_only_bounded_counts_and_categories(self):
        profile = fixtures.arithmetic_profile()
        prompt = profile.cases['arithmetic']
        for text, category in [(prompt + ' syntheticsecret', 'length'),
                               (prompt.replace('fourteen', 'syntheticsecret'), 'same_length_difference')]:
            body = {'model': fixtures.MODEL, 'reasoning': {'effort': 'none'},
                    'input': [{'role': 'user', 'content': [{'type': 'input_text', 'text': text}]}]}
            with fixtures.FakeServices(profile=profile) as service:
                self.assertEqual(request(service.generation_endpoint, body)[0], 400)
                diagnostic = service.snapshot()['rejection_diagnostics']
                self.assertEqual(diagnostic['mismatch'], category)
                self.assertGreaterEqual(diagnostic['first_mismatch_index'], 0)
                self.assertEqual(diagnostic['user_count'], 1)
                self.assertEqual(diagnostic['input_text_part_count'], 1)
                self.assertNotIn('syntheticsecret', json.dumps(diagnostic))
                self.assertTrue(all(type(value) is int or value in {
                    'complete', 'bounded', 'list', 'string', 'invalid', 'missing',
                    'length', 'same_length_difference', 'match', 'unknown', 'shape'}
                    for value in diagnostic.values()))

    def test_rejection_shape_and_analysis_limits_do_not_change_alias_success(self):
        profile = fixtures.arithmetic_profile()
        base = {'model': fixtures.MODEL, 'reasoning': {'effort': 'none'}}
        for inputs in ['syntheticsecret', [{'role': 'user', 'content': 7}]]:
            with fixtures.FakeServices(profile=profile) as service:
                self.assertEqual(request(service.generation_endpoint, dict(base, input=inputs))[0], 400)
                diagnostic = service.snapshot()['rejection_diagnostics']
                self.assertEqual(diagnostic['mismatch'], 'shape')
                self.assertNotIn('syntheticsecret', json.dumps(diagnostic))
        with fixtures.FakeServices(profile=profile) as service:
            body = dict(base, input=[{'role': 'user', 'content': 'syntheticsecret ' * 1000}])
            self.assertEqual(request(service.generation_endpoint, body)[0], 400)
            diagnostic = service.snapshot()['rejection_diagnostics']
            self.assertEqual(diagnostic['analysis'], 'bounded')
            self.assertEqual(diagnostic['mismatch'], 'unknown')
            self.assertLessEqual(diagnostic['actual_token_count'], 256)
        with fixtures.FakeServices(profile=profile) as service:
            text = profile.cases['arithmetic'].replace('fourteen', '14').replace('fifteen', '15').replace('times', '*')
            self.assertEqual(request(service.generation_endpoint, dict(base, input=[{'role': 'user', 'content': text}]))[0], 200)
            self.assertNotIn('rejection_diagnostics', service.snapshot())

    def test_identity_covers_adjacent_tts_json(self):
        profile = fixtures.arithmetic_profile()
        with tempfile.TemporaryDirectory() as temporary:
            model = Path(temporary)/'synthetic.onnx'
            model.write_bytes(b'synthetic-model')
            config = Path(str(model)+'.json')
            with self.assertRaises(ValueError):
                runner.arithmetic_identity(profile, model, {})
            config.write_text('{}')
            first = runner.arithmetic_identity(profile, model, {})
            config.write_text('{"synthetic":true}')
            second = runner.arithmetic_identity(profile, model, {})
            self.assertNotEqual(first['tts_assets_sha256'], second['tts_assets_sha256'])
            self.assertEqual(first['output_route'], 'alsa-null-software')
            self.assertFalse(first['acoustic_verified'])

    def test_arithmetic_identity_covers_imported_matcher_and_parser_not_just_contract(self):
        profile = fixtures.arithmetic_profile()
        legacy = runner.harness_identity()
        original = Path.read_bytes
        for filename in ('voice_contract.py', 'benchmark.py'):
            with self.subTest(filename=filename):
                def changed(path):
                    data = original(path)
                    return data + b'\n# synthetic implementation revision\n' if path.name == filename else data
                with patch.object(Path, 'read_bytes', changed):
                    revised = fixtures.arithmetic_profile()
                self.assertEqual(profile.contract_sha256, revised.contract_sha256)
                self.assertNotEqual(profile.implementation_sha256, revised.implementation_sha256)
        self.assertEqual(runner.harness_identity(), legacy)
        with tempfile.TemporaryDirectory() as temporary:
            model = Path(temporary)/'synthetic.onnx'
            model.write_bytes(b'synthetic')
            Path(str(model)+'.json').write_text('{}')
            identity = runner.arithmetic_identity(profile, model, {})
            self.assertEqual(identity['implementation_sha256'], profile.implementation_sha256)

    def test_arithmetic_environment_enables_only_required_native_owners(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root/'synthetic-source.toml'
            asset = root/'synthetic-asset'
            asset.write_bytes(b'synthetic')
            source.write_text('\n'.join(f'{key} = "{asset}"' for key in
                ('wake_word_model_path', 'parakeet_model_dir', 'silero_vad_model')))
            run = root/'run'
            run.mkdir()
            env = runner.isolated_environment(run, source, 'http://127.0.0.1:32123/responses',
                entry='voice-arithmetic', tts_model=asset)
            config = tomllib.loads((run/'config/config.toml').read_text())
            state = tomllib.loads((run/'config/state.toml').read_text())
            self.assertTrue(config['tts']['enabled'])
            self.assertTrue(state['tts_enabled'])
            self.assertFalse(config['web_enabled'])
            self.assertFalse(state['web_enabled'])
            self.assertFalse(state['assistant_completion'])
            self.assertNotIn('opencode', config)
            self.assertEqual(env['VOXGOLEM_BENCH_ENTRY'], 'voice-arithmetic')
            self.assertEqual(env['VOXGOLEM_BENCH_MODE'], 'offline')
            self.assertNotIn('VOXGOLEM_BENCH_PERMITS', env)


if __name__ == '__main__':
    unittest.main()
