#!/usr/bin/env python3
"""One real-app workflow benchmark. Offline unless explicitly --live.

Run with the pinned benchmark-tools interpreter. Fixtures and reports never use
personal audio/history. Live generation is additionally fenced inside Rust.
"""
import argparse
try:
    import fcntl
except ImportError:  # Windows diagnostic never accesses the Unix live ledger.
    fcntl = None
import hashlib
import json
import math
import os
from pathlib import Path
import re
import signal
import shutil
import socket
import subprocess
import sys
import time
import tomllib
from types import SimpleNamespace

from fixtures import CASES, URLS, FakeServices, expected_answer, generate_fixtures, arithmetic_profile
from paths import artifact_root

ROOT = Path(__file__).resolve().parents[2]
MAX_ASSIGNMENT_CALLS = 100
LIVE_BATCH_ALLOWANCE = 12
ASSIGNMENT_LEDGER = None
ARTIFACT_ROOT = None
PHASE = 'preflight'
APP_PID = None
CAPABILITY = b'voxgolem:app-bench:canonical-managed-auth:v2'
CANCEL_POST_RELEASE_SECONDS = 0.5
WORKFLOW_SAFETY_CHECKS = ('empty_admission', 'duplicate_ui_submission', 'cancellation_retirement',
    'warm_context', 'longer_history', 'relaunch_preferences', 'relaunch_without_generation')


def arithmetic_speech_passed(speech):
    return (isinstance(speech, dict) and speech.get('retired') is True and
            all(type(speech.get(key)) is int and speech[key] == 1 for key in
                ('admitted', 'synthesis_valid', 'output_submitted', 'output_completed')) and
            type(speech.get('failed')) is int and speech['failed'] == 0)


def speech_identity_matches(actual, expected):
    return (isinstance(actual, dict) and actual.keys() == expected.keys() and
            all(type(actual[key]) is type(value) and actual[key] == value
                for key, value in expected.items()))


def private_json(path, value):
    with open(path, 'x', opener=lambda p, f: os.open(p, f, 0o600)) as stream:
        json.dump(value, stream, indent=2, sort_keys=True)
        stream.flush()
        os.fsync(stream.fileno())
    if os.name != 'nt':
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try: os.fsync(directory)
        finally: os.close(directory)


def prune_owned_reports(parent, now=None, max_bytes=8*1024*1024, max_files=128):
    """Only this controller's marked metadata reports, never fixtures or ledgers."""
    now = time.time() if now is None else now
    owned = []
    for directory in parent.iterdir():
        marker = directory/'.app-bench-run.json'
        try:
            if not directory.is_dir() or directory.is_symlink() or marker.is_symlink(): continue
            if marker.stat().st_size > 1024: continue
            value = json.loads(marker.read_text())
            created = value['created_epoch_seconds']
            if (value.get('owner') != 'voxgolem-app-bench' or value.get('schema') != 1 or
                    type(created) not in (int, float) or not math.isfinite(created) or created < 0): continue
            for name in ['report.json', 'driver-free-diagnostic.json']:
                report = directory/name
                if report.is_file() and not report.is_symlink():
                    owned.append((created, report, report.stat().st_size))
        except (OSError, ValueError, KeyError, TypeError):
            continue
    total = sum(size for _, _, size in owned)
    count = len(owned)
    for created, report, size in sorted(owned):
        if created > now or now-created > 90*86400 or total > max_bytes or count > max_files:
            try: report.unlink()
            except FileNotFoundError: pass
            total -= size
            count -= 1


