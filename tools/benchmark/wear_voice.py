"""One legacy synthetic Wear pipeline; no typed mailbox or question catalog."""
import asyncio
import hashlib
import json
import re
import signal
import subprocess
import sys

import benchmark
import wear
import instrumentation

Blocked = benchmark.Blocked
HOST_WATCHDOG_SECONDS = 239 + 2 * 5 + 30 + 21
MAX_OUTPUT_BYTES = 128 * 1024
TRANSPORT_REAP_SECONDS = 5
RUNNER = wear.PACKAGE + '.test/dev.gitblame.voxgolem.android.wear.speechtest.SyntheticAskInstrumentation'
ONE = ('synthetic_trial', 'finals', 'generation', 'GENERATION_DURABLE_COMMITTED',
       'GENERATION_SETTLED', 'VOICE_FLOW_SETTLED', 'fixture_words_match', 'fixture_eof_observed')
POSITIVE = ('PRESENTATION_TEXT_LAYOUT_OBSERVED', 'PRESENTATION_TEXT_DRAW_SUBMITTED',
            'TTS_UTTERANCE_ON_START', 'TTS_UTTERANCE_ON_DONE')
EVENTS = ('ACTIVITY_ACTIVITY_CREATED', 'ACTIVITY_RESUMED', 'RECOGNITION_FIRST_PARTIAL',
          'RECOGNITION_FINAL_CALLBACK', 'RECOGNITION_FINAL_MAIN_DELIVERED',
          'GENERATION_REQUEST_DISPATCHED', 'GENERATION_FIRST_TEXT', 'GENERATION_DURABLE_COMMITTED',
          'GENERATION_SETTLED', 'VOICE_FLOW_SETTLED', *POSITIVE)


def authorize(args, common):
    if args.platform != 'wear':
        raise Blocked('synthetic_voice_wear_only')
    if args.device != 'watch7' or args.build_variant != 'performance':
        raise Blocked('synthetic_voice_explicit_one_trial_consent_required')
    authorize_common(args)


def authorize_common(args):
    """Consent, bounded files and unsupported controls shared by voice adapters."""
    benchmark.reject_desktop_voice_options(args)
    if (not args.live or not args.allow_device or not args.adb or
            args.model != 'custom-luna-none' or args.repetitions != 1 or args.max_live_calls != 1):
        raise Blocked('synthetic_voice_explicit_one_trial_consent_required')
    if (args.cases is not None or args.wear_ui_probe or args.collect_stages or
            args.history_seed_turns != 1 or args.turn_timeout != 180 or args.port != 9223 or
            any(getattr(args, k) is not None for k in ('binary', 'tools', 'config', 'offline_evidence'))):
        raise Blocked('synthetic_voice_typed_or_desktop_options_unsupported')
    if getattr(args, 'explicit_options', set()) & {'--history-seed-turns', '--turn-timeout', '--port'}:
        raise Blocked('synthetic_voice_typed_options_unsupported')
    if (args.apk is None or args.apk.is_symlink() or not args.apk.is_file() or
            not 0 < args.apk.stat().st_size <= 256 * 1024 * 1024):
        raise Blocked('synthetic_bounded_local_apk_required')
    helper = getattr(args, 'helper_apk', None)
    if helper is None or helper.is_symlink() or not helper.is_file() or not 0 < helper.stat().st_size <= 256 * 1024 * 1024:
        raise Blocked('synthetic_bounded_helper_apk_required')
    if args.ssh and (not args.remote_adb or not re.fullmatch(r'[A-Za-z0-9_.@-]{1,128}', args.ssh) or args.ssh.startswith('-')):
        raise Blocked('ssh_host_alias_required')
    if args.remote_adb and not args.ssh:
        raise Blocked('remote_adb_requires_ssh')


def checked_trial(trial, required_positive=POSITIVE, word_count=4):
    """Validate native trial measurements, independently of final retirement proof."""
    try:
        if trial.get('stage') != 'complete' or trial.get('model') != 'custom-luna-none' or trial.get('idle') != 'true':
            raise ValueError()
        safe = {'stage':'complete', 'model':'custom-luna-none', 'idle':True}
        def number(key, low, high):
            value = trial.get(key, '')
            if not re.fullmatch(r'0|[1-9][0-9]{0,9}', value) or not low <= int(value) <= high:
                raise ValueError()
            safe[key] = int(value)
            return int(value)
        for key in ONE:
            number(key, 1, 1)
        for key in required_positive:
            number(key, 1, 10000)
        number('fixture_word_count', word_count, word_count)
        for key, value in (('fixture_sample_rate_hz', 16000), ('fixture_channels', 1), ('fixture_bits_per_sample', 16)):
            number(key, value, value)
        size = number('fixture_bytes', 2, 336000)
        duration = number('fixture_duration_ms', 0, 10500)
        if size % 2 or duration != size * 1000 // 32000:
            raise ValueError()
        digest = trial.get('fixture_pcm_sha256', '')
        if not re.fullmatch('[0-9a-f]{64}', digest):
            raise ValueError()
        safe['fixture_pcm_sha256'] = digest
        number('ACTIVITY_ACTIVITY_CREATED_ms', 0, 0)
        for key in EVENTS:
            if key + '_ms' in trial:
                number(key + '_ms', 0, 300000)
        return safe
    except (ValueError, TypeError):
        raise Blocked('synthetic_trial_invalid') from None


