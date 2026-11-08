"""Neural typed-speech adapter to the existing Windows desktop lifecycle owner."""
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import uuid

import benchmark
import voice_contract

Blocked = benchmark.Blocked
HOST_OBSERVATION_SECONDS = 900
MAX_REPORT_BYTES = 128 * 1024
TRANSPORT_REAP_SECONDS = 5


def live_profile(args):
    """The shared entry opts in through an explicit schema-3 profile manifest."""
    path = getattr(args, 'windows_manifest', None)
    if not getattr(args, 'live', False) or path is None:
        return False
    try:
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 256*1024:
            return False
        value = json.loads(path.read_text())
        return (type(value.get('schema')) is int and value['schema'] == 3 and
                value.get('profile') == 'windows-neural-live-qualified-v1')
    except (OSError, ValueError):
        return False


def native_platform():
    if sys.platform == 'win32' and os.name == 'nt':
        return 'windows'
    if sys.platform.startswith('linux') and os.name == 'posix':
        return 'linux'
    raise Blocked('desktop_native_host_unsupported')


def windows_python():
    if (native_platform() != 'windows' or sys.prefix == sys.base_prefix or
            importlib.util.find_spec('selenium') is None):
        raise Blocked('windows_native_private_python_selenium_required')
    return sys.executable


def authorize(args):
    if getattr(args, 'workload', None) != 'typed-speech':
        raise Blocked('desktop_synthetic_voice_profile_retired_use_neural_typed_speech_windows')
    voice_contract.load_contract()
    qualified = live_profile(args)
    if (args.platform != 'desktop' or (args.live and not qualified) or args.max_live_calls != (12 if qualified else 0) or
            args.repetitions != 1 or args.model != 'custom-luna-none'):
        raise Blocked('desktop_voice_offline_zero_live_budget_required')
    if getattr(args, 'wake_phrase', None) is not None:
        raise Blocked('typed_speech_forbids_voice_assets')
    required = ('binary', 'tools', 'config')
    if any(getattr(args, name, None) is None for name in required):
        raise Blocked('desktop_voice_explicit_assets_required')
    if (any(getattr(args, name, None) is not None for name in
            (('device', 'adb', 'ssh', 'remote_adb', 'apk', 'helper_apk', 'cases') if qualified else
             ('device', 'adb', 'ssh', 'remote_adb', 'apk', 'helper_apk', 'offline_evidence', 'cases'))) or
            args.allow_device or args.wear_ui_probe or args.collect_stages or
            args.history_seed_turns != 1 or args.port != 9223 or args.turn_timeout != 180 or
            args.build_variant != ('performance' if qualified else 'unspecified') or
            getattr(args, 'explicit_options', set()) &
            ({'--history-seed-turns', '--port', '--turn-timeout'} if qualified else
             {'--history-seed-turns', '--port', '--turn-timeout', '--build-variant'})):
        raise Blocked('desktop_voice_foreign_controls_unsupported')
    if qualified and args.offline_evidence is None:
        raise Blocked('desktop_voice_matching_qualification_required')
    platform = getattr(args, 'desktop_platform', None) or native_platform()
    if platform != 'windows':
        raise Blocked('typed_speech_requires_native_windows')
    if platform != native_platform():
        raise Blocked('desktop_voice_native_host_mismatch')
    if platform == 'windows' and getattr(args, 'windows_manifest', None) is None:
        raise Blocked('windows_staged_manifest_required')
    return platform


def fresh_output(args, common):
    output = args.output if '--output' in getattr(args, 'explicit_options', set()) else (
        common.ROOT.parent / 'artifacts' / ('voice-desktop-' + uuid.uuid4().hex))
    # Child owns creation, Linux permissions / Windows DACL and retention.
    if output.exists() or output.is_symlink() or not output.parent.is_dir():
        raise Blocked('desktop_voice_fresh_output_existing_parent_required')
    if any(path.is_symlink() for path in (output.parent, *output.parent.parents)):
        raise Blocked('desktop_voice_output_symlink_refused')
    output = output.absolute()
    if output.resolve().is_relative_to(common.ROOT.resolve()):
        raise Blocked('desktop_voice_output_must_be_outside_checkout')
    return output