def reserve_live_budget(ledger, reservation):
    """Reservations survive disposable runs; uncertainty never refunds calls."""
    if fcntl is None:
        raise ValueError('Live ledger is supported only by the qualified Unix path')
    if ledger.is_symlink() or any(p.is_symlink() for p in ledger.parents):
        raise ValueError('Budget ledger must not be a symlink')
    ledger = ledger.resolve()
    if ledger.is_relative_to(reservation.parent.resolve()):
        raise ValueError('Budget ledger must be outside disposable run storage')
    if not ledger.parent.is_dir():
        raise ValueError('Budget ledger requires an existing durable parent')
    descriptor = os.open(ledger, os.O_RDWR | os.O_CREAT, 0o600)
    with os.fdopen(descriptor, 'r+') as stream:
        fcntl.flock(stream, fcntl.LOCK_EX)
        stream.seek(0)
        text = stream.read(8193)
        value = json.loads(text) if text else {'schema': 1, 'reserved': 0}
        if (len(text) > 8192 or value.get('schema') != 1 or
                type(value.get('reserved')) is not int or
                not 0 <= value['reserved'] <= MAX_ASSIGNMENT_CALLS - LIVE_BATCH_ALLOWANCE):
            raise ValueError('Budget ledger invalid or allowance exhausted')
        # Persist the debit first. A failed subsequent reservation burns it.
        value['reserved'] += LIVE_BATCH_ALLOWANCE
        stream.seek(0)
        json.dump(value, stream, sort_keys=True)
        stream.truncate()
        stream.flush()
        os.fsync(stream.fileno())
        # A durable file does not make a newly created directory entry durable.
        directory = os.open(ledger.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        private_json(reservation, {'schema': 1, 'allowance': LIVE_BATCH_ALLOWANCE})
    return value['reserved']


def validate_offline_namespace(original, current, interfaces):
    if (not isinstance(original, str) or not re.fullmatch(r'net:\[\d+\]', original) or
            original == current or set(interfaces) != {'lo'}):
        raise ValueError('Offline mode requires a distinct loopback-only namespace')


def validate_private_session(original_pidns, current_pidns, original_bus, current_bus):
    if (not isinstance(original_pidns, str) or not re.fullmatch(r'pid:\[\d+\]', original_pidns) or
            original_pidns == current_pidns or original_bus is None or
            not current_bus or current_bus == original_bus):
        raise ValueError('Benchmark requires an owned process namespace and private session bus')


def free_port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def harness_identity():
    digest = hashlib.sha256()
    for name in ['runner.py', 'fixtures.py', 'paths.py']:
        digest.update(name.encode())
        digest.update(Path(__file__).with_name(name).read_bytes())
    return digest.hexdigest()


def validate_benchmark_binary(binary):
    if not binary.is_file() or binary.stat().st_size > 256*1024*1024:
        raise ValueError('Benchmark executable is unavailable or oversized')
    if CAPABILITY not in binary.read_bytes():
        raise ValueError('Refusing to launch an executable without the synthetic-input capability')


def validate_offline_evidence(path, binary, fixtures, speech_identity, entry='voice'):
    if entry not in ('voice', 'typed'):
        raise ValueError('Only legacy voice and typed profiles have a live qualification policy')
    if path is None or path.stat().st_size > 128*1024:
        raise ValueError('A bounded successful offline report is required')
    evidence = json.loads(path.read_text())
    rows = evidence.get('rows', [])
    if entry == 'typed':
        if (evidence.get('schema') != 2 or evidence.get('mode') != 'offline' or
                evidence.get('entry') != 'typed' or evidence.get('coverage') != 'typed-workflow' or
                evidence.get('diagnostic_only') is not False or
                evidence.get('binary_sha256') != hashlib.sha256(binary.read_bytes()).hexdigest() or
                evidence.get('harness_sha256') != harness_identity() or
                evidence.get('workflow_identity') != typed_identity() or
                evidence.get('cleanup', {}).get('app_exit_before_group_termination_observed') is not True or
                {r.get('case') for r in rows if r.get('qualification_eligible') is True} != set(CASES) or
                len(rows) != len(CASES) or
                any(r.get('qualification_eligible') is not True or
                    typed_eligibility(r.get('checks', {}))[1] is not True or
                    any(r.get('checks', {}).get(name) is not True for name in (
                        'fixture_answer_exact', 'request_contract_and_history',
                        'fixture_operations_exact', 'bounded_stream_gate')) or
                    (r.get('case') == 'quick' and r.get('checks', {}).get('quick_answer_correct') is not True)
                    for r in rows) or
                not workflow_safety_passed(evidence.get('safety_checks', {}))):
            raise ValueError('Matching offline typed-workflow and safety evidence is required')
        return
    if (evidence.get('schema') != 1 or evidence.get('mode') != 'offline' or evidence.get('entry') != 'voice' or
            evidence.get('diagnostic_only') is not False or
            evidence.get('wake_score_diagnostics') is not False or
            evidence.get('browser_call_probes') is not False or
            evidence.get('binary_sha256') != hashlib.sha256(binary.read_bytes()).hexdigest() or
            evidence.get('harness_sha256') != harness_identity() or
            evidence.get('fixture_metadata') != fixtures or
            evidence.get('speech_identity') != speech_identity or
            evidence.get('cleanup', {}).get('app_exit_before_group_termination_observed') is not True or
            {r.get('case') for r in rows if r.get('passed') is True} != set(CASES) or
            len(rows) != len(CASES) or
            any(r.get('passed') is not True or r.get('dropped') != 0 or
                not native_audio_owned(r) for r in rows)):
        raise ValueError('Offline acceptance does not match this binary and all happy paths')


def typed_identity():
    return {'entry': 'typed', 'model': 'custom-luna-none', 'effort': 'none',
            'web_enabled': True, 'tts_enabled': False, 'completion': False,
            'speech_assets_loaded': False, 'prompts_sha256': hashlib.sha256(
                json.dumps(CASES, sort_keys=True).encode()).hexdigest(),
            'deferred_checks': ['desktop_capture', 'ui_source_projection', 'desktop_history_relaunch']}


def speech_configuration_identity(source_config):
    config = tomllib.loads(source_config.read_text())
    identity = {key: config.get(key, default) for key, default in [
        ('initial_silence_timeout_ms', 2500), ('silence_timeout_ms', 750),
        ('wake_word_detection_threshold', 0.68)]}
    hashes = {}
    for key in ['wake_word_model_path', 'silero_vad_model', 'parakeet_model_dir']:
        path = Path(config[key])
        if not path.is_absolute(): path = source_config.parent/path
        digest = hashlib.sha256()
        files = sorted(p for p in path.rglob('*') if p.is_file()) if path.is_dir() else [path]
        for file in files:
            if path.is_dir(): digest.update(str(file.relative_to(path)).encode())
            with file.open('rb') as stream:
                while block := stream.read(1024*1024): digest.update(block)
        hashes[key] = digest.hexdigest()
    identity['asset_sha256'] = hashes
    identity.update(source_rate_hz=16000, source_channels=1, frame_samples=480,
                    web_enabled=True, tts_enabled=False, completion=False, partial_asr=True)
    return identity


MANAGED_ENDPOINT = 'https://chatgpt.com/backend-api/codex/responses'


def managed_credential(config_dir):
    return config_dir/'chatgpt'/'chatgpt-auth.json'


def live_auth_source_config(run, source_config):
    """Validate configuration metadata only; native code owns all credential access."""
    try:
        resolved = source_config.resolve(strict=True)
        if (not source_config.is_absolute() or source_config != resolved or
                not source_config.is_file() or resolved.is_relative_to(run.resolve())):
            raise ValueError('Live auth source must be a canonical regular config outside the run')
    except OSError as error:
        raise ValueError('Live auth source config unavailable') from error
    return resolved


def isolated_environment(run, source_config, endpoint, live=False, reservation=None, entry='voice',
                         output_route='windows-diagnostic', base_environment=None):
    if entry == 'typed-speech' and (live or reservation is not None):
        raise ValueError('Typed speech is offline with no live reservation')
    if entry not in ('voice', 'typed', 'typed-control', 'typed-speech'):
        raise ValueError('Desktop synthetic-voice arithmetic profile retired; use Neural typed-speech Windows diagnostics')
    env = (os.environ if base_environment is None else base_environment).copy()
    if not live and 'VOXGOLEM_BENCH_AUTH_SOURCE_CONFIG' in env:
        raise ValueError('Offline mode forbids an auth source override')
    if live:
        source_config = live_auth_source_config(run, source_config)
    original = tomllib.loads(source_config.read_text())
    if not env.get('XAUTHORITY') and (Path(env.get('HOME', ''))/'.Xauthority').is_file():
        env['XAUTHORITY'] = str(Path(env['HOME'])/'.Xauthority')
    wayland = env.get('WAYLAND_DISPLAY')
    if wayland and not Path(wayland).is_absolute() and env.get('XDG_RUNTIME_DIR'):
        env['WAYLAND_DISPLAY'] = str(Path(env['XDG_RUNTIME_DIR'])/wayland)
    for key in list(env):
        if key.lower().endswith('_proxy'):
            del env[key]
    for name, folder in [('HOME', 'home'), ('XDG_CONFIG_HOME', 'config'),
                         ('XDG_DATA_HOME', 'data'), ('XDG_STATE_HOME', 'state'),
                         ('XDG_CACHE_HOME', 'cache'), ('XDG_RUNTIME_DIR', 'runtime')]:
        path = run / folder
        path.mkdir(mode=0o700, exist_ok=True)
        env[name] = str(path)
    config_path = run / 'config' / 'config.toml'
    config = []
    for key in ([] if entry in ('typed', 'typed-speech') else ['wake_word_model_path', 'parakeet_model_dir', 'silero_vad_model']):
        path = Path(original[key])
        if not path.is_absolute():
            path = source_config.parent / path
        if not path.exists():
            raise ValueError('Required speech asset missing')
        config.append(f'{key} = {json.dumps(str(path.resolve()))}')
    for key, default in [('initial_silence_timeout_ms', 2500), ('silence_timeout_ms', 750),
                         ('wake_word_detection_threshold', 0.68)]:
        config.append(f'{key} = {original.get(key, default)}')
    # Custom is selected in assistant settings, not the legacy backend enum.
    # No OpenCode table means no unrelated server is started.
    typed_speech = entry == 'typed-speech'
    config += [f'web_enabled = {str(not typed_speech).lower()}', 'response_backend = "opencode"', '[custom_openai]']
    if live:
        env['VOXGOLEM_BENCH_AUTH_SOURCE_CONFIG'] = str(source_config)
        config += [f'endpoint = {json.dumps(MANAGED_ENDPOINT)}']
    else:
        credential = managed_credential(run/'config')
        credential.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        private_json(credential, {'openai': {'access': 'synthetic-benchmark-access',
                                            'expires': 4102444800000}})
        config += [f'endpoint = {json.dumps(endpoint)}']
    # Legacy profiles omit reply speech; typed speech uses the existing Neural service.
    if entry == 'typed-speech':
        from windows_effects import neural_configuration
        neural = neural_configuration(source_config)
        config += ['[tts]', 'enabled = true'] + [f'{key} = {json.dumps(value)}' for key, value in neural.items()]
    config += ['[telemetry]', 'enabled = false',
               '[logging]', 'enabled = false']
    config_path.write_text('\n'.join(config) + '\n')
    state = '''web_enabled = true
tts_enabled = false
auto_update_download = false
auto_update_install = false
assistant_instant = "custom-luna-none"
assistant_deep = "opencode-sol-high"
assistant_review = "custom-sol-high"
assistant_deep_enabled = false
assistant_review_enabled = false
assistant_prefetch = false
assistant_completion = false
'''
    if typed_speech:
        state = state.replace('web_enabled = true', 'web_enabled = false').replace('tts_enabled = false', 'tts_enabled = true')
        if output_route == 'windows-diagnostic':
            env = {key: value for key, value in env.items()
                   if not key.upper().startswith(('PULSE_', 'PIPEWIRE_', 'ALSA_', 'JACK_'))}
        else:
            raise ValueError('Unknown typed-speech output route')
    (config_path.parent / 'state.toml').write_text(state)
    env.update(VOXGOLEM_BENCH_MODE='live' if live else 'offline',
               VOXGOLEM_BENCH_ENTRY=entry if entry in ('typed', 'typed-speech') else 'voice',
               VOXGOLEM_BENCH_ROOT=str(run), VOXGOLEM_CONFIG_PATH=str(config_path),
               TAURI_AUTOMATION='true')
    env.pop('VOXGOLEM_BENCH_AUTO_DIAGNOSTIC_CASE', None)
    env.pop('VOXGOLEM_BENCH_MCP_ENDPOINT', None)
    env.pop('VOXGOLEM_BENCH_PERMITS', None)
    if reservation:
        env['VOXGOLEM_BENCH_PERMITS'] = str(reservation)
    return env


def invoke(driver, command, args=None, observer=False):
    global APP_PID
    value = driver.execute_async_script('''const done = arguments[arguments.length - 1];
window.__TAURI_INTERNALS__.invoke(arguments[0], arguments[1]).then(
  value => {if(arguments[2])window.__voxBenchObserve?.speech(value);done({ok:true,value});}, () => done({ok:false}));''', command, args or {}, observer)
    if not isinstance(value, dict) or value.get('ok') is not True:
        raise RuntimeError('Application observation/admission command failed')
    result = value['value']
    if command == 'benchmark_snapshot':
        pid = result.pop('process_id', None)
        if type(pid) is int:
            if APP_PID is not None and APP_PID != pid:
                raise RuntimeError('Application process correlation changed')
            APP_PID = pid
    return result


def app_running(pid):
    if pid is None: return False
    try:
        stat = Path(f'/proc/{pid}/stat').read_text()
        return stat.rsplit(')', 1)[1].strip().split()[0] != 'Z'
    except FileNotFoundError:
        return False


def shutdown_owned(driver, process):
    started = time.perf_counter()
    quit_error = False
    forced = False
    if driver:
        try: driver.quit()
        except Exception: quit_error = True
    deadline = time.monotonic()+3
    while app_running(APP_PID) and time.monotonic() < deadline:
        time.sleep(0.05)
    graceful = APP_PID is not None and not app_running(APP_PID)
    if process:
        try:
            if app_running(APP_PID): forced = True
            os.killpg(process.pid, signal.SIGTERM)
            process.wait(timeout=10)
        except ProcessLookupError:
            pass
        except subprocess.TimeoutExpired:
            forced = True
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)
    return {'app_exit_observed': APP_PID is not None and not app_running(APP_PID),
            'app_exit_before_group_termination_observed': graceful, 'forced_group_termination': forced,
            'webdriver_quit_error': quit_error,
            'elapsed_ms': (time.perf_counter()-started)*1000}


def wait_for(check, seconds, label):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.1)
    raise TimeoutError(label)


def ui_state(driver):
    return driver.execute_script('''const phase=document.querySelector('.shell')?.dataset.runtimeStatus;const text=document.body.textContent.toLowerCase();const allowed=['channel support','unavailable','not configured','invalid','credential','authentication','stale','cancelled','stream','permission','unknown command','failed','request is','model','auth'];return {
failure_categories:allowed.filter(word=>text.includes(word)),
turn_states:Array.from(document.querySelectorAll('[data-turn-role=assistant][data-request-id]')).map(e=>({outcome:e.dataset.turnOutcome,stage:e.dataset.stageStatus,kind:e.dataset.contentKind})),
composer_length:document.getElementById('promptComposer')?.value.length||0,
user_rows:document.querySelectorAll('[data-turn-role=user][data-request-id]').length,
submit_enabled:document.querySelector('.composer button[type=submit]')?.disabled===false,
runtime_status:['initializing','sleeping','listening','processing','executing','error'].includes(phase)?phase:'unknown'};''')


OBSERVE = '''const prior=arguments[0];
const noticeText=Array.from(document.querySelectorAll('.notice-toast__message')).map(e=>e.textContent).join(' ').toLowerCase();
const metadata={notice_categories:['fell behind','invalid','failed','unavailable','cancelled','stopped','overflow'].filter(c=>noticeText.includes(c))};
const users=Array.from(document.querySelectorAll('[data-turn-role="user"][data-request-id]'))
 .filter(e=>!prior.includes(e.dataset.requestId));
if(users.length!==1) return {...metadata,admitted:false, count:users.length};
const id=users[0].dataset.requestId;
const rows=Array.from(document.querySelectorAll('[data-turn-role="assistant"][data-request-id]'))
 .filter(e=>e.dataset.requestId===id);
if(rows.length!==1) return {...metadata,admitted:true, answer:false};
const row=rows[0], text=row.querySelector('.answer-stage__answer,.message__content');
return {...metadata,admitted:true, answer:true, outcome:row.dataset.turnOutcome,
 kind:row.dataset.contentKind, stage:row.dataset.stageStatus,
 nonblank:!!text?.textContent?.trim()&&!text.querySelector('.message__thinking'),
 source_links:row.querySelectorAll('.answer-stage__sources a').length};'''