def parse_output(raw, returncode, *, required_positive=POSITIVE, word_count=4):
    """Never return arbitrary instrumentation keys, messages, or enum values."""
    if len(raw) > MAX_OUTPUT_BYTES:
        raise Blocked('synthetic_output_limit')
    try:
        trial, final, terminal_code = instrumentation.records(raw, trial=True, require_terminal=False)
        safe = checked_trial(trial, required_positive, word_count)
        expected = {'synthetic_protocol':'1', 'cleanup_verified':'true', 'tts_retired':'true',
                    'benchmark_result':'complete', 'completed_trials':'1',
                    'model':'custom-luna-none', 'phase':'trial_1'}
        if 'failure_class' in final and final.get('benchmark_result') == 'complete':
            raise ValueError()
        success = returncode == 0 and terminal_code == -1 and all(final.get(k) == v for k, v in expected.items())
        return safe, success
    except (ValueError, UnicodeError):
        raise Blocked('synthetic_protocol_invalid') from None


async def retire_transport(process, cleanup):
    return await instrumentation.retire_transport(process, cleanup, TRANSPORT_REAP_SECONDS)


async def observe(adb, cleanup, admissions=None):
    raw, code = await instrumentation.observe(adb, [
        '-e', 'liveProvider', 'true', '-e', 'syntheticAudio', 'true', '-e', 'trials', '1',
        '-e', 'model', 'custom-luna-none', RUNNER], cleanup, HOST_WATCHDOG_SECONDS,
        admissions, retire=retire_transport)
    return parse_output(raw, code)


async def run(args, report, common):
    adb = common.ADB(args.adb, args.ssh, args.remote_adb)
    adb.device = wear.select_watch(await asyncio.to_thread(adb.run, 'devices', '-l'))
    report['build'] = await asyncio.to_thread(wear.build_identity, adb, args)
    # Include secondary target processes, not just the main pid.
    processes = await asyncio.to_thread(adb.run, 'shell', 'ps', '-A', '-o', 'NAME')
    if any(p == wear.PACKAGE or p.startswith(wear.PACKAGE + ':') or p.startswith(wear.PACKAGE + '.test') for p in processes.split()):
        raise Blocked('preexisting_target_process_refused')
    paths = await asyncio.to_thread(adb.run, 'shell', 'pm', 'path', wear.PACKAGE + '.test')
    if not re.fullmatch(r'package:/[A-Za-z0-9_./=+~\-]+\.apk', paths):
        raise Blocked('synthetic_helper_identity_unavailable')
    digest = (await asyncio.to_thread(adb.run, 'shell', 'sha256sum', paths[len('package:'):])).split()[0]
    local = hashlib.sha256(args.helper_apk.read_bytes()).hexdigest()
    if not re.fullmatch('[0-9a-f]{64}', digest):
        raise Blocked('synthetic_helper_identity_unavailable')
    report['helper'] = {'installed_apk_sha256':digest, 'provided_apk_sha256':local, 'independent_signer_proof':'unverified',
                        'same_signer_enforcement':'android-instrumentation'}
    if digest != local:
        raise Blocked('synthetic_installed_helper_mismatch')
    trial, success = await observe(adb, report['cleanup'], report['admissions'])
    report['trial'] = trial
    report['trial_completed'] = True
    report['admissions'].update(submitted=1, observed=1)
    if not success:
        raise Blocked('synthetic_final_or_cleanup_unverified')
    report['checks'].update(pipeline='pass', native_cleanup='pass', tts_retirement='pass')
    report['cleanup']['native_retirement'] = 'verified'
    if report['cleanup'].get('local_transport') == 'reap_unverified':
        raise Blocked('local_transport_reap_unverified')
    report['workflow_completed'] = True


def harness_digest(seed, root, *, phone=False):
    """Hash only public checked-in source owners, never runtime config or keys."""
    files = {root / 'tools/benchmark' / name for name in
             ('wear_voice.py', 'instrumentation.py')}
    source_roots = [root / 'apps/wear/app/src/main', root / 'apps/wear/voice-benchmark/src/main',
                    root / 'tools/benchmark/android/src/main']
    if phone:
        files.update(root / 'tools/benchmark' / name for name in
                     ('phone_voice.py', 'voice_contract.py', 'scenarios.json'))
        source_roots += [
            root / 'crates/assistant-plugin/android/src/main',
            root / 'crates/assistant-plugin/android/src/benchmark',
            root / 'apps/android-tauri/src-tauri/gen/android/app/src/main',
            root / 'apps/android-tauri/voice-benchmark/src/main',
            root / 'apps/android-tauri/src-tauri/gen/android/voice-benchmark/src/main']
        for directory in ('apps/wear/voice-benchmark', 'apps/wear',
                          'apps/android-tauri/src-tauri/gen/android',
                          'apps/android-tauri/src-tauri/gen/android/app',
                          'apps/android-tauri/voice-benchmark',
                          'apps/android-tauri/src-tauri/gen/android/voice-benchmark',
                          'crates/assistant-plugin/android', 'tools/benchmark/android'):
            for name in ('build.gradle', 'build.gradle.kts', 'settings.gradle', 'settings.gradle.kts'):
                files.add(root / directory / name)
    source_roots.append(root / 'crates/voice-android/src/main')
    for directory in source_roots:
        if directory.is_dir():
            files.update(path for path in directory.rglob('*') if path.suffix in ('.kt', '.java', '.xml'))
    digest = hashlib.sha256(seed.encode())
    for path in sorted(files):
        if path.is_file():
            digest.update(path.relative_to(root).as_posix().encode() + b'\0' + path.read_bytes())
    return digest.hexdigest()