def command(args, common, platform, output):
    interpreter = windows_python()
    if not Path(interpreter).is_file():
        raise Blocked('desktop_voice_existing_python_required')
    argv = [interpreter, str(common.ROOT / 'tools/app-bench/runner.py'), '--binary', str(args.binary),
        '--tools', str(args.tools), '--output', str(output), '--config', str(args.config),
        '--entry', 'typed-speech']
    argv += ['--desktop-platform', 'windows', '--windows-manifest', str(args.windows_manifest)]
    if live_profile(args):
        # Config means the separate canonical auth *reference* in this explicit
        # live contract. The staged source remains the synthetic Neural-only file.
        sys.path.insert(0, str(common.ROOT/'tools/app-bench'))
        import windows_effects
        from fixtures import arithmetic_profile
        _, roles = windows_effects.load_manifest(args.windows_manifest, arithmetic_profile(), qualified=True)
        argv[argv.index('--config')+1] = str(roles['config'])
        argv += ['--windows-profile', 'live-qualified-v1', '--live',
                 '--auth-source-config', str(args.config), '--offline-evidence', str(args.offline_evidence)]
    return argv


def qualified_failure_facts(value, code, args, common, evidence=None):
    """Validate bounded observations without treating them as successful execution."""
    sys.path.insert(0, str(common.ROOT/'tools/app-bench'))
    import runner
    import windows_qualification as qualification
    from fixtures import arithmetic_profile
    binding = value.get('windows_binding')
    if type(binding) is not dict or type(binding.get('speech_identity')) is not dict:
        raise Blocked('desktop_voice_child_profile_mismatch')
    expected = qualification.binding(args, runner, arithmetic_profile(),
                                      binding['speech_identity'])
    profile = dict(schema=5, mode='live', entry='typed-speech-qualified-windows',
        windows_profile=qualification.PROFILE, windows_binding=expected,
        requested_model='custom-luna-none', diagnostic_only=False, qualified_for_live=False)
    if any(type(value.get(key)) is not type(item) or value.get(key) != item for key, item in profile.items()):
        raise Blocked('desktop_voice_child_profile_mismatch')
    facts = dict(child_schema=5, child_evidence='validated-self-reported',
        child_exit_code=code if type(code) is int else None,
        live_calls=None, live_calls_verification='unverified', assignment_reserved=None,
        assignment_reserved_verification='unverified',
        child_identities={key:expected[key] for key in ('binary_sha256', 'harness_sha256',
            'source_manifest_sha256', 'contract_sha256', 'implementation_sha256')})
    invalid = code is not None and type(code) is not int
    cleanup = value.get('cleanup')
    if type(cleanup) is dict:
        safe_cleanup = {}
        for key in ('app_exit_before_group_termination_observed', 'app_exit_observed',
                    'forced_group_termination', 'webdriver_quit_error', 'owned_job_retired'):
            if key in cleanup:
                if type(cleanup[key]) is bool:
                    safe_cleanup[key] = cleanup[key]
                else:
                    invalid = True
        if safe_cleanup:
            facts['cleanup'] = safe_cleanup
    else:
        invalid = True
    for key, maximum in (('live_calls', 12), ('assignment_reserved', 100)):
        count = value.get(key)
        if count is not None:
            if type(count) is int and 0 <= count <= maximum and (key != 'assignment_reserved' or count % 12 == 0):
                facts[key] = count
                facts[key+'_verification'] = 'validated-self-reported'
            else:
                invalid = True
    rows = value.get('rows')
    counts = rows[0].get('generation_counts') if type(rows) is list and len(rows) == 1 and type(rows[0]) is dict else None
    if type(counts) is dict and 'spent' in counts:
        spent = counts['spent']
        if (type(spent) is not int or not 0 <= spent <= 12 or
                (facts['live_calls'] is not None and spent != facts['live_calls'])):
            facts.update(live_calls=None, live_calls_verification='unverified')
            invalid = True
    for key in ('passed', 'workflow_completed'):
        if key in value:
            if type(value[key]) is bool:
                facts['child_'+key] = value[key]
            else:
                invalid = True
    for key in ('failure_category', 'stage', 'platform_stage'):
        if key in value:
            category = value[key]
            if type(category) is str and re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]{0,63}', category):
                facts['child_'+key] = category
            else:
                invalid = True
    if evidence is not None:
        evidence.update(facts)
    if invalid:
        raise Blocked('desktop_voice_qualified_live_receipts_invalid')
    return runner, qualification, expected, facts


