"""A17-only arithmetic instrumentation controller; never installs or retries."""
import asyncio
import hashlib
import re
import sys

import benchmark
import instrumentation
import voice_contract
import wear_voice

Blocked = benchmark.Blocked
RUNNER = 'dev.gitblame.voxgolem.android.test/dev.gitblame.voxgolem.android.speechtest.PhoneSyntheticVoiceInstrumentation'
CLEANUP_OBSERVATION_SECONDS = 60
PHONE_OFFSETS = ('FRONTEND_ASK_CLICKED_ms', 'FRONTEND_SUBMIT_OBSERVED_ms', 'PRESENTATION_DOM_OBSERVED_ms')
PHASES = {'startup', 'process_reservation', 'fixture', 'trial_1', 'profile_cleanup', 'profile_cleanup_pending'}
CLEANUP_FAILURE_REASONS = {'entry_precondition', 'marker_directory_invalid',
    'marker_namespace_invalid', 'marker_file_invalid', 'marker_schema_invalid', 'producer_not_exited',
    'owned_path_invalid', 'profile_budget_exceeded', 'owned_root_delete_failed', 'marker_delete_failed',
    'io_unavailable', 'unknown'}
FAILURE_CLASSES = {'ProcessReservationFailure', 'IllegalStateException', 'IllegalArgumentException',
    'SecurityException', 'UnsupportedOperationException', 'ClassNotFoundException', 'NoSuchMethodException',
    'InvocationTargetException', 'IOException', 'TimeoutException', 'Incomplete'}
TOKEN = r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
RESOURCE_PROOF = {'native_resources_retired':'true', 'activity_webview_retirement_pending':'true',
                  'native_cleanup_verified':'false', 'tts_retired':'true',
                  'profile_cleanup_pending':'true', 'cleanup_verified':'false'}


def require(bundle, expected):
    if any(bundle.get(k) != v for k, v in expected.items()):
        raise Blocked('phone_synthetic_proof_invalid')


def checked_phone_trial(trial):
    require(trial, dict.fromkeys((*wear_voice.ONE, 'frontend_submit_observed', 'ui_correlated_rows'), '1') |
        {'canonical_answer_exact':'true', 'canonical_ui_equal':'true',
         'profile_mapping_verified':'true', 'frontend_ask_clicked':'true',
         'synthetic_protocol':'2', 'contract_sha256':voice_contract.contract_sha256()})
    safe = wear_voice.checked_trial(trial,
        required_positive=('TTS_UTTERANCE_ON_START', 'TTS_UTTERANCE_ON_DONE'),
        word_count=voice_contract.expected_word_count())
    safe.update(frontend_submit_observed=1, ui_correlated_rows=1,
                profile_mapping_verified=True, frontend_ask_clicked=True)
    for key in PHONE_OFFSETS:
        if key in trial:
            value = trial[key]
            if not re.fullmatch(r'0|[1-9][0-9]{0,9}', value) or not 0 <= int(value) <= 300000:
                raise Blocked('phone_offset_invalid')
            safe[key] = int(value)
    return safe


def safe_status(trial):
    safe = {}
    if trial.get('stage') in ('complete', 'running', 'failed'):
        safe['stage'] = trial['stage']
    for key in (*wear_voice.ONE, *wear_voice.POSITIVE, 'frontend_submit_observed', 'ui_correlated_rows'):
        value = trial.get(key, '')
        if re.fullmatch(r'0|[1-9][0-9]{0,4}', value) and int(value) <= 10000:
            safe[key] = int(value)
    for key in ('profile_mapping_verified', 'frontend_ask_clicked'):
        if trial.get(key) in ('true', 'false'):
            safe[key] = trial[key] == 'true'
    for key in (*PHONE_OFFSETS, *(name + '_ms' for name in wear_voice.EVENTS)):
        value = trial.get(key, '')
        if re.fullmatch(r'0|[1-9][0-9]{0,9}', value) and int(value) <= 300000:
            safe[key] = int(value)
    return safe