def main(args, common, *, adapter=None):
    if args.list:
        if args.platform != 'wear':
            print('synthetic_voice_wear_only', file=sys.stderr)
            return 1
        print('synthetic-voice-pipeline legacy-fixture one-trial')
        return 0
    if '--output' not in getattr(args, 'explicit_options', set()):
        args.output = common.ROOT.parent / 'artifacts/benchmarks/voice'
    report = {'schema':1, 'suite':'synthetic-voice-pipeline', 'platform':args.platform, 'status':'unverified',
        'workflow_completed':False, 'trial_completed':False,
        'admissions':{'submitted':0, 'observed':0, 'budget':1},
        'model':'custom-luna-none', 'repetitions':1, 'admission_budget':1,
        'native_trial_timeout_seconds':30, 'native_workload_timeout_seconds':90,
        'host_observation_watchdog_seconds':HOST_WATCHDOG_SECONDS,
        'transport_teardown_allowance_seconds':3 * TRANSPORT_REAP_SECONDS,
        'cleanup':{'native_retirement':'unverified', 'local_transport':'not_acquired'},
        'checks':dict.fromkeys(('pipeline', 'native_cleanup', 'tts_retirement', 'answer_correctness',
            'canonical_ui_equality', 'painted_pixels', 'acoustic_output', 'voice_provider_identity'), 'unverified'),
        'measurement':{'clock':'device-monotonic', 'offset_origin':'ACTIVITY_ACTIVITY_CREATED',
                       'boundary':'first-observations-not-duration-spans'},
        'limitations':['Generation counts BEGIN events, not HTTP requests.',
            'Transport termination does not retire native work; no retries after loss or timeout.',
            'Fixture PCM identity is observed; exact generator voice/provider provenance remains unverified.',
             'Pipeline acceptance is not answer correctness, painted pixels or acoustic acceptance.']}
    if adapter is not None:
        report.update(schema=2, suite='synthetic-voice-arithmetic', workload='arithmetic')
        report.pop('native_trial_timeout_seconds')
        report.pop('native_workload_timeout_seconds')
        report['profile_cleanup_observation_watchdog_seconds'] = adapter.CLEANUP_OBSERVATION_SECONDS
        report['limitations'][-1] = 'Exact canonical/UI checks do not establish painted pixels or acoustic acceptance.'
        report['checks']['profile_cleanup'] = 'unverified'
        report['cleanup']['profile_cleanup'] = 'unverified'
    def interrupted(signum, frame):
        raise Blocked('interrupted')
    previous = {s:signal.signal(s, interrupted) for s in (signal.SIGINT, signal.SIGTERM)}
    ready = False
    try:
        if adapter is not None:
            report['contract_sha256'] = adapter.voice_contract.contract_sha256()
        args.output = common.preflight_output(args.output)
        ready = True
        (adapter.authorize if adapter else authorize)(args, common)
        report['source'] = common.source_identity()
        report['source']['harness_sha256'] = harness_digest(
            report['source']['harness_sha256'], common.ROOT, phone=adapter is not None)
        asyncio.run((adapter.run if adapter else run)(args, report, common))
    except (Blocked, common.Blocked) as error:
        report.update(status='fail', failure_category=str(error))
    except (TimeoutError, subprocess.TimeoutExpired):
        report.update(status='fail', failure_category='observation_or_transport_deadline_native_retirement_unverified')
    except ConnectionError:
        report.update(status='fail', failure_category='transport_lost_native_retirement_unverified')
    except BaseException:
        report.update(status='fail', failure_category='operation_failed_payload_omitted')
    finally:
        for s, handler in previous.items():
            signal.signal(s, handler)
    report_path = None
    try:
        if not ready:
            args.output = common.preflight_output(args.output)
        report_path = str(common.write_report(args.output, report))
    except BaseException:
        report.update(status='fail', failure_category='report_writefail')
    print(json.dumps({key:report.get(key) for key in ('status', 'workflow_completed', 'trial_completed',
        'admissions', 'cleanup', 'failure_category')} | {'report_path':report_path}))
    return 1 if report['status'] == 'fail' else 2