def interpret_qualified(value, code, args, common, *, evidence=None):
    runner, qualification, expected, facts = qualified_failure_facts(value, code, args, common, evidence)
    qualification.speech_evidence(args.offline_evidence, expected, runner, args.binary)
    rows, cleanup = value.get('rows'), value.get('cleanup', {})
    if (type(code) is not int or code != 0 or value.get('passed') is not True or
            value.get('workflow_completed') is not True or
            type(value.get('live_calls')) is not int or value['live_calls'] != 1 or
            value.get('private_storage_acl_verified') is not True or
            value.get('app_job_and_creation_identity_verified') is not True or
            cleanup.get('owned_job_retired') is not True or
            cleanup.get('app_exit_before_group_termination_observed') is not True or
            type(rows) is not list or len(rows) != 1 or rows[0].get('case') != 'arithmetic' or
            rows[0].get('passed') is not True or
            not qualification.speech_row_valid(rows[0], expected, runner, False) or
            rows[0].get('generation_counts') != {'generation_attempts':1, 'spent':1, 'remaining':11}):
        raise Blocked('desktop_voice_qualified_live_receipts_invalid')
    return dict(facts, status='pass', workflow_completed=True, qualified_for_live=False,
        live_admission_budget=12, fake_model_admission_budget=0, live_calls=1, child_schema=5,
        child_evidence='validated-self-reported',
        windows_binding=expected, cleanup=cleanup, child_rows=rows,
        acoustic_output='unverified', external_shared_service_lifecycle='unverified')


def launch(argv, root):
    # Only the runner is ours to signal. Its namespace / Windows Job owns app
    # retirement; killing this transport is never accepted as native cleanup.
    process = subprocess.Popen(argv, cwd=root, stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        return process.wait(timeout=HOST_OBSERVATION_SECONDS)
    finally:
        if process.returncode is None:
            try:
                process.terminate()
                try:
                    process.wait(timeout=TRANSPORT_REAP_SECONDS)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=TRANSPORT_REAP_SECONDS)
            except (OSError, subprocess.TimeoutExpired):
                pass  # Native and local retirement remain unverified on failure.


def child_report(output):
    path = output / 'report.json'
    if (output.is_symlink() or path.is_symlink() or not path.is_file() or
            any(parent.is_symlink() for parent in output.parents)):
        raise Blocked('desktop_voice_child_report_missing_or_unsafe')
    if path.resolve().parent != output.resolve():
        raise Blocked('desktop_voice_child_report_path_invalid')
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError()
            result[key] = value
        return result
    try:
        with path.open('rb') as stream:
            raw = stream.read(MAX_REPORT_BYTES + 1)
        if len(raw) > MAX_REPORT_BYTES:
            raise ValueError()
        value = json.loads(raw, object_pairs_hook=unique)
        if type(value) is not dict:
            raise ValueError()
        return value
    except (OSError, ValueError, UnicodeError):
        raise Blocked('desktop_voice_child_report_invalid') from None


def failure_receipts(value):
    """Bounded child categories/counts, not raw UI/error/service payloads."""
    rows = value.get('rows', [])
    if type(rows) is not list or len(rows) > 1:
        raise Blocked('desktop_voice_child_rows_invalid')
    receipts = []
    ui_categories = {'not configured':'not_configured', 'unavailable':'unavailable', 'invalid':'invalid',
                     'credential':'credential', 'authentication':'authentication', 'permission':'permission', 'failed':'failed'}
    for row in rows:
        if type(row) is not dict or row.get('case') != 'arithmetic':
            raise Blocked('desktop_voice_child_rows_invalid')
        if 'passed' in row and type(row['passed']) is not bool:
            raise Blocked('desktop_voice_child_rows_invalid')
        receipt = {}
        if 'failure' in row and type(row['failure']) is not str:
            raise Blocked('desktop_voice_child_rows_invalid')
        if row.get('failure') == 'flow_deadline':
            receipt['failure_category'] = 'flow_deadline'
        ui = row.get('ui_state', {})
        service = row.get('service_metadata', {})
        if type(ui) is not dict or type(service) is not dict:
            raise Blocked('desktop_voice_child_rows_invalid')
        categories = ui.get('failure_categories', [])
        if type(categories) is not list or len(categories) > 16 or any(type(c) is not str for c in categories):
            raise Blocked('desktop_voice_child_rows_invalid')
        known = sorted({ui_categories[c] for c in categories if c in ui_categories})
        if known:
            receipt['ui_failure_categories'] = known
        if 'runtime_status' in ui and type(ui['runtime_status']) is not str:
            raise Blocked('desktop_voice_child_rows_invalid')
        if ui.get('runtime_status') in ('initializing', 'sleeping', 'listening', 'processing', 'executing', 'error', 'unknown'):
            receipt['runtime_status'] = ui['runtime_status']
        if 'generation_requests' in service:
            count = service['generation_requests']
            if type(count) is not int or not 0 <= count <= 10000:
                raise Blocked('desktop_voice_child_rows_invalid')
            receipt['fake_generation_requests'] = count
        if receipt:
            receipts.append(receipt)
    return receipts