def final_diagnostic(final):
    safe = {}
    if final.get('phase') in PHASES:
        safe['phase'] = final['phase']
    if final.get('benchmark_result') == 'failed':
        safe['result'] = 'failed'
    if final.get('benchmark_result') == 'failed' or 'failure_class' in final:
        category = final.get('failure_class')
        safe['failure_class'] = category if category in FAILURE_CLASSES else 'other'
        if 'cleanup_failure_reason' in final:
            reason = final['cleanup_failure_reason']
            safe['cleanup_failure_reason'] = reason if reason in CLEANUP_FAILURE_REASONS else 'unknown'
        if final.get('pending_count_class') in ('none', 'one', 'multiple', 'unknown'):
            safe['pending_count_class'] = final['pending_count_class']
        for key in ('producer_exit_verified', 'deletion_started'):
            if final.get(key) in ('true', 'false'):
                safe[key] = final[key] == 'true'
        if final.get('deleted_root_count') in ('0', '1', '2', '3'):
            safe['deleted_root_count'] = int(final['deleted_root_count'])
        for key, maximum in (('deleted_owned_directory_count', 32),
                             ('deleted_preference_count', 24), ('deleted_session_count', 8)):
            value = final.get(key)
            if isinstance(value, str) and re.fullmatch(r'0|[1-9][0-9]?', value) and int(value) <= maximum:
                safe[key] = int(value)
    return safe


def cleanup_token(final, code, terminal_code, digest):
    expected = {'synthetic_protocol':'2', **RESOURCE_PROOF}
    if (code != 0 or terminal_code not in (-1, 0) or
            any(final.get(k) != v for k, v in expected.items()) or
            final.get('phase') not in ('fixture', 'trial_1') or
            final.get('benchmark_result') not in ('failed', 'complete')):
        return None
    failed = final['benchmark_result'] == 'failed'
    if final.get('completed_trials') != ('0' if failed else '1'):
        return None
    if not failed and 'failure_class' in final:
        return None
    for key, value in (('model', 'custom-luna-none'), ('contract_sha256', digest)):
        if (key in final or not failed) and final.get(key) != value:
            return None
    token = final.get('cleanup_token', '')
    return token if re.fullmatch(TOKEN, token) else None


def parse_trial(raw, code, digest):
    trial, final, terminal_code = instrumentation.records(raw, trial='optional')
    token = final.get('cleanup_token', '')
    safe = checked_phone_trial(trial)
    if (final.get('synthetic_protocol') == '2' and final.get('model') == 'custom-luna-none' and
            final.get('contract_sha256') == digest):
        safe.update(contract_sha256=digest, canonical_answer_exact=True, canonical_ui_equal=True)
    expected = {'synthetic_protocol':'2', 'model':'custom-luna-none', 'contract_sha256':digest,
        'benchmark_result':'complete', 'completed_trials':'1', 'phase':'trial_1',
        **RESOURCE_PROOF}
    success = (code == 0 and terminal_code == -1 and 'failure_class' not in final and
        all(final.get(k) == v for k, v in expected.items()) and bool(re.fullmatch(
        TOKEN, token)))
    return safe, token if success else None, success


async def process_absence(adb):
    names = (await asyncio.to_thread(adb.run, 'shell', 'ps', '-A', '-o', 'NAME')).split()
    if any(n == benchmark.PACKAGE or n.startswith(benchmark.PACKAGE + ':') or
           n.startswith(benchmark.PACKAGE + '.test') for n in names):
        raise Blocked('preexisting_target_process_refused')