def native_audio_owned(evidence):
    """Require measured native work and explicit zero counters, never absent keys.

    Capture admission and processing run concurrently, so this does not require
    equality of independently sampled counters during continuous silence capture.
    These hooks plus source inspection cover the native route; trace.dropped is
    only lost diagnostic observations, not an audio-loss measurement.
    """
    audio = evidence.get('native_audio')
    handlers = evidence.get('handler_call_metadata')
    if (not isinstance(audio, dict) or not isinstance(handlers, dict) or
            type(audio.get('schema')) is not int or audio['schema'] != 1):
        return False
    def counter(value):
        return type(value) is int and 0 <= value <= 2**53-1
    for name in ('admitted_frames', 'admitted_samples', 'processed_frames'):
        if not counter(audio.get(name)) or audio[name] <= 0:
            return False
    for name in ('pcm_events_emitted', 'overflows', 'stalls', 'capture_failures', 'processing_failures'):
        if not counter(audio.get(name)) or audio[name] != 0:
            return False
    for name in ('ingest_audio_frame', 'mark_silence', 'record_speech_activity', 'save_wake_clip'):
        stats = handlers.get(name)
        if not isinstance(stats, dict) or not counter(stats.get('calls')) or stats['calls'] != 0:
            return False
    return True


def accepted_flow(case, final, native, canonical_added_once, sources_preserved,
                  saw_inflight, service=None, require_voice=True):
    flow = max((e['flow'] for e in native['events']), default=0)
    events = [e for e in native['events'] if e['flow'] == flow]
    stages = {e['stage'] for e in events}
    required = {'generation_dispatched'}
    if require_voice:
        required |= {'source_samples_completed', 'wake_detected', 'final_asr_started', 'final_asr_completed'}
        if not native_audio_owned(native):
            return False
    if (not canonical_added_once or native['dropped'] != 0 or not required <= stages):
        return False
    if not final.get('nonblank') or final.get('canonical_ui_match') is not True:
        return False
    pages = 0 if case in ('quick', 'arithmetic') else (1 if case == 'short_research' else 2)
    if case == 'quick' and not final['quick_correct']:
        return False
    if pages and (native.get('readable_pages', 0) < pages or
                  native.get('tool_failures', 0) != 0 or not sources_preserved):
        return False
    if service is not None:
        if (final.get('offline_fixture_match') is not True or not saw_inflight or service['failures'] != 0 or
                not service['transcript_matches'] or not service['model_matches'] or
                not service['terminal']):
            return False
        for actual, expected in [('generation_requests', 'expected_model_rounds'),
                                 ('searches', 'expected_searches'),
                                 ('fetches', 'expected_fetches'),
                                 ('readable_sources', 'expected_readable_sources')]:
            if service[actual] != service[expected]:
                return False
        if sum(e['stage'] == 'generation_dispatched' for e in events) != service['expected_model_rounds']:
            return False
    return True


def answer_integrity(case, canonical, rendered, urls, offline, canonical_sources=None, profile=None):
    parts = canonical.rsplit('\n\nSources:\n', 1)
    if isinstance(canonical_sources, list) and canonical_sources:
        plain_canonical = canonical
        saved = [
            item['url'].strip()
            for item in canonical_sources
            if isinstance(item, dict) and isinstance(item.get('url'), str) and item['url'].strip()
        ]
        has_canonical_sources = bool(saved)
    elif len(parts) == 2:
        plain_canonical = parts[0]
        saved = parts[1].strip().split('\n')
        has_canonical_sources = True
    else:
        plain_canonical = parts[0]
        saved = []
        has_canonical_sources = False
    expected_urls = [] if profile or case == 'quick' else URLS
    sources_preserved = len(urls) == len(saved) and set(urls) == set(saved)
    if not profile and case != 'quick': sources_preserved &= bool(urls)
    if offline: sources_preserved &= set(urls) == set(expected_urls)
    return {'canonical_ui_match': bool(canonical.strip()) and (
                canonical.strip() == rendered.strip() or plain_canonical.strip() == rendered.strip()),
            'canonical_sources_expected': set(saved) == set(expected_urls) if offline else (case == 'quick' or bool(saved)),
            'sources_preserved': sources_preserved,
            'canonical_source_count': len(saved), 'ui_source_count': len(urls),
            'expected_source_count': len(expected_urls) if offline else None,
            'offline_fixture_match': offline and plain_canonical == (profile.answer if profile else expected_answer(case)) and
                has_canonical_sources == bool(expected_urls)}


def quick_answer_correct(text):
    return len(text.strip()) <= 128 and re.search(r'\b42\b', text) is not None


INSTALL_TYPED_OBSERVER = r'''
window.__voxBenchObserve?.retire();
const prior=new Set(arguments[0]);
const state={submit_ms:null,first_text_ms:null,first_word_ms:null,speech_observation_ms:null,terminal_ms:null,inflight_text:false,users:0};
const submit=()=>{if(state.submit_ms===null)state.submit_ms=performance.now()};
document.addEventListener('submit',submit,true);
const observe=()=>{
 const users=Array.from(document.querySelectorAll('[data-turn-role="user"][data-request-id]')).filter(e=>!prior.has(e.dataset.requestId));
 state.users=users.length;if(users.length!==1)return;
 const row=Array.from(document.querySelectorAll('[data-turn-role="assistant"][data-request-id]')).find(e=>e.dataset.requestId===users[0].dataset.requestId);
 if(!row)return;const terminal=['completed','cancelled','error','refused'].includes(row.dataset.turnOutcome);
 const text=row.querySelector('.answer-stage__answer,.message__content');
 if(state.first_text_ms===null&&text?.textContent?.trim()&&!text.querySelector('.message__thinking')){
  state.first_text_ms=performance.now();state.inflight_text=!terminal;
 }
 if(state.first_word_ms===null&&text&&!text.querySelector('.message__thinking')&&
    (/^\s*\S+\s/.test(text.textContent)||terminal&&text.textContent.trim()))state.first_word_ms=performance.now();
 if(terminal&&state.terminal_ms===null)state.terminal_ms=performance.now();
};
const observer=new MutationObserver(observe);observer.observe(document.body,{subtree:true,childList:true,characterData:true,attributes:true,attributeFilter:['data-turn-outcome','data-stage-status','data-content-kind']});
window.__voxBenchObserve={read:()=>({...state}),speech:value=>{
 if(state.submit_ms!==null&&state.speech_observation_ms===null&&value.events?.some(e=>e.stage==='reply_output_activated'))state.speech_observation_ms=performance.now();
},retire:()=>{observer.disconnect();document.removeEventListener('submit',submit,true)}};
'''


def fill_composer(driver, text):
    driver.execute_script('''const field=document.getElementById('promptComposer');
Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(field,arguments[0]);
field.dispatchEvent(new Event('input',{bubbles:true}));''', text)
    wait_for(lambda: driver.execute_script('return document.querySelector(".composer button[type=submit]")?.disabled===false'),
             5, 'typed_admission_ready')


def stage_breakdown(events):
    """All intervals here use one native monotonic origin; none are summed."""
    def first(name):
        return next((e['elapsed_us'] for e in events if e['stage'] == name), None)
    def between(start, end):
        a, b = first(start), first(end)
        return (b-a)/1000 if a is not None and b is not None and b >= a else None
    def speech_between(start, end):
        a = [(i, e.get('elapsed_us')) for i, e in enumerate(events) if e.get('stage') == start]
        b = [(i, e.get('elapsed_us')) for i, e in enumerate(events) if e.get('stage') == end]
        if len(a) != 1 or len(b) != 1:
            return None
        (ai, av), (bi, bv) = a[0], b[0]
        return (bv-av)/1000 if (ai <= bi and type(av) is int and type(bv) is int and 0 <= av <= bv) else None
    tools = []
    for event in events:
        if event['stage'] not in ('search_started', 'read_started'): continue
        finish = next((e for e in events if e['round'] == event['round'] and
                       e['elapsed_us'] >= event['elapsed_us'] and
                       e['stage'] in ('tool_completed', 'tool_failed')), None)
        tools.append({'operation': 'read' if event['stage'] == 'read_started' else 'search',
                      'round': event['round'], 'completed': bool(finish and finish['stage'] == 'tool_completed'),
                      'duration_ms': (finish['elapsed_us']-event['elapsed_us'])/1000 if finish else None})
    rounds = []
    for dispatch in (e for e in events if e['stage'] == 'generation_dispatched'):
        text = next((e for e in events if e['round'] == dispatch['round'] and e['stage'] == 'first_model_text'), None)
        rounds.append({'round': dispatch['round'], 'dispatch_to_first_text_ms':
                       (text['elapsed_us']-dispatch['elapsed_us'])/1000 if text and text['elapsed_us'] >= dispatch['elapsed_us'] else None})
    return {'prompt_to_first_model_word_ms': speech_between('prompt_command_started', 'first_model_word'),
            'prompt_to_first_model_text_ms': speech_between('prompt_command_started', 'first_model_text'),
            'prompt_to_synthesis_valid_ms': speech_between('prompt_command_started', 'reply_synthesis_valid'),
            'prompt_to_output_submitted_ms': speech_between('prompt_command_started', 'reply_output_submitted'),
            'prompt_to_output_activated_ms': speech_between('prompt_command_started', 'reply_output_activated'),
            'synthesis_request_to_valid_pcm_ms': speech_between('reply_synthesis_requested', 'reply_synthesis_valid'),
            'native_admission_ms': between('prompt_command_started', 'prompt_admitted'),
            'admission_to_dispatch_ms': between('prompt_admitted', 'generation_dispatched'),
            'provider_and_tools_ms': between('generation_dispatched', 'provider_run_completed'),
            'provider_to_canonical_commit_ms': between('provider_run_completed', 'canonical_turn_committed'),
            'commit_to_command_result_ready_ms': between('canonical_turn_committed', 'command_result_ready'),
            'model_rounds': rounds, 'tools': tools}


