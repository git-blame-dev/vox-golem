from pathlib import Path
import unittest
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
    def test_live_arithmetic_requires_exact_source_free_canonical_and_ui_answer(self):
        profile = fixtures.arithmetic_profile()
        good = runner.answer_integrity('arithmetic', profile.answer, profile.answer,
                                       [], False, [], profile)
        self.assertTrue(good['canonical_sources_expected'])
        self.assertTrue(good['sources_preserved'])
        self.assertTrue(good['profile_answer_exact'])
        self.assertFalse(good['offline_fixture_match'])
        for answer in (profile.answer+'0', 'The answer is '+profile.answer,
                       profile.answer+'\n\nSources:\n', ''):
            bad = runner.answer_integrity('arithmetic', answer, answer, [], False, [], profile)
            self.assertFalse(bad['profile_answer_exact'])
            native = dict(events=[{'stage':stage} for stage in (
                'prompt_admitted', 'generation_dispatched', 'canonical_turn_committed')], dropped=0)
            checks = runner.typed_checks('arithmetic', bad, native, None, True,
                                         {'inflight_text':True}, profile)
            checks['completed_text_outcome'] = True
            self.assertEqual(runner.typed_eligibility(checks), (False, False))
        sources = [{'url': 'https://example.invalid/synthetic'}]
        bad = runner.answer_integrity('arithmetic', profile.answer, profile.answer,
                                      [sources[0]['url']], False, sources, profile)
        self.assertFalse(bad['canonical_sources_expected'])
        self.assertFalse(bad['sources_preserved'])

    def test_removed_profiles_and_model_option_reject_before_execution(self):
        base = ['runner.py', '--binary', 'synthetic-app', '--tools', 'synthetic-tools',
                '--config', 'synthetic-config', '--output', 'synthetic-output']
        for flags in (['--entry', 'voice-arithmetic'], ['--entry', 'voice-arithmetic-windows'],
                      ['--entry', 'typed-speech', '--tts-model', 'synthetic-model']):
            with patch.object(sys, 'argv', base+flags), patch.object(runner, 'execute') as execute, \
                    contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    runner.main()
                self.assertEqual(error.exception.code, 2)
                execute.assert_not_called()

    def test_retired_internal_dispatch_cannot_reach_binary_or_live_budget(self):
        for live in (False, True):
            args = SimpleNamespace(entry='voice-arithmetic', live=live, cases=['arithmetic'],
                                   binary=Path('synthetic-app'))
            with patch.object(runner, 'validate_benchmark_binary') as validate:
                with self.assertRaisesRegex(ValueError, 'retired'):
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
