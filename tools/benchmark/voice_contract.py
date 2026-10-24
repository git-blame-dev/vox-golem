"""Canonical portable spoken scenario contract; no runtime or device access."""
import argparse
import hashlib
import json
from pathlib import Path
import re


def validate_voice(case):
    voice = case.get('voice')
    if 'voice' not in case:
        return
    if case['id'] != 'arithmetic' or type(voice) is not dict or set(voice) != {'prompt', 'token_aliases'}:
        raise ValueError('voice_metadata_invalid')
    prompt, aliases = voice['prompt'], voice['token_aliases']
    if (type(prompt) is not str or not 1 <= len(prompt.encode('utf-8')) <= 512 or
            any(ord(c) < 32 for c in prompt) or type(aliases) is not dict or len(aliases) > 32):
        raise ValueError('voice_metadata_invalid')
    for key, value in aliases.items():
        if (not re.fullmatch(r'[a-z]+|[0-9]+|[×*]', key) or type(value) is not str or
                not re.fullmatch('[a-z]{1,32}', value) or len(key) > 32):
            raise ValueError('voice_alias_invalid')


def load_contract():
    import benchmark
    case = benchmark.load_scenarios()[0]
    if 'voice' not in case:
        raise benchmark.Blocked('voice_metadata_required')
    return {'version': 1, 'scenario_id': case['id'], 'spoken_prompt': case['voice']['prompt'],
            'expected_answer': case['expected'], 'web_enabled': case['web_enabled'],
            'token_aliases': case['voice']['token_aliases']}


def contract_bytes(contract=None):
    payload = json.dumps(load_contract() if contract is None else contract,
                         ensure_ascii=True, sort_keys=True, separators=(',', ':')).encode('utf-8')
    if len(payload) > 4096:
        raise ValueError('voice_contract_size_limit')
    return payload


def contract_sha256(contract=None):
    return hashlib.sha256(contract_bytes(contract)).hexdigest()


def implementation_sha256():
    import benchmark
    sources = {
        'matcher': fixture_matches.__code__.co_filename,
        'normalizer': normalize_tokens.__code__.co_filename,
        'parser': benchmark.load_scenarios.__code__.co_filename,
        'validator': validate_voice.__code__.co_filename,
    }
    return hashlib.sha256(json.dumps({
        label: hashlib.sha256(Path(path).read_bytes()).hexdigest()
        for label, path in sources.items()
    }, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def normalize_tokens(text, contract=None):
    aliases = (load_contract() if contract is None else contract)['token_aliases']
    return [aliases.get(token, token) for token in re.findall(r'[a-z]+|[0-9]+|[×*]', text.lower())]


def fixture_matches(text, eof, contract=None):
    contract = load_contract() if contract is None else contract
    return eof is True and normalize_tokens(text, contract) == normalize_tokens(contract['spoken_prompt'], contract)


def expected_word_count(contract=None):
    contract = load_contract() if contract is None else contract
    return len(normalize_tokens(contract['spoken_prompt'], contract))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write-android-assets', required=True, type=Path)
    args = parser.parse_args()
    payload = contract_bytes()
    directory = args.write_android_assets / 'voxgolem-benchmark'
    directory.mkdir(parents=True, exist_ok=True)
    (directory / 'arithmetic-voice.json').write_bytes(payload)


if __name__ == '__main__':
    main()