def typed_checks(case, integrity, native, service, added_once, observed, profile=None):
    stages = {e['stage'] for e in native['events']}
    checks = {'canonical_turn_added_once': added_once,
              'canonical_ui_answer_match': integrity['canonical_ui_match'],
              'canonical_sources_retained': integrity.get('canonical_sources_expected', False),
              'no_dropped_observations': native['dropped'] == 0,
              'normal_admission_and_commit': {'prompt_admitted', 'generation_dispatched', 'canonical_turn_committed'} <= stages,
              'tool_reads_completed': profile is not None or case == 'quick' or (native['readable_pages'] >= 1 and native['tool_failures'] == 0),
              'inflight_stream_presented': observed['inflight_text'],
              'ui_sources_preserved': integrity['sources_preserved']}
    if service is not None:
        checks['fixture_answer_exact'] = integrity['offline_fixture_match']
        checks['request_contract_and_history'] = service['model_matches'] and service['transcript_matches'] and service['history_matches']
        checks['fixture_operations_exact'] = (service['failures'] == 0 and service['terminal'] and
            all(service[a] == service[e] for a, e in [
                ('generation_requests', 'expected_model_rounds'), ('searches', 'expected_searches'),
                ('fetches', 'expected_fetches'), ('readable_sources', 'expected_readable_sources')]) and
            sum(e['stage'] == 'generation_dispatched' for e in native['events']) == service['expected_model_rounds'])
        checks['bounded_stream_gate'] = not service['stream_gate_timed_out']
    return checks


def typed_eligibility(checks):
    required = {'canonical_turn_added_once', 'canonical_ui_answer_match', 'canonical_sources_retained',
                'no_dropped_observations', 'normal_admission_and_commit', 'tool_reads_completed',
                'completed_text_outcome'}
    core = required <= checks.keys() and all(value is True for name, value in checks.items()
               if name not in ('ui_sources_preserved', 'inflight_stream_presented'))
    return core, core and checks.get('inflight_stream_presented') is True


def exercise_typed(driver, case, services=None, timeout=180, duplicate_click=False, profile=None):
    before = invoke(driver, 'get_app_snapshot')
    invoke(driver, 'benchmark_begin_flow')
    if services:
        if not services.wait_quiescent(): raise RuntimeError('Previous synthetic generation did not retire')
        services.expected_case = case
        services.expect_history(before['transcript'])
    baseline = driver.execute_script('return Array.from(document.querySelectorAll("[data-turn-role=user][data-request-id]")).map(e=>e.dataset.requestId)')
    driver.execute_script(INSTALL_TYPED_OBSERVER, baseline)
    fill_composer(driver, (profile.cases if profile else CASES)[case])
    host_started = time.perf_counter()
    button = driver.find_element('css selector', '.composer button[type=submit]')
    if duplicate_click:
        # Two browser form admissions, not direct native requests or a Stop click.
        driver.execute_script('const form=document.querySelector(".composer");form.requestSubmit();form.requestSubmit()')
    else:
        button.click()
    deadline = time.monotonic()+timeout
    final = None
    while time.monotonic() < deadline:
        observation = driver.execute_script('return window.__voxBenchObserve.read()')
        if services and observation['inflight_text']: services.release_stream()
        if profile:
            invoke(driver, 'benchmark_snapshot', observer=True)
        state = driver.execute_script(OBSERVE, baseline)
        if state.get('outcome') == 'completed' and state.get('stage') == 'completed' and state.get('kind') == 'text':
            final = state
            break
        if state.get('outcome') in ('error', 'failed', 'cancelled', 'refused'):
            break
        time.sleep(0.05)
    host_completed = time.perf_counter()
    if profile:
        # Canonical text completion and physical local retirement are distinct.
        native = invoke(driver, 'benchmark_snapshot', observer=True)
        while time.monotonic() < deadline and not arithmetic_speech_passed(native.get('reply_speech')):
            if native.get('reply_speech', {}).get('failed', 0) and native['reply_speech'].get('retired'):
                break
            time.sleep(0.05)
            native = invoke(driver, 'benchmark_snapshot', observer=True)
    observation = driver.execute_script('return window.__voxBenchObserve.read()')
    driver.execute_script('window.__voxBenchObserve.retire()')
    if services:
        services.release_stream()
        if not services.wait_quiescent(): raise RuntimeError('Synthetic generation did not retire')
    after = invoke(driver, 'get_app_snapshot')
    native = native if profile else invoke(driver, 'benchmark_snapshot')
    added_once = len(after['transcript']) == len(before['transcript'])+1 and observation['users'] == 1
    turn = after['transcript'][-1] if added_once else {}
    text = turn.get('answer', {}).get('content', {}).get('text', '')
    delivered = driver.execute_script('''const id=arguments[0];const row=Array.from(document.querySelectorAll('[data-turn-role=assistant][data-request-id]')).find(e=>e.dataset.requestId===id);
return {text:row?.querySelector('.answer-stage__answer,.message__content')?.textContent||'',urls:Array.from(row?.querySelectorAll('.answer-stage__sources a')||[]).map(a=>a.href)};''', turn.get('request_id'))
    integrity = answer_integrity(case, text, delivered['text'], delivered['urls'], services is not None, turn.get('sources'), profile)
    service = services.snapshot() if services else None
    checks = typed_checks(case, integrity, native, service, added_once, observation, profile)
    checks['completed_text_outcome'] = final is not None
    if profile:
        checks['local_speech_completed_and_retired'] = arithmetic_speech_passed(native.get('reply_speech'))
    if case == 'quick': checks['quick_answer_correct'] = quick_answer_correct(text)
    # The user deferred source-list projection repair, not canonical integrity.
    core, qualification = typed_eligibility(checks)
    def elapsed(end):
        start = observation['submit_ms']
        value = observation.get(end)
        return value-start if start is not None and value is not None and value >= start else None
    return {'case': case, 'coverage': 'typed-workflow', 'workflow_completed': core,
            'qualification_eligible': qualification, 'all_checks_passed': all(checks.values()),
            'checks': checks, 'defects': [name for name, passed in checks.items() if not passed],
            'ui_submit_to_first_text_dom_ms': elapsed('first_text_ms'),
            'ui_submit_to_first_complete_word_dom_ms': elapsed('first_word_ms'),
            'ui_submit_to_speech_observation_ms': elapsed('speech_observation_ms') if profile else None,
            'speech_observation_boundary': 'WebView payload receipt; includes IPC/poll observer overhead; not exact audio' if profile else None,
            'speech_identity': native.get('speech_identity') if profile else None,
            'reply_speech': native.get('reply_speech') if profile else None,
            'ui_submit_to_terminal_dom_ms': elapsed('terminal_ms'),
            'host_click_to_completed_observation_ms': (host_completed-host_started)*1000,
            'native_breakdown': stage_breakdown(native['events']),
            'native_events': native['events'], 'generation_counts': native['generation'],
            'readable_pages': native['readable_pages'], 'integrity': integrity,
            'service_metadata': service, 'canonical_turns_before': len(before['transcript']),
            'canonical_turns_after': len(after['transcript']), 'dropped': native['dropped']}


def typed_empty_admission(driver):
    before = invoke(driver, 'get_app_snapshot')
    count = invoke(driver, 'benchmark_snapshot')['generation']['generation_attempts']
    driver.execute_script('''const field=document.getElementById('promptComposer');
Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(field,'');
field.dispatchEvent(new Event('input',{bubbles:true}));document.querySelector('.composer').requestSubmit();''')
    time.sleep(0.2)
    after = invoke(driver, 'get_app_snapshot')
    return (before['transcript'] == after['transcript'] and after.get('active_request') is None and
            invoke(driver, 'benchmark_snapshot')['generation']['generation_attempts'] == count)


def typed_cancellation(driver, services):
    before = invoke(driver, 'get_app_snapshot')
    before_generations = invoke(driver, 'benchmark_snapshot')['generation']['generation_attempts']
    invoke(driver, 'benchmark_begin_flow')
    services.expected_case = 'quick'
    services.expect_history(before['transcript'])
    fill_composer(driver, CASES['quick'])
    driver.find_element('css selector', '.composer button[type=submit]').click()
    try:
        wait_for(lambda: services.snapshot()['first_delta_sent'], 5, 'cancel_after_dispatch')
        stop = wait_for(lambda: driver.find_elements('css selector', '.composer button[aria-label=Stop]'),
                        5, 'cancel_control_available')[0]
        stop.click()
        def settled():
            snapshot = invoke(driver, 'get_app_snapshot')
            return snapshot if snapshot.get('active_request') is None and snapshot['runtime_phase'] == 'sleeping' else None
        after = wait_for(settled, 5, 'native_cancellation_settlement')
        if before['transcript'] != after['transcript'] or services.snapshot()['generation_requests'] != 1:
            return False
        services.release_stream()
        if not services.wait_quiescent(): return False
        # A cancellation receipt precedes the delayed provider completion. Check
        # again after it is released/drained; server quiescence alone is not proof
        # that the application ignored a late result.
        deadline = time.monotonic()+CANCEL_POST_RELEASE_SECONDS
        while True:
            after = invoke(driver, 'get_app_snapshot')
            generations = invoke(driver, 'benchmark_snapshot')['generation']['generation_attempts']
            if (after['transcript'] != before['transcript'] or after.get('active_request') is not None or
                    after['runtime_phase'] != 'sleeping' or generations != before_generations+1 or
                    services.snapshot()['generation_requests'] != 1):
                return False
            if time.monotonic() >= deadline: return True
            time.sleep(0.05)
    finally:
        services.release_stream()
        if not services.wait_quiescent(): raise RuntimeError('Cancelled synthetic transport did not retire')


def workflow_safety_passed(checks):
    return all(checks.get(name) is True for name in WORKFLOW_SAFETY_CHECKS)


def finding_dispositions(rows, relaunch, safety):
    deferred, failures = set(), set()
    for row in rows:
        for name, passed in row['checks'].items():
            if passed is True: continue
            if name == 'ui_sources_preserved': deferred.add('ui_source_projection')
            else: failures.add(name)
    if relaunch and not relaunch['canonical_history_retained']: deferred.add('desktop_history_relaunch')
    for name in WORKFLOW_SAFETY_CHECKS:
        if name in safety and safety[name] is not True: failures.add('safety:'+name)
    return sorted(deferred), sorted(failures)