def interpret(value, code, platform, *, evidence=None, workload='typed-speech'):
    if type(code) is not int:
        raise Blocked('desktop_voice_child_exit_invalid')
    windows = platform == 'windows'
    if workload != 'typed-speech':
        raise Blocked('desktop_synthetic_voice_profile_retired_use_neural_typed_speech_windows')
    if not windows:
        raise Blocked('typed_speech_requires_native_windows')
    expected = {'schema':4, 'mode':'offline',
        'entry':'typed-speech-windows',
        'requested_model':'custom-luna-none', 'qualified_for_live':False,
        'diagnostic_only':windows, 'output_route':'wasapi-render-diagnostic'}
    if any(type(value.get(k)) is not type(v) or value.get(k) != v for k, v in expected.items()):
        raise Blocked('desktop_voice_child_profile_mismatch')
    identity = value.get('profile_identity')
    if (type(identity) is not dict or identity.get('contract_sha256') != voice_contract.contract_sha256() or
            identity.get('profile') != 'typed-speech-windows-diagnostic-v1'):
        raise Blocked('desktop_voice_child_contract_mismatch')
    if (type(value.get('live_calls')) is not int or value['live_calls'] != 0 or
            value.get('external_shared_service_lifecycle') != 'unverified'):
        raise Blocked('typed_speech_child_lifecycle_or_live_mismatch')
    cleanup = value.get('cleanup')
    if type(cleanup) is not dict:
        raise Blocked('desktop_voice_child_cleanup_missing')
    qualified = value.get('offline_profile_qualified')
    if type(qualified) is not bool:
        raise Blocked('desktop_voice_child_qualification_invalid')
    if ('passed' in value and type(value['passed']) is not bool) or (
            'workflow_completed' in value and type(value['workflow_completed']) is not bool):
        raise Blocked('desktop_voice_child_outcome_invalid')
    if 'overall_status' in value and value['overall_status'] != 'unverified':
        raise Blocked('desktop_voice_child_outcome_invalid')
    identities = {}
    for key in ('binary_sha256', 'harness_sha256', 'source_manifest_sha256', 'windows_effects_sha256',
                'native_source_sha256', 'isolated_config_sha256'):
        digest = value.get(key)
        if digest is not None or key in ('binary_sha256', 'harness_sha256'):
            if type(digest) is not str or not re.fullmatch('[0-9a-f]{64}', digest):
                raise Blocked('desktop_voice_child_identity_invalid')
            identities[key] = digest
    identities['contract_sha256'] = identity['contract_sha256']
    cleanup_fields = ('app_exit_before_group_termination_observed', 'app_exit_observed',
                      'forced_group_termination', 'webdriver_quit_error', 'owned_job_retired')
    if any(key in cleanup and type(cleanup[key]) is not bool for key in cleanup_fields):
        raise Blocked('desktop_voice_child_cleanup_invalid')
    if 'private_storage_acl_verified' in value and type(value['private_storage_acl_verified']) is not bool:
        raise Blocked('desktop_voice_child_cleanup_invalid')
    receipts = failure_receipts(value)
    facts = {'offline_profile_qualified':qualified, 'qualified_for_live':False,
        'child_schema':value['schema'], 'child_exit_code':code, 'child_passed':value.get('passed'),
        'child_workflow_completed':value.get('workflow_completed'),
        'child_overall_status':value.get('overall_status'),
        'diagnostic_only':windows, 'output_route':value['output_route'], 'child_identities':identities,
        'child_evidence':'validated-self-reported', 'child_failure_receipts':receipts,
        'cleanup':{key:cleanup[key] for key in cleanup_fields if key in cleanup}}
    if 'session_creation_cause' in value:
        cause = value['session_creation_cause']
        if type(cause) is not str or cause not in {'webview2_runtime_unavailable', 'edge_webdriver_mismatch', 'session_port_or_handshake_failed', 'application_terminated_during_startup', 'unclassified_session_not_created', 'webview_attach_timeout', 'version_mismatch', 'unsupported_capability', 'application_launch_failed', 'unknown'}:
            raise Blocked('desktop_voice_child_failure_metadata_invalid')
        facts['session_creation_cause'] = cause
    for meta_key in ('stage', 'platform_stage', 'failure_category'):
        if meta_key in value:
            meta_val = value[meta_key]
            if type(meta_val) is not str or not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]{0,63}', meta_val):
                raise Blocked('desktop_voice_child_failure_metadata_invalid')
            facts[meta_key] = meta_val
            if meta_key == 'failure_category':
                facts['child_failure_category'] = meta_val
    if value.get('native_error_code') is not None:
        err_code = value['native_error_code']
        if type(err_code) is not int or not 0 <= err_code <= 0xFFFFFFFF:
            raise Blocked('desktop_voice_child_failure_metadata_invalid')
        facts['native_error_code'] = err_code
    # Publish validated facts before classifying workflow/exit failure. None of
    # these receipts independently upgrades root workflow or native retirement.
    if evidence is not None:
        evidence.update(facts)
    if code != 1:
        raise Blocked('desktop_voice_unexpected_child_exit')
    if (value.get('passed') is not False or value.get('overall_status') != 'unverified' or
            qualified is not False or value.get('workflow_completed') is not True or
            cleanup.get('owned_job_retired') is not True or
            value.get('private_storage_acl_verified') is not True):
        raise Blocked('desktop_voice_windows_workflow_unverified')
    if cleanup.get('app_exit_before_group_termination_observed') is not True:
        raise Blocked('desktop_voice_child_retirement_unverified')
    return dict(facts, workflow_completed=True)


