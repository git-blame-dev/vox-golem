"""Explicit Windows qualification contract; legacy schema-4 reports never qualify."""
import hashlib
import json
import math
from pathlib import Path

PROFILE = 'windows-neural-live-qualified-v1'


def speech_row_valid(row, expected, api, offline):
    if type(row) is not dict:
        return False
    checks = row.get('checks', {})
    required = {'profile_answer_exact', 'local_speech_completed_and_retired',
                'speech_identity_matches_descriptor', 'ui_sources_preserved'}
    if offline:
        required |= {'fixture_answer_exact', 'request_contract_and_history',
                     'fixture_operations_exact', 'bounded_stream_gate'}
    dom = ('ui_submit_to_first_text_dom_ms', 'ui_submit_to_first_complete_word_dom_ms',
           'ui_submit_to_speech_observation_ms')
    native = ('prompt_to_first_model_text_ms', 'prompt_to_first_model_word_ms',
              'prompt_to_output_activated_ms', 'synthesis_request_to_valid_pcm_ms')
    events = row.get('native_events')
    if type(events) is not list or not 1 <= len(events) <= 512:
        return False
    try:
        if api.stage_breakdown(events) != row.get('native_breakdown'):
            return False
    except (KeyError, TypeError, AttributeError):
        return False
    number = lambda value: type(value) in (int, float) and math.isfinite(value) and value >= 0
    return (type(checks) is dict and type(row.get('native_breakdown')) is dict and required <= checks.keys() and
        api.typed_eligibility(checks)[1] is True and all(value is True for value in checks.values()) and
        all(number(row.get(key)) for key in dom) and
        all(number(row.get('native_breakdown', {}).get(key)) for key in native) and
        api.arithmetic_speech_passed(row.get('reply_speech')) and
        api.speech_identity_matches(row.get('speech_identity'), expected['speech_identity']))


def selected(args):
    return getattr(args, 'windows_profile', 'diagnostic') == 'live-qualified-v1'


def binding(args, api, profile, speech):
    from windows_effects import file_hash
    return dict(profile=PROFILE, binary_sha256=file_hash(args.binary),
        harness_sha256=api.windows_harness_identity(),
        source_manifest_sha256=hashlib.sha256(args.windows_manifest.read_bytes()).hexdigest(),
        contract_sha256=profile.contract_sha256, implementation_sha256=profile.implementation_sha256,
        workflow_identity=api.typed_identity(), speech_identity=speech)


def read(path):
    from windows_effects import reject_reparse, unique_object
    if path is None:
        raise ValueError('Windows qualification evidence required')
    reject_reparse(path)
    if not path.is_file() or path.stat().st_size > 128*1024:
        raise ValueError('Bounded qualification evidence required')
    return json.loads(path.read_text(), object_pairs_hook=unique_object)


def typed_evidence(path, expected, api, binary):
    value = read(path)
    if (value.get('windows_binding') != expected or value.get('windows_profile') != PROFILE or
            value.get('private_storage_acl_verified') is not True or
            value.get('app_job_and_creation_identity_verified') is not True or
            value.get('cleanup', {}).get('owned_job_retired') is not True):
        raise ValueError('Matching Windows typed qualification required')
    # Reuse every same-binary context, cancellation, duplicate and relaunch gate.
    api.validate_offline_evidence(path, binary, {}, {}, 'typed')
    return value


def speech_evidence(path, expected, api, binary):
    value = read(path)
    rows = value.get('rows')
    if (type(value.get('schema')) is not int or value['schema'] != 5 or
            value.get('mode') != 'offline' or value.get('entry') != 'typed-speech-qualified-windows' or
            value.get('windows_profile') != PROFILE or value.get('windows_binding') != expected or
            value.get('diagnostic_only') is not False or value.get('qualified_for_live') is not True or
            value.get('offline_profile_qualified') is not True or value.get('passed') is not True or
            value.get('workflow_completed') is not True or
            value.get('live_calls') != 0 or type(value.get('live_calls')) is not int or
            value.get('private_storage_acl_verified') is not True or
            value.get('app_job_and_creation_identity_verified') is not True or
            value.get('cleanup', {}).get('owned_job_retired') is not True or
            value.get('cleanup', {}).get('app_exit_before_group_termination_observed') is not True or
            type(rows) is not list or len(rows) != 1 or rows[0].get('case') != 'arithmetic' or
            rows[0].get('passed') is not True or not speech_row_valid(rows[0], expected, api, True) or
            value.get('typed_evidence_sha256') is None):
        raise ValueError('Matching qualified offline Neural arithmetic evidence required')
    # Revalidate the full typed report, not a copied boolean or diagnostic receipt.
    typed = typed_evidence(value.get('typed_evidence_path') and Path(value['typed_evidence_path']), expected, api, binary)
    if hashlib.sha256(Path(value['typed_evidence_path']).read_bytes()).hexdigest() != value['typed_evidence_sha256']:
        raise ValueError('Typed evidence changed')
    return typed