def start_typed_app(args, env):
    global PHASE, APP_PID
    from selenium import webdriver
    from selenium.webdriver.common.options import BaseOptions
    class Options(BaseOptions):
        def to_capabilities(self): return self._caps
        @property
        def default_capabilities(self): return {'browserName': 'wry'}
    APP_PID = None
    port, native_port = free_port(), free_port()
    PHASE = 'webdriver_starting'
    process = subprocess.Popen([str(args.tools/'bin/tauri-driver'), '--port', str(port),
        '--native-port', str(native_port), '--native-driver',
        str(args.tools/'webkit/extracted/usr/bin/WebKitWebDriver')], env=env,
        start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    driver = None
    try:
        wait_for(lambda: can_connect(port), 15, 'webdriver_startup')
        options = Options()
        options.set_capability('tauri:options', {'application': str(args.binary.resolve())})
        PHASE = 'typed_app_launch'
        started = time.perf_counter()
        driver = webdriver.Remote(command_executor=f'http://127.0.0.1:{port}', options=options)
        driver.set_script_timeout(30)
        def ready():
            native = invoke(driver, 'benchmark_snapshot')
            return (native['entry'] == 'typed' and not native['capture_active'] and
                    native['phase'] == 'sleeping' and
                    driver.execute_script('return document.getElementById("composerInstantSelect")?.value') == 'custom-luna-none')
        wait_for(ready, 30, 'typed_model_ready_without_capture')
        snapshot = invoke(driver, 'get_app_snapshot')
        if snapshot['preferences'].get('web_enabled') is not True:
            raise RuntimeError('Web permission does not match the typed workflow')
        return driver, process, (time.perf_counter()-started)*1000
    except Exception as error:
        print(json.dumps({'stage': PHASE, 'failure_category': type(error).__name__,
                          'driver_exit_code': process.poll(), 'app_pid_observed': APP_PID is not None}))
        shutdown_owned(driver, process)
        raise


def execute_typed(args):
    global PHASE
    run = args.output.resolve()
    service = FakeServices(gated_stream=True) if not args.live else None
    if service: service.__enter__()
    driver = process = None
    rows, supporting = [], []
    safety, relaunch = {}, {}
    try:
        reservation = run/'reservation.json' if args.live else None
        if args.live:
            validate_offline_evidence(args.offline_evidence, args.binary, {}, {}, 'typed')
            reserved = reserve_live_budget(args.ledger, reservation)
        else:
            reserved = 0
        env = isolated_environment(run, args.config if args.live else args.config.resolve(), service.generation_endpoint if service else None,
                                    args.live, reservation, entry='typed')
        if service: env['VOXGOLEM_BENCH_MCP_ENDPOINT'] = service.mcp_endpoint
        driver, process, launch_ms = start_typed_app(args, env)
        for case in args.cases:
            PHASE = 'typed_workflow'
            result = exercise_typed(driver, case, service, args.flow_timeout)
            rows.append(result)
            if not result['workflow_completed']: break
        if args.extended and len(rows) == len(CASES) and all(row['workflow_completed'] for row in rows):
            PHASE = 'offline_context_and_safety'
            safety['empty_admission'] = typed_empty_admission(driver)
            warm = exercise_typed(driver, 'quick', service, args.flow_timeout)
            safety['warm_context'] = warm['workflow_completed'] and warm['service_metadata']['history_turns_observed'] >= 3
            supporting.append({'scenario': 'warm_followup', **warm})
            history_ok = True
            for index in range(14):
                seeded = exercise_typed(driver, 'quick', service, args.flow_timeout)
                history_ok &= seeded['workflow_completed'] and seeded['service_metadata']['history_matches']
                supporting.append({'scenario': 'history_seed', 'index': index,
                    **{k: v for k, v in seeded.items() if k not in ('native_events', 'service_metadata')}})
                if not history_ok: break
            followup = exercise_typed(driver, 'long_research', service, args.flow_timeout) if history_ok else None
            safety['longer_history'] = bool(followup and followup['workflow_completed'] and
                followup['service_metadata']['history_turns_observed'] >= 18)
            if followup: supporting.append({'scenario': 'long_history_followup', **followup})
            duplicate = exercise_typed(driver, 'quick', service, args.flow_timeout, duplicate_click=True)
            safety['duplicate_ui_submission'] = duplicate['workflow_completed'] and duplicate['service_metadata']['generation_requests'] == 1
            supporting.append({'scenario': 'duplicate_ui_submission', **duplicate})
            safety['cancellation_retirement'] = typed_cancellation(driver, service)
            previous = invoke(driver, 'get_app_snapshot')
            previous_requests = service.snapshot()['generation_requests']
            prior_exit = shutdown_owned(driver, process)
            driver = process = None
            if not prior_exit['app_exit_before_group_termination_observed']:
                raise RuntimeError('Pre-relaunch application did not exit')
            driver, process, relaunch_ms = start_typed_app(args, env)
            restored = invoke(driver, 'get_app_snapshot')
            safety['relaunch_preferences'] = previous['preferences'] == restored['preferences']
            safety['relaunch_without_generation'] = (service.snapshot()['generation_requests'] == previous_requests and
                invoke(driver, 'benchmark_snapshot')['generation']['generation_attempts'] == 0)
            relaunch = {'launch_ms': relaunch_ms, 'canonical_turns_before': len(previous['transcript']),
                'canonical_turns_after': len(restored['transcript']),
                'canonical_history_retained': previous['transcript'] == restored['transcript'],
                'preferences_retained': safety['relaunch_preferences'],
                'no_new_generation': safety['relaunch_without_generation']}
        safety['passed'] = workflow_safety_passed(safety)
        cleanup = shutdown_owned(driver, process)
        driver = process = None
        summary = {'schema': 2, 'mode': 'live' if args.live else 'offline', 'entry': 'typed',
            'coverage': 'typed-workflow', 'voice_tested': False, 'speech_assets_loaded': False,
            'physical_microphone_tested': False, 'windows_or_phone_acceptance': False,
            'diagnostic_only': False, 'workflow_identity': typed_identity(),
            'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
            'harness_sha256': harness_identity(), 'launch_to_typed_ready_observed_ms': launch_ms,
            'rows': rows, 'supporting_rows': supporting, 'safety_checks': safety,
            'relaunch': relaunch, 'cleanup': cleanup,
            'live_batch_reserved': LIVE_BATCH_ALLOWANCE if args.live else 0, 'assignment_reserved': reserved,
            'cancellation_post_release_observation_ms': CANCEL_POST_RELEASE_SECONDS*1000 if args.extended else None,
            'offline_timing_note': 'Gated synthetic SSE delay is measured separately; not real-provider latency' if service else None}
        completed = (len(rows) == len(args.cases) and all(row['workflow_completed'] for row in rows) and
                     cleanup['app_exit_before_group_termination_observed'])
        summary['workflow_completed'] = completed
        summary['qualified_for_live'] = (not args.live and completed and len(rows) == len(CASES) and
            all(row['qualification_eligible'] for row in rows) and safety['passed'])
        summary['deferred_findings'], summary['failed_checks'] = finding_dispositions(rows+supporting, relaunch, safety)
        private_json(run/'report.json', summary)
        prune_owned_reports(run.parent)
        print(json.dumps({'workflow_completed': completed, 'qualified_for_live': summary['qualified_for_live'],
            'coverage': 'typed-workflow', 'completed_cases': len(rows), 'deferred_findings': summary['deferred_findings']}))
        return 0 if completed else 1
    finally:
        if driver or process: shutdown_owned(driver, process)
        if service: service.__exit__(None, None, None)


INSTALL_NATIVE_OBSERVER_HOLD = '''
if(window.__voxNativeHold)throw new Error('hold already installed');
const api=window.__TAURI_INTERNALS__, original=api.invoke;
let pending=null,held=0,extra=0,released=false;
function wrapper(...args){
 if(args[0]!=='read_desktop_runtime'||released)return original.apply(this,args);
 held++;
 if(held>1){extra++;return Promise.reject(new Error('second held read'));}
 const receiver=this;
 return new Promise((resolve,reject)=>{pending=()=>{
  try{Promise.resolve(original.apply(receiver,args)).then(resolve,reject);}catch(e){reject(e);}
 };});
}
api.invoke=wrapper;
if(api.invoke!==wrapper)throw new Error('invoke is not replaceable');
window.__voxNativeHold={read:()=>({read_actually_held:held===1,held_reads:held,extra_reads:extra}),
 release:()=>{released=true;api.invoke=original;const run=pending;pending=null;if(run)run();
 return {restored:api.invoke===original};}};
'''
RELEASE_NATIVE_OBSERVER = '''const hold=window.__voxNativeHold;
if(!hold)return {restored:true};const result=hold.release();delete window.__voxNativeHold;return result;'''


def exercise(driver, case, timeout=180, wake_diagnostics=False, services=None, poll_interval=0.1,
             entry='voice', hold_native_observer=False):
    if hold_native_observer and (entry != 'voice' or case != 'quick' or services is None):
        raise ValueError('Observer hold requires offline quick voice')
    try:
        return _exercise(driver, case, timeout, wake_diagnostics, services, poll_interval,
                         entry, hold_native_observer)
    finally:
        if hold_native_observer:
            driver.execute_script(RELEASE_NATIVE_OBSERVER)


def _exercise(driver, case, timeout, wake_diagnostics, services, poll_interval, entry, hold_native_observer):
    page_at_start = driver.execute_script('return {visibility:document.visibilityState, focused:document.hasFocus()}')
    baseline = driver.execute_script('return Array.from(document.querySelectorAll("[data-turn-role=user][data-request-id]")).map(e=>e.dataset.requestId)')
    before = invoke(driver, 'get_app_snapshot')
    hold = None
    if hold_native_observer:
        driver.execute_script(INSTALL_NATIVE_OBSERVER_HOLD)
    if wake_diagnostics:
        invoke(driver, 'set_wake_diagnostics', {'enabled': True})
    if entry == 'typed-control':
        driver.execute_script('''const field=document.getElementById('promptComposer');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(field,arguments[0]);field.dispatchEvent(new Event('input',{bubbles:true}));''', CASES[case])
        wait_for(lambda: driver.execute_script('return !document.querySelector(".composer button[type=submit]")?.disabled'), 5, 'typed_control_admission')
    started = time.perf_counter()
    deadline = time.monotonic() + timeout
    try:
        if entry == 'voice':
            invoke(driver, 'benchmark_queue', {'fixture': case})
        else:
            driver.find_element('css selector', '.composer button[type=submit]').click()
    except RuntimeError:
        return {'case': case, 'passed': False, 'failure': 'source_retired_before_completion',
                'ui_state': ui_state(driver),
                'page_at_start': page_at_start,
                'page_at_outcome': driver.execute_script('return {visibility:document.visibilityState, focused:document.hasFocus()}'),
                'service_metadata': services.snapshot() if services else None,
                'native': invoke(driver, 'benchmark_snapshot')}
    source_end_observed = time.perf_counter()
    if hold_native_observer:
        hold = observe_native_completion_held(driver, before, baseline, services,
                                              min(deadline, time.monotonic()+10))
        hold.update(driver.execute_script('return window.__voxNativeHold.read()'))
        hold['invoke_restored'] = driver.execute_script(RELEASE_NATIVE_OBSERVER)['restored']
    first = None
    final = None
    saw_inflight = False
    next_diagnostic = time.monotonic() + 2
    wake_metrics = {'maximum_score': 0, 'maximum_peak': 0, 'sample_position': 0}
    failure = 'flow_deadline'
    notices = set()
    while time.monotonic() < deadline:
        if wake_diagnostics and time.monotonic() >= next_diagnostic:
            diagnostics = invoke(driver, 'get_wake_diagnostics')
            for event in diagnostics.get('events', []):
                if event.get('kind') == 'score':
                    wake_metrics['maximum_score'] = max(wake_metrics['maximum_score'], event['score'])
                    wake_metrics['maximum_peak'] = max(wake_metrics['maximum_peak'], event['peak'])
                    wake_metrics['sample_position'] = max(wake_metrics['sample_position'], event['sample_position'])
            next_diagnostic = time.monotonic() + 2
            current_native = invoke(driver, 'benchmark_snapshot')
            if not current_native['capture_active']:
                failure = 'capture_retired_before_completion'
                break
        state = driver.execute_script(OBSERVE, baseline)
        notices.update(state.get('notice_categories', []))
        now = time.perf_counter()
        if state.get('outcome') in ('error', 'failed', 'cancelled', 'refused'):
            raise RuntimeError('Correlated turn failed')
        if services is not None and services.snapshot().get('failures', 0) > 0:
            failure = 'service_contract_failure'
            break
        if state.get('nonblank') and first is None:
            first = now
        if state.get('nonblank') and state.get('outcome') not in ('completed', 'cancelled', 'error', 'refused'):
            saw_inflight = True
        if state.get('outcome') == 'completed' and state.get('stage') == 'completed' and state.get('kind') == 'text':
            final = state
            break
        time.sleep(poll_interval)
    if final is None or first is None:
        # Fixed numeric/category evidence is safe even when a preceding stage failed.
        return {'case': case, 'passed': False, 'failure': failure,
                'native_observer_hold': hold, 'diagnostic_success': False,
                'ui_state': ui_state(driver),
                'page_at_start': page_at_start,
                'page_at_outcome': driver.execute_script('return {visibility:document.visibilityState, focused:document.hasFocus()}'),
                'wake_diagnostic_metadata': wake_metrics if wake_diagnostics else None,
                'notice_categories': sorted(notices),
                'service_metadata': services.snapshot() if services else None,
                'native': invoke(driver, 'benchmark_snapshot')}
    after = invoke(driver, 'get_app_snapshot')
    native = invoke(driver, 'benchmark_snapshot')
    flow = max((e['flow'] for e in native['events']), default=0)
    events = [e for e in native['events'] if e['flow'] == flow]
    canonical_added_once = len(after['transcript']) == len(before['transcript'])+1
    text = after['transcript'][-1].get('answer', {}).get('content', {}).get('text', '') if canonical_added_once else ''
    # Compare internally; neither canonical text nor source addresses enter reports.
    request_id = after['transcript'][-1].get('request_id') if canonical_added_once else None
    delivered = driver.execute_script('''const id=arguments[0];
const rows=Array.from(document.querySelectorAll('[data-turn-role="assistant"][data-request-id]')).filter(e=>e.dataset.requestId===id);
if(rows.length!==1)return {text:'',urls:[]};
const rendered=rows[0].querySelector('.answer-stage__answer,.message__content')?.textContent||'';
const urls=Array.from(rows[0].querySelectorAll('.answer-stage__sources a')).map(a=>a.href);
return {text:rendered,urls};''', request_id)
    integrity = answer_integrity(case, text, delivered['text'], delivered['urls'], services is not None,
                                 after['transcript'][-1].get('sources') if canonical_added_once else None,
                                 None)
    final.update(integrity)
    final['quick_correct'] = quick_answer_correct(text)
    sources_preserved = integrity['sources_preserved']
    service_metadata = services.snapshot() if services else None
    passed = accepted_flow(case, final, native, canonical_added_once, sources_preserved,
                           saw_inflight, service_metadata, entry == 'voice')
    if hold is not None:
        hold['canonical_prefix_preserved'] = after['transcript'][:-1] == before['transcript']
        hold['canonical_ui_reconciled_once'] = canonical_added_once and final['canonical_ui_match'] and driver.execute_script('''const prior=arguments[0],id=arguments[1];return Array.from(document.querySelectorAll('[data-turn-role=user][data-request-id]')).filter(e=>!prior.includes(e.dataset.requestId)).length===1&&document.querySelectorAll('[data-turn-role=assistant][data-request-id]').length===prior.length+1&&Array.from(document.querySelectorAll('[data-turn-role=user][data-request-id]')).filter(e=>e.dataset.requestId===id).length===1;''', baseline, request_id)
        hold['normal_native_flow_and_exact_operations'] = accepted_flow(
            case, final, native, canonical_added_once, sources_preserved, True, service_metadata, True)
        hold['no_late_duplicate_admission'] = True
        retirement_deadline = min(deadline, time.monotonic()+0.5)
        while time.monotonic() < retirement_deadline:
            settled = invoke(driver, 'get_app_snapshot')
            metadata = services.snapshot()
            ui = driver.execute_script(OBSERVE, baseline)
            current_native = invoke(driver, 'benchmark_snapshot')
            if (settled['transcript'] != after['transcript'] or settled.get('active_request') is not None or
                    metadata['generation_requests'] != service_metadata['expected_model_rounds'] or
                    metadata['failures'] != 0 or ui.get('outcome') != 'completed' or
                    ui_state(driver)['user_rows'] != len(baseline)+1 or
                    current_native['generation'] != native['generation']):
                hold['no_late_duplicate_admission'] = False
                break
            time.sleep(0.05)
        hold['post_release_observation_completed'] = time.monotonic() < deadline
    return {'case': case, 'passed': bool(passed) and not hold_native_observer,
            'native_observer_hold': hold,
            'diagnostic_success': hold is not None and all(value is True for key, value in hold.items() if key not in ('held_reads', 'extra_reads')) and hold['held_reads'] == 1 and hold['extra_reads'] == 0,
            'page_at_start': page_at_start,
            'page_at_outcome': driver.execute_script('return {visibility:document.visibilityState, focused:document.hasFocus()}'),
            'queue_to_first_text_observed_ms': (first-started)*1000,
            'queue_to_completed_observed_ms': (now-started)*1000,
            'source_end_receipt_to_first_text_observed_ms': (first-source_end_observed)*1000 if entry == 'voice' else None,
            'source_end_receipt_to_completed_observed_ms': (now-source_end_observed)*1000 if entry == 'voice' else None,
            'source_queue_completion_receipt_ms': (source_end_observed-started)*1000 if entry == 'voice' else None,
            'canonical_turn_added_once': len(after['transcript']) == len(before['transcript'])+1,
            'canonical_sources_preserved': sources_preserved,
            'canonical_ui_match': final['canonical_ui_match'],
            'canonical_source_count': final['canonical_source_count'],
            'ui_source_count': final['ui_source_count'],
            'expected_source_count': final['expected_source_count'],
            'offline_fixture_match': final['offline_fixture_match'] if services else None,
            'observed_inflight_text': saw_inflight,
            'readable_pages': native.get('readable_pages', 0),
            'service_metadata': service_metadata,
            'source_links': final['source_links'], 'native_events': events,
            'native_audio': native.get('native_audio'),
            'handler_call_metadata': native.get('handler_call_metadata'),
            'native_audio_owned': native_audio_owned(native) if entry == 'voice' else None,
            'dropped': native['dropped']}


def observe_native_completion_held(driver, before, baseline, services, deadline):
    result = {'canonical_completed_while_held': False, 'ui_user_row_absent_before_release': False,
              'ui_pending_row_absent_before_release': False,
              'exact_generation_count_while_held': False}
    while time.monotonic() < deadline:
        state = driver.execute_script('return window.__voxNativeHold.read()')
        result.update(state)
        if state['extra_reads']:
            break
        snapshot = invoke(driver, 'get_app_snapshot')
        service = services.snapshot()
        complete = (snapshot['transcript'][:-1] == before['transcript'] and
                    len(snapshot['transcript']) == len(before['transcript'])+1 and
                    snapshot.get('active_request') is None and service['terminal'])
        if complete:
            result['canonical_completed_while_held'] = state['read_actually_held']
            result['ui_user_row_absent_before_release'] = driver.execute_script(OBSERVE, baseline).get('count') == 0
            result['ui_pending_row_absent_before_release'] = driver.execute_script('''return document.querySelectorAll('[data-turn-role=assistant]').length===arguments[0]&&document.querySelectorAll('[data-turn-role=user]').length===arguments[0];''', len(baseline)) is True
            result['exact_generation_count_while_held'] = (service['generation_requests'] == service['expected_model_rounds'] == 1 and service['failures'] == 0)
            break
        time.sleep(0.1)
    return result


def execute(args, platform=None):
    global PHASE, APP_PID
    if args.entry == 'typed-speech':
        if platform is None:
            raise ValueError('Typed speech requires Windows effects')
        import windows_effects
        windows_effects.validate_options(args)
    if args.entry not in ('voice', 'typed', 'typed-control', 'typed-speech'):
        raise ValueError('Desktop synthetic-voice arithmetic profile retired')
    validate_benchmark_binary(args.binary)
    if getattr(args, 'shared_suite', False):
        import importlib.util
        spec = importlib.util.spec_from_file_location('typed_benchmark_desktop', ROOT/'tools/benchmark/desktop.py')
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module.execute_inside(args, SimpleNamespace(**globals()))
    if args.entry == 'typed':
        return execute_typed(args)
    from selenium import webdriver
    from selenium.webdriver.common.options import BaseOptions

    class Options(BaseOptions):
        def to_capabilities(self):
            return self._caps
        @property
        def default_capabilities(self):
            return {'browserName': 'wry'}

    run = args.output.resolve()
    typed_speech = args.entry == 'typed-speech'
    profile = arithmetic_profile() if args.entry == 'typed-speech' else None
    effects_api = SimpleNamespace(**globals()) if platform else None
    if platform:
        fixtures, speech_identity, identity = platform.prepare(profile, effects_api)
    else:
        fixtures = generate_fixtures(run / 'fixtures', voice=args.voice, wake_phrase=args.wake_phrase, profile=profile)
    if sum(fixture['byte_count'] for fixture in fixtures.values()) > 16*1024*1024:
        raise ValueError('Synthetic fixture bundle exceeds its byte budget')
    if not platform:
        source_config = live_auth_source_config(run, args.config) if args.live else args.config.resolve()
        speech_identity = speech_configuration_identity(source_config)
        identity = None
    services = FakeServices(profile=profile, gated_stream=typed_speech) if not args.live else None
    if services:
        services.__enter__()
    driver = None
    process = None
    rows = []
    try:
        reservation = run / 'reservation.json' if args.live else None
        if args.live:
            validate_offline_evidence(args.offline_evidence, args.binary, fixtures, speech_identity)
            reserved = reserve_live_budget(args.ledger, reservation)
        else:
            reserved = 0
        env = platform.environment(services.generation_endpoint, effects_api) if platform else isolated_environment(
            run, source_config, services.generation_endpoint if services else None,
            args.live, reservation, entry=args.entry)
        if services:
            env['VOXGOLEM_BENCH_MCP_ENDPOINT'] = services.mcp_endpoint
        if args.driver_free_diagnostic:
            return driver_free_diagnostic(args, run, fixtures, env, services)
        port, native_port = free_port(), free_port()
        PHASE = 'webdriver_starting'
        if platform:
            process = platform.start(env, port, native_port)
        else:
            process = subprocess.Popen([str(args.tools/'bin/tauri-driver'), '--port', str(port),
                 '--native-port', str(native_port), '--native-driver',
                 str(args.tools/'webkit/extracted/usr/bin/WebKitWebDriver')],
                 env=env, start_new_session=True, stdout=subprocess.DEVNULL,
                 stderr=subprocess.DEVNULL)
        wait_for(lambda: can_connect(port), 15, 'webdriver_startup')
        options = Options()
        options.set_capability('tauri:options', platform.application_options() if platform else
                               {'application': str(args.binary.resolve())})
        PHASE = 'app_launch'
        launch = time.perf_counter()
        if platform:
            APP_PID = None
        driver = webdriver.Remote(command_executor=f'http://127.0.0.1:{port}', options=options)
        driver.set_script_timeout(75)
        if platform:
            invoke(driver, 'benchmark_snapshot')
            platform.observe_app(APP_PID)
        PHASE = 'automatic_capture_start'
        if not typed_speech:
            wait_for(lambda: invoke(driver, 'benchmark_snapshot').get('capture_active'),
                     60, 'automatic_capture_start')
        launch_ms = (time.perf_counter()-launch)*1000
        if typed_speech:
            wait_for(lambda: driver.execute_script('return document.getElementById("composerInstantSelect")?.value') == 'custom-luna-none',
                     30, 'typed_composer_ready')
        selected = driver.execute_script('return document.getElementById("composerInstantSelect")?.value')
        if selected != 'custom-luna-none':
            raise RuntimeError('Requested model was not selected')
        if args.event_loop_diagnostic:
            driver.execute_script('''const metrics={ticks:0,max_tick_gap_ms:0};window.__benchLoopDiagnostic=metrics;let previous=performance.now();setInterval(()=>{const now=performance.now();metrics.max_tick_gap_ms=Math.max(metrics.max_tick_gap_ms,now-previous);metrics.ticks++;previous=now;},50);''')
        for case in args.cases:
            if typed_speech:
                PHASE = 'typed_speech_flow'
                result = exercise_typed(driver, case, services, args.flow_timeout, profile=profile)
                result['checks']['speech_identity_matches_descriptor'] = speech_identity_matches(result.get('speech_identity'), speech_identity)
                result['passed'] = result['workflow_completed'] and result['checks']['speech_identity_matches_descriptor']
                result['workflow_completed'] = result['passed']
                result['qualification_eligible'] = False
                result['all_checks_passed'] = all(result['checks'].values())
                result['defects'] = [name for name, passed in result['checks'].items() if not passed]
                rows.append(result)
                break
            PHASE = 'voice_flow'
            if services:
                services.expected_case = case
            wait_for(lambda: invoke(driver, 'benchmark_snapshot')['phase'] == 'sleeping',
                     15, 'idle_before_input')
            result = exercise(driver, case, args.flow_timeout, args.wake_diagnostics, services, args.poll_interval, args.entry, args.hold_native_observer)
            if args.entry == 'typed-control':
                result['typed_control_passed'] = result['passed']
                result['passed'] = False
            if args.event_loop_diagnostic:
                result['event_loop_diagnostic'] = driver.execute_script('return window.__benchLoopDiagnostic')
            result['launch_to_capture_observed_ms'] = launch_ms if not rows else None
            rows.append(result)
            if not result.get('typed_control_passed', result['passed']):
                break
        summary = {'schema': 3 if profile else 1, 'mode': 'live' if args.live else 'offline',
                   'requested_model': 'custom-luna-none', 'effort': 'none',
                   'entry': args.entry,
                   'software_input': True, 'physical_microphone_tested': False,
                   'diagnostic_only': args.hold_native_observer or args.entry != 'voice' or args.wake_diagnostics or args.event_loop_diagnostic or args.poll_interval != 0.1,
                   'hold_native_observer': args.hold_native_observer,
                   'wake_score_diagnostics': args.wake_diagnostics,
                   'browser_call_probes': args.event_loop_diagnostic,
                   'windows_or_phone_acceptance': False,
                   'gpu_runtime_devices_available': any(Path(p).exists() for p in ['/dev/dri', '/dev/dxg']),
                   'shared_memory_available': Path('/dev/shm').is_dir(),
                   'private_session_bus': True, 'private_process_namespace': True,
                   'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
                   'harness_sha256': harness_identity(),
                   'fixture_metadata': fixtures, 'rows': rows,
                   'speech_identity': speech_identity,
                   'live_batch_reserved': LIVE_BATCH_ALLOWANCE if args.live else 0,
                   'assignment_reserved': reserved,
                   'nominal_observation_interval_ms': args.poll_interval*1000}
        cleanup = platform.shutdown(driver, process) if platform else shutdown_owned(driver, process)
        driver = None
        process = None
        summary['cleanup'] = cleanup
        if profile:
            summary['profile_identity'] = identity
            summary['isolated_config_sha256'] = hashlib.sha256((run/'config/config.toml').read_bytes()).hexdigest()
            summary['qualified_for_live'] = False
            summary['acoustic_output'] = 'unverified'
            summary['output_route'] = 'wasapi-render-diagnostic'
            summary['offline_profile_qualified'] = False
        summary['diagnostic_success'] = (args.hold_native_observer and len(rows) == 1 and
            rows[0].get('diagnostic_success') is True and cleanup['app_exit_before_group_termination_observed'])
        if args.hold_native_observer:
            summary['qualified_for_live'] = False
        if platform:
            platform.annotate(summary)
        private_json(run/'report.json', summary)
        prune_owned_reports(run.parent)
        passed = (not summary['diagnostic_only'] and len(rows)==len(args.cases) and
                  all(r['passed'] for r in rows) and cleanup['app_exit_before_group_termination_observed'])
        if profile:
            passed = passed and summary['offline_profile_qualified']
        print(json.dumps({'passed': passed,
                          'workflow_completed': summary.get('workflow_completed'),
                          'diagnostic_success': summary['diagnostic_success'],
                          'mode': summary['mode'], 'completed_flows': len(rows)}))
        return 0 if passed else 1
    except Exception:
        if driver is not None and PHASE == 'automatic_capture_start':
            try:
                startup = invoke(driver, 'get_startup_state')
                native = invoke(driver, 'benchmark_snapshot')
                print(json.dumps({'stage': PHASE, 'startup_kind': startup.get('kind'),
                    'voice_input_available': startup.get('voice_input_available'),
                    'capabilities': [{'id': c.get('id'), 'state': c.get('state')} for c in startup.get('capabilities', [])],
                    'native': native}))
            except Exception:
                print(json.dumps({'stage': PHASE, 'snapshot_available': False}))
        raise
    finally:
        if driver or process:
            if platform:
                platform.shutdown(driver, process)
            else:
                shutdown_owned(driver, process)
        if services:
            services.__exit__(None, None, None)


def can_connect(port):
    try:
        with socket.create_connection(('127.0.0.1', port), timeout=0.2):
            return True
    except OSError:
        return False


def driver_free_diagnostic(args, run, fixtures, env, services):
    """Equivalent source/app control with no automation session. Never acceptance."""
    case = args.cases[0]
    services.expected_case = case
    env.pop('TAURI_AUTOMATION', None)
    env['VOXGOLEM_BENCH_AUTO_DIAGNOSTIC_CASE'] = case
    process = subprocess.Popen([str(args.binary.resolve())], env=env, start_new_session=True,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        time.sleep(min(args.flow_timeout, fixtures[case]['duration_seconds']+13))
        process.terminate()
        process.wait(timeout=15)
        path = run/'driver-free-diagnostic.json'
        native = json.loads(path.read_text()) if path.exists() and path.stat().st_size <= 128*1024 else None
        private_json(run/'report.json', {'schema': 1, 'mode': 'offline',
            'diagnostic_only': True, 'driver_free': True,
            'snapshot_after_shutdown': True,
            'binary_sha256': hashlib.sha256(args.binary.read_bytes()).hexdigest(),
            'rows': [{'case': case, 'passed': False, 'native': native,
                      'service_metadata': services.snapshot()}],
            'exit_code': process.returncode})
        if path.exists(): path.unlink()
        print(json.dumps({'passed': False, 'diagnostic_only': True,
                          'driver_free': True, 'service_metadata': services.snapshot()}))
        return 1
    finally:
        if process.poll() is None:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--tools', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--config', type=Path, required=True)
    parser.add_argument('--wake-phrase', help='Required for voice, unused by typed-workflow')
    parser.add_argument('--voice', default='en-gb', choices=['en-gb', 'en-us', 'en-us+f3', 'en-gb+f2'])
    parser.add_argument('--cases', nargs='+', choices=list(CASES)+['arithmetic'])
    parser.add_argument('--entry', choices=['voice', 'typed', 'typed-control', 'typed-speech'], default='voice', help='Typed speech is a Neural Windows offline diagnostic')
    parser.add_argument('--desktop-platform', choices=['linux', 'windows'], default='linux')
    parser.add_argument('--windows-manifest', type=Path, help='Windows-only staged PCM/assets/tools hash manifest')
    parser.add_argument('--extended', action='store_true', help='Offline typed context/history/cancellation/relaunch matrix')
    parser.add_argument('--flow-timeout', type=float, default=180)
    parser.add_argument('--poll-interval', type=float, default=0.1, help='Non-default intervals are observer-effect diagnostics, not qualifying evidence')
    parser.add_argument('--wake-diagnostics', action='store_true', help='Bounded score diagnostics for fixture qualification, not final timing runs')
    parser.add_argument('--event-loop-diagnostic', action='store_true', help='Temporary browser heartbeat probe; never qualifying evidence')
    parser.add_argument('--driver-free-diagnostic', action='store_true', help='Offline automation-effect control only; never qualifying evidence')
    parser.add_argument('--hold-native-observer', action='store_true', help='Offline quick voice consumer-stall diagnostic only; always exits 1')
    parser.add_argument('--live', action='store_true')
    parser.add_argument('--shared-suite', action='store_true', help='Qualified live typed shared scenario suite')
    parser.add_argument('--offline-evidence', type=Path, help='Matching successful offline report, required before live reservation')
    global ARTIFACT_ROOT, ASSIGNMENT_LEDGER
    try:
        if ARTIFACT_ROOT is None:
            ARTIFACT_ROOT = artifact_root(ROOT)
        if ASSIGNMENT_LEDGER is None:
            ASSIGNMENT_LEDGER = ARTIFACT_ROOT / 'unified-app-bench-live-budget.json'
    except ValueError as error:
        parser.error(str(error))
    parser.add_argument('--ledger', type=Path, default=ASSIGNMENT_LEDGER)
    parser.add_argument('--inside-offline', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--inside-session', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if any(path.is_symlink() for path in (ARTIFACT_ROOT, *ARTIFACT_ROOT.parents)):
        parser.error('Benchmark artifact storage must not use symlinks')
    typed_speech = args.entry == 'typed-speech'
    if typed_speech and args.desktop_platform != 'windows':
        parser.error('Typed speech requires native Windows offline diagnostics')
    if args.cases is None:
        args.cases = ['arithmetic'] if typed_speech else list(CASES)
    if not typed_speech and 'arithmetic' in args.cases:
        parser.error('Arithmetic fixtures require Neural typed-speech entry')
    args.tools = args.tools.resolve()
    if args.shared_suite and (not args.live or args.entry != 'typed' or args.extended or
            args.driver_free_diagnostic or args.hold_native_observer):
        parser.error('Shared suite requires live typed entry without extended or diagnostic modes')
    if args.hold_native_observer and (args.live or args.entry != 'voice' or args.cases != ['quick'] or
            args.driver_free_diagnostic or args.wake_diagnostics or args.event_loop_diagnostic or
            args.extended or args.poll_interval != 0.1):
        parser.error('Native observer hold requires exactly one offline quick voice case without other probes')
    if args.entry not in ('typed', 'typed-speech') and not args.wake_phrase:
        parser.error('Voice and legacy controls require an explicit synthetic wake phrase')
    if args.extended and (args.entry != 'typed' or args.live):
        parser.error('Extended safety/context tests are offline typed-workflow only')
    if (not args.output.resolve().is_relative_to(ARTIFACT_ROOT.resolve()) or
            args.output.resolve() in (ARTIFACT_ROOT.resolve(), ASSIGNMENT_LEDGER.resolve())):
        parser.error('Benchmark artifacts must use a private run under the outer project artifacts tree')
    if not 10 <= args.flow_timeout <= 180:
        parser.error('Flow timeout must be between 10 and 180 seconds')
    if not 0.05 <= args.poll_interval <= 1:
        parser.error('Observer interval must be 0.05..1 seconds')
    if args.driver_free_diagnostic and (args.live or len(args.cases) != 1 or args.entry != 'voice'):
        parser.error('Driver-free diagnostic requires exactly one offline case')
    if args.entry == 'typed-control' and len(args.cases) != 1:
        parser.error('Typed controls use one fresh app/case, not a substitute voice matrix')
    if args.live and args.inside_offline:
        parser.error('The internal namespace continuation cannot be used for live mode')
    if args.inside_session and not args.live:
        parser.error('Offline continuation requires its network namespace')
    if args.entry == 'typed' and (args.wake_diagnostics or args.event_loop_diagnostic or args.poll_interval != 0.1):
        parser.error('Typed-workflow measurements cannot enable diagnostic probes')
    if args.live and (args.entry not in ('voice', 'typed') or args.wake_diagnostics or args.event_loop_diagnostic or args.poll_interval != 0.1):
        parser.error('Diagnostic probes cannot qualify or run final live measurement')
    if args.live and args.cases != list(CASES):
        parser.error('The final live batch runs each approved case once, in order')
    if args.ledger.resolve() != ASSIGNMENT_LEDGER.resolve():
        parser.error('Live allowance uses the fixed assignment-wide ledger')
    if args.desktop_platform == 'windows':
        import windows_effects
        effects = None
        try:
            windows_effects.validate_options(args)
            if args.inside_offline or args.inside_session or args.shared_suite:
                raise ValueError('Windows cannot use Unix namespace continuations or live suites')
            effects = windows_effects.WindowsEffects(args)
            return execute(args, platform=effects)
        except Exception as error:
            report_persistence_failed = False
            if effects:
                try:
                    effects.failure_report(SimpleNamespace(**globals()), PHASE, error)
                except Exception:
                    report_persistence_failed = True
            print(json.dumps({'passed': False, 'overall_status': 'unverified',
                              'stage': PHASE, 'failure_category': type(error).__name__,
                              'platform_stage': getattr(effects, 'stage', None),
                              'native_error_code': getattr(error, 'native_error_code', None),
                              'report_persistence_failed': report_persistence_failed}))
            return 1
    if sys.platform != 'linux' or args.windows_manifest is not None:
        parser.error('Linux namespace entry requires Linux and no Windows manifest')
    inside = args.inside_offline or args.inside_session
    if not inside:
        args.output = args.output.resolve()
        args.output.mkdir(mode=0o700, parents=True, exist_ok=False)
        private_json(args.output/'.app-bench-run.json', {'schema': 1,
            'owner': 'voxgolem-app-bench', 'created_epoch_seconds': time.time()})
        prune_owned_reports(args.output.parent)
    if not inside:
        env = os.environ.copy()
        env['VOXGOLEM_BENCH_ORIGINAL_NETNS'] = os.readlink('/proc/self/ns/net')
        env['VOXGOLEM_BENCH_ORIGINAL_PIDNS'] = os.readlink('/proc/self/ns/pid')
        env['VOXGOLEM_BENCH_ORIGINAL_DBUS'] = env.get('DBUS_SESSION_BUS_ADDRESS', '')
        env['VOXGOLEM_BENCH_SOURCE_HOME'] = env.get('HOME', '')
        env['VOXGOLEM_BENCH_SOURCE_DATA_HOME'] = env.get('XDG_DATA_HOME', '')
        runtime = args.output/'runtime'
        runtime.mkdir(mode=0o700)
        if env.get('WAYLAND_DISPLAY') and not Path(env['WAYLAND_DISPLAY']).is_absolute() and env.get('XDG_RUNTIME_DIR'):
            env['WAYLAND_DISPLAY'] = str(Path(env['XDG_RUNTIME_DIR'])/env['WAYLAND_DISPLAY'])
        env['XDG_RUNTIME_DIR'] = str(runtime)
        for name, folder in [('HOME','home'),('XDG_CONFIG_HOME','config'),('XDG_DATA_HOME','data'),
                             ('XDG_STATE_HOME','state'),('XDG_CACHE_HOME','cache')]:
            path = args.output/folder
            path.mkdir(mode=0o700)
            env[name] = str(path)
        dbus = shutil.which('dbus-run-session')
        if dbus is None: raise RuntimeError('Private D-Bus session tooling is required')
        command = ['bwrap', '--unshare-pid', '--die-with-parent']
        if not args.live: command += ['--unshare-net']
        command += ['--bind' if args.live else '--ro-bind', '/', '/',
                   '--dev', '/dev', '--tmpfs', '/dev/shm', '--proc', '/proc', '--tmpfs', '/tmp',
                   '--dir', '/tmp/opencode', '--bind', str(args.tools), str(args.tools),
                   '--bind', str(args.output), str(args.output)]
        # Keep normal graphics resources without exposing any audio input device.
        for graphics in ['/dev/dri', '/dev/dxg']:
            if Path(graphics).exists(): command += ['--dev-bind', graphics, graphics]
        if Path('/tmp/.X11-unix').is_dir():
            command += ['--ro-bind', '/tmp/.X11-unix', '/tmp/.X11-unix']
        command += [dbus, '--', sys.executable, str(Path(__file__).resolve()), *sys.argv[1:],
                    '--inside-session' if args.live else '--inside-offline']
        return subprocess.call(command, env=env, stderr=subprocess.DEVNULL)
    validate_private_session(os.environ.get('VOXGOLEM_BENCH_ORIGINAL_PIDNS'),
        os.readlink('/proc/self/ns/pid'), os.environ.get('VOXGOLEM_BENCH_ORIGINAL_DBUS'),
        os.environ.get('DBUS_SESSION_BUS_ADDRESS'))
    subprocess.run(['gdbus', 'call', '--session', '--dest', 'org.freedesktop.DBus',
        '--object-path', '/org/freedesktop/DBus', '--method', 'org.freedesktop.DBus.Peer.Ping'],
        check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=3)
    if not args.live:
        interfaces = [line.split(':')[0].strip() for line in Path('/proc/net/dev').read_text().splitlines() if ':' in line]
        validate_offline_namespace(os.environ.get('VOXGOLEM_BENCH_ORIGINAL_NETNS'),
                                   os.readlink('/proc/self/ns/net'), interfaces)
    for key in list(os.environ):
        if key.lower().endswith('_proxy'):
            del os.environ[key]
    try:
        return execute(args)
    except Exception as error:
        # Deliberately do not print WebDriver/provider exception payloads.
        print(json.dumps({'passed': False, 'stage': PHASE, 'failure_category': type(error).__name__}))
        return 1


if __name__ == '__main__':
    sys.exit(main())
