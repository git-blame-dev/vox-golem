import json
import io
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import benchmark
import voice_contract as voice


class ContractTest(unittest.TestCase):
    def test_production_source(self):
        contract = voice.load_contract()
        self.assertEqual(contract['expected_answer'], '210')
        self.assertEqual(len(voice.normalize_tokens(contract['spoken_prompt'])), 10)
        self.assertTrue(voice.fixture_matches('What is 14 × 15? Reply only with the number.', True))
        self.assertFalse(voice.fixture_matches(contract['spoken_prompt'], False))
        self.assertEqual(voice.contract_bytes(), json.dumps(contract, ensure_ascii=True,
            sort_keys=True, separators=(',', ':')).encode())

    def test_asset_real_generator(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch('sys.argv', ['voice_contract', '--write-android-assets', directory]):
                voice.main()
            self.assertEqual((Path(directory) / 'voxgolem-benchmark/arithmetic-voice.json').read_bytes(),
                             voice.contract_bytes())

    def test_unknown_voice_key_rejected(self):
        case = benchmark.load_scenarios()[0]
        case['voice']['unknown'] = True
        with self.assertRaises(ValueError):
            voice.validate_voice(case)

    def test_real_parser_rejects_unknown_duplicate_nonarithmetic_and_null(self):
        cases = benchmark.load_scenarios()
        variants = []
        duplicate = json.dumps(cases).replace('"voice": {', '"voice": {"prompt":"synthetic",', 1)
        variants.append(duplicate.encode())
        cases[0]['voice']['unknown'] = True
        variants.append(json.dumps(cases).encode())
        del cases[0]['voice']['unknown']
        cases[1]['voice'] = cases[0]['voice']
        variants.append(json.dumps(cases).encode())
        del cases[1]['voice']
        cases[0]['voice'] = None
        variants.append(json.dumps(cases).encode())
        for raw in variants:
            with self.subTest(raw=raw[:30]), patch.object(Path, 'open', return_value=io.BytesIO(raw)):
                with self.assertRaises(benchmark.Blocked):
                    benchmark.load_scenarios()