async def run(args, report, common):
    digest = voice_contract.contract_sha256()
    adb = common.ADB(args.adb, args.ssh, args.remote_adb)
    adb.device = common.select_a17(await asyncio.to_thread(adb.run, 'devices', '-l'))
    report['build'] = await asyncio.to_thread(common.build_identity, adb, args)
    await process_absence(adb)
    paths = await asyncio.to_thread(adb.run, 'shell', 'pm', 'path', common.PACKAGE + '.test')
    if not re.fullmatch(r'package:/[A-Za-z0-9_./=+~\-]+\.apk', paths):
        raise Blocked('synthetic_helper_identity_unavailable')
    installed = (await asyncio.to_thread(adb.run, 'shell', 'sha256sum', paths[8:])).split()[0]
    local = hashlib.sha256(args.helper_apk.read_bytes()).hexdigest()
    if installed != local:
        raise Blocked('synthetic_installed_helper_mismatch')
    report['helper'] = {'installed_apk_sha256':installed, 'provided_apk_sha256':local,
                        'same_signer_enforcement':'android-instrumentation'}
    await process_absence(adb)
    def completed_trial(trial):
        # Record counter/PCM evidence immediately; final contract identity and
        # retirement have not yet been established at this framing boundary.
        report['native_status'] = safe_status(trial)
        try:
            checked = checked_phone_trial(trial)
        except Blocked:
            return  # Let the helper finish its own native retirement, even on semantic failure.
        report['trial'] = checked
        report['trial_completed'] = True
        report['admissions'].update(submitted=1, observed=1)
    raw, code = await instrumentation.observe(adb, ['-e', 'liveProvider', 'true', '-e', 'syntheticAudio',
        'true', '-e', 'trials', '1', '-e', 'contractSha', digest, '-e', 'model',
        'custom-luna-none', RUNNER], report['cleanup'], wear_voice.HOST_WATCHDOG_SECONDS,
        report['admissions'], on_trial=completed_trial, trial='optional')
    trial, final, terminal_code = instrumentation.records(raw, trial='optional')
    report['native_final'] = final_diagnostic(final)
    native_success = False
    try:
        safe, _, native_success = parse_trial(raw, code, digest)
    except Blocked:
        pass
    else:
        report['trial'] = safe
        report['trial_completed'] = True
        report['admissions'].update(submitted=1, observed=1)
    if 'contract_sha256' in report.get('trial', {}):
        report['checks'].update(pipeline='pass', answer_correctness='pass', canonical_ui_equality='pass')
    token = cleanup_token(final, code, terminal_code, digest)
    if token is None:
        raise Blocked('phone_native_final_or_cleanup_unverified')
    report['checks'].update(native_resources_retirement='pass', tts_retirement='pass',
                            native_cleanup='pending', activity_webview_retirement='pending')
    report['cleanup'].update(native_resources_retirement='verified', native_retirement='pending',
                             activity_webview_retirement='pending', process_retirement='pending')
    if report['cleanup'].get('local_transport') == 'reap_unverified':
        raise Blocked('local_transport_reap_unverified')
    # No kill/retry: a foreign or surviving target blocks cleanup admission.
    # FINAL proves resource retirement only. Instrumentation must have ended and
    # independent process absence must establish Activity/WebView retirement.
    await process_absence(adb)
    report['checks'].update(activity_webview_retirement='pass',
                            process_retirement='pass')
    report['cleanup'].update(activity_webview_retirement='verified',
                             process_retirement='verified')
    profile_transport = {'local_transport':'not_acquired'}
    report['cleanup']['profile_transport'] = profile_transport
    raw, code = await instrumentation.observe(adb, ['-e', 'mode', 'cleanup', '-e', 'cleanupToken', token,
        RUNNER], profile_transport, CLEANUP_OBSERVATION_SECONDS)
    _, final, terminal_code = instrumentation.records(raw, trial=False)
    report['profile_final'] = final_diagnostic(final)
    if code != 0 or terminal_code != -1 or 'failure_class' in final:
        raise Blocked('phone_profile_cleanup_failure')
    require(final, {'synthetic_protocol':'2', 'benchmark_result':'cleaned', 'cleanup_verified':'true',
                    'profile_cleaned':'true', 'completed_trials':'0',
                    'producer_exit_verified':'true', 'activity_webview_retired':'true',
                    'storage_cleanup_verified':'true'})
    await process_absence(adb)
    if profile_transport.get('local_transport') != 'reaped':
        raise Blocked('local_transport_reap_unverified')
    report['cleanup']['profile_cleanup'] = 'verified'
    report['cleanup']['native_retirement'] = 'verified'
    report['checks'].update(profile_cleanup='pass', native_cleanup='pass')
    if not native_success:
        raise Blocked('phone_trial_semantics_failed_after_owned_cleanup')
    report['workflow_completed'] = True


def authorize(args, common):
    voice_contract.load_contract()
    if args.platform != 'android' or args.device != 'a17' or args.build_variant not in ('release', 'profileable'):
        raise Blocked('phone_optimized_a17_required')
    wear_voice.authorize_common(args)


def main(args, common):
    if args.list:
        voice_contract.load_contract()
        print('synthetic-voice-arithmetic arithmetic one-trial')
        return 0
    # Shared report/error/retention lifecycle; the phone policy is independently fenced.
    return wear_voice.main(args, common, adapter=sys.modules[__name__])