def main(args, common):
    if getattr(args, 'workload', None) != 'typed-speech':
        print(json.dumps({'status':'unsupported', 'workflow_completed':False,
            'failure_category':'desktop_synthetic_voice_profile_retired_use_neural_typed_speech_windows'}))
        return 2
    if args.list:
        voice_contract.load_contract()
        print('typed-speech-windows arithmetic one-trial diagnostic')
        return 0
    summary = {'status':'fail', 'workflow_completed':False, 'qualified_for_live':False,
        'live_admission_budget':0, 'fake_model_admission_budget':1, 'cleanup':'unverified',
        'host_observation_watchdog_seconds':HOST_OBSERVATION_SECONDS,
        'transport_teardown_allowance_seconds':2 * TRANSPORT_REAP_SECONDS}
    code = 1
    qualified, output = False, None
    try:
        platform = authorize(args)
        qualified = args.live
        summary['desktop_platform'] = platform
        if qualified:
            summary.update(live_admission_budget=12, fake_model_admission_budget=0,
                live_calls=None, live_calls_verification='unverified', assignment_reserved=None,
                assignment_reserved_verification='unverified')
        output = fresh_output(args, common)
        argv = command(args, common, platform, output)
        child_code = launch(argv, common.ROOT)
        summary['child_exit_code'] = child_code
        summary['report_path'] = str(output / 'report.json')
        if qualified:
            summary.update(interpret_qualified(child_report(output), child_code, args, common, evidence=summary))
            print(json.dumps(summary))
            return 0
        summary.update(interpret(child_report(output), child_code, platform, evidence=summary, workload=args.workload))
        summary['status'] = 'unverified'
        code = 2
    except Blocked as error:
        summary['failure_category'] = str(error)
    except subprocess.TimeoutExpired:
        if qualified and output is not None:
            try:
                qualified_failure_facts(child_report(output), None, args, common, summary)
            except Exception:
                pass  # Retain the timeout and any independently validated facts.
        summary['failure_category'] = 'desktop_voice_child_timeout_cleanup_unverified'
    except BaseException:
        summary['failure_category'] = 'desktop_voice_operation_failed_payload_omitted'
    print(json.dumps(summary))
    return code
