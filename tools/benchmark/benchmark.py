#!/usr/bin/env python3
"""Shared typed suite. No device activity without both explicit authorization flags."""
import argparse
import asyncio
from contextlib import contextmanager
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import shlex
import signal
import socket
import subprocess
import sys
import time
from urllib.parse import urlsplit, urlunsplit
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
PACKAGE = 'dev.gitblame.voxgolem.android'
MAX_REPORT_BYTES = 128 * 1024
SCENARIO_IDS = ('arithmetic', 'coding', 'lookup', 'page_read', 'mixed', 'history')
MIN_ANDROID_API = 35  # Android 15 minimum; actual API is always recorded, not assumed 36.
# Rechecked in the same JS continuation as every semantic mutation, not only in
# Python before an intervening native round trip. Never returns transcript text.
IDLE_FENCE = """const owner = window.__voxBenchmarkOwner;
const snapshot = await window.__TAURI_INTERNALS__.invoke('get_app_snapshot');
const tail = snapshot.transcript.at(-1);
if (!owner || snapshot.session_id !== owner.session || snapshot.active_request !== null ||
    snapshot.runtime_phase !== 'sleeping' || (owner.request === null ?
      JSON.stringify(snapshot.transcript.map(t => [t.request_id,t.execution_id])) !== owner.baseline :
      !((tail?.request_id === owner.request && JSON.stringify(tail?.execution_id) === owner.execution) ||
        JSON.stringify(snapshot.transcript.map(t => [t.request_id,t.execution_id])) === owner.before))) return false;
"""

NATIVE_PARSE_JS = r"""function parseNativeRate(text, at) {
  if (typeof text !== 'string' || text.length > 160) return null;
  const m = /^Rate limited · (Exa|Parallel) · provider wait (unknown|(?:0|[1-9][0-9]{0,7}) s) · local wait ((?:0|[1-9][0-9]{0,7})) s$/.exec(text);
  if (!m || m[0] !== text) return null;
  const provider = m[2] === 'unknown' ? null : Number(m[2].slice(0,-2));
  const local = Number(m[3]);
  if ((provider !== null && provider > 31536000) || local > 31536000) return null;
  return {provider:m[1], providerWait:provider, localWait:local, at};
} """

NATIVE_INSTALL_JS = """(() => {
  const form = document.getElementById('promptComposer')?.closest('form');
  const owner = window.__voxBenchmarkOwner;
  if (!form || !owner || window.__voxBenchmarkRateStatus || typeof MutationObserver !== 'function') return false;
  """ + NATIVE_PARSE_JS + """
  const selector = '.composer__actions--secondary [role="status"], .composer__status[role="status"], .composer__status [role="status"]';
  const state = {owner, started:false, submitted:false, admitted:null, records:new Map()};
  const existing = new WeakSet(Array.from(form.querySelectorAll(selector)).slice(0,4));
  const changed = new WeakSet();
  state.observer = new MutationObserver(mutations => {
    if (!state.started || window.__voxBenchmarkOwner !== owner) return;
    const candidates = new Map();
    const collect = (node, added=false) => {
      if (node.nodeType !== 1) return;
      const elements = [node, ...Array.from(node.querySelectorAll(selector)).slice(0,4)];
      for (const e of elements) if (e.matches(selector)) {
        if (added && !existing.has(e)) changed.add(e);
        if (changed.has(e)) candidates.set(e, added || candidates.get(e) === true);
      }
    };
    for (const m of mutations.slice(-16)) {
      if (m.target !== form && !form.contains(m.target)) continue;
      const target = m.target.nodeType === 1 ? m.target : m.target.parentElement;
      const status = target?.closest(selector);
      if (status && form.contains(status)) { changed.add(status); candidates.set(status,true); }
      for (const node of Array.from(m.addedNodes || []).slice(0,4)) collect(node, true);
      for (const node of Array.from(m.removedNodes || []).slice(0,4)) collect(node);
    }
    for (const [e,fresh] of Array.from(candidates).slice(0,8)) {
      const record = parseNativeRate(e.textContent, performance.now());
      if (record && (fresh || !state.records.has(record.provider))) state.records.set(record.provider, record);
    }
  });
  state.observer.observe(form, {subtree:true, childList:true, characterData:true});
  window.__voxBenchmarkRateStatus = state;
  return true;
})()"""

NATIVE_ARM_JS = """(() => {
  const s = window.__voxBenchmarkRateStatus;
  if (!s || s.owner !== window.__voxBenchmarkOwner) return false;
  s.observer.takeRecords(); s.started = true; return true;
})()"""

NATIVE_READ_JS = """(async () => {
  const s = window.__voxBenchmarkRateStatus, o = window.__voxBenchmarkOwner;
  if (!s || s.owner !== o || !s.submitted || !s.admitted || o.request !== s.admitted) return [];
  const snapshot = await window.__TAURI_INTERNALS__.invoke('get_app_snapshot');
  if (snapshot.session_id !== o.session) return [];
  if (snapshot.active_request) {
    if (snapshot.active_request.request_id !== o.request || JSON.stringify(snapshot.active_request.execution_id) !== o.execution) return [];
  } else {
    const last = snapshot.transcript.at(-1);
    if (!(last?.request_id === o.request && JSON.stringify(last?.execution_id) === o.execution) &&
        JSON.stringify(snapshot.transcript.map(t => [t.request_id,t.execution_id])) !== o.before) return [];
  }
  const now = performance.now();
  return Array.from(s.records.values()).slice(0,2).map(r => {
    const elapsed = Math.max(0, (now-r.at)/1000);
    const remaining = value => value === null ? null : Math.ceil(Math.max(0,value-elapsed));
    return {provider:r.provider, provider_retry_after_seconds:remaining(r.providerWait),
      local_cooldown_seconds:remaining(r.localWait), provenance:'native_provider_status',
      provider_reset:r.providerWait === null ? 'unknown' : 'minimum_retry_delay'};
  });
})()"""

NATIVE_DETACH_JS = """(() => {
  const s = window.__voxBenchmarkRateStatus;
  if (s && s.owner !== window.__voxBenchmarkOwner) return false;
  if (s) { s.observer.disconnect(); delete window.__voxBenchmarkRateStatus; }
  return true;
})()"""


class Blocked(Exception):
    """Only categorical, non-payload explanations may be passed here."""


def load_scenarios():
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise Blocked('duplicate_scenario_key')
            result[key] = value
        return result
    try:
        with Path(__file__).with_name('scenarios.json').open('rb') as stream:
            raw = stream.read(32 * 1024 + 1)
    except OSError:
        raise Blocked('scenario_source_unavailable') from None
    if len(raw) > 32 * 1024:
        raise Blocked('scenario_size_limit')
    try:
        cases = json.loads(raw, object_pairs_hook=unique)
    except (ValueError, UnicodeError):
        raise Blocked('scenario_json_invalid') from None
    allowed = {'id', 'prompt', 'web_enabled', 'allows_no_sources', 'expected',
               'official_hosts', 'requires_page_read', 'seed', 'history_condition', 'voice'}
    if type(cases) is not list or len(cases) != 6:
        raise Blocked('scenario_set_invalid')
    for index, case in enumerate(cases):
        if type(case) is not dict or set(case) - allowed or case.get('id') != SCENARIO_IDS[index]:
            raise Blocked('scenario_contract_invalid')
        from voice_contract import validate_voice
        try:
            validate_voice(case)
        except (ValueError, TypeError, UnicodeError):
            raise Blocked('scenario_voice_metadata_invalid') from None
        for key, maximum in (('prompt', 2048), ('seed', 1024), ('history_condition', 128), ('expected', 128)):
            if key in case:
                if type(case[key]) is not str:
                    raise Blocked('scenario_text_invalid')
                try:
                    size = len(case[key].encode())
                except UnicodeError:
                    raise Blocked('scenario_text_invalid') from None
                if not 1 <= size <= maximum or any(ord(c) < 32 for c in case[key]):
                    raise Blocked('scenario_text_invalid')
        if 'prompt' not in case or any(type(case.get(k)) is not bool for k in ('web_enabled', 'allows_no_sources')):
            raise Blocked('scenario_types_invalid')
        if case['web_enabled'] != (index in (1, 2, 3, 4)) or case['allows_no_sources'] != (index in (0, 1, 5)):
            raise Blocked('scenario_policy_invalid')
        if case.get('requires_page_read', False) is not (index in (3, 4)):
            raise Blocked('scenario_read_policy_invalid')
        expected_hosts = ['federalreserve.gov'] if index in (2, 3) else ['tokio.rs', 'async.rs'] if index == 4 else None
        if case.get('official_hosts') != expected_hosts:
            raise Blocked('scenario_official_policy_invalid')
        expected_answer = '210' if index == 0 else 'SYNTHETIC-ORBIT-42' if index == 5 else None
        if case.get('expected') != expected_answer:
            raise Blocked('scenario_answer_policy_invalid')
        if (index == 5) != ('seed' in case and 'history_condition' in case) or (index != 5 and ('seed' in case or 'history_condition' in case)):
            raise Blocked('scenario_history_policy_invalid')
        if index == 5 and (case['history_condition'] != 'one-ui-seed-plus-followup-not-24-message-host-history' or
                           case['expected'] not in case['seed']):
            raise Blocked('scenario_history_integrity_invalid')
    return cases


def official_url(url, domain):
    try:
        parsed = urlsplit(url)
        if parsed.scheme != 'https' or parsed.username or parsed.password or parsed.port not in (None, 443):
            return False
        host = (parsed.hostname or '').lower()
        if host == domain or host.endswith('.' + domain):
            return True
        # Deliberate official project alternatives: never arbitrary docs.rs/GitHub.
        alternatives = {'async.rs':[('docs.rs', '/async-std'), ('github.com', '/async-rs/async-std')],
                        'tokio.rs':[('docs.rs', '/tokio'), ('github.com', '/tokio-rs/tokio')]}
        return '%' not in parsed.path and '\\' not in parsed.path and \
            not any(p in ('.', '..') for p in parsed.path.split('/')) and any(host == h and
            (parsed.path == p or parsed.path.startswith(p + '/')) for h, p in
            alternatives.get(domain, []))
    except ValueError:
        return False


def select_scenarios(args, report=None):
    full = load_scenarios()
    requested = getattr(args, 'cases', None)
    requested = list(SCENARIO_IDS) if requested is None else requested
    if type(requested) is not list or not requested or len(set(requested)) != len(requested) or any(c not in SCENARIO_IDS for c in requested):
        raise Blocked('scenario_selection_invalid')
    selected = [c for c in full if c['id'] in requested]
    ids = [c['id'] for c in selected]
    if getattr(args, 'platform', 'android') == 'desktop' and ids != list(SCENARIO_IDS):
        raise Blocked('desktop_partial_suite_unsupported')
    if report is not None:
        report.update(requested_cases=list(requested), selected_cases=ids,
                      omitted_cases=[c for c in SCENARIO_IDS if c not in ids],
                      coverage='full' if len(selected) == len(full) else 'partial',
                      suite_case_count=len(full), selected_case_count=len(selected),
                      planned_admissions=planned_admissions(args, selected))
    return selected


def planned_admissions(args, selected):
    return sum(1 + getattr(args, 'history_seed_turns', 1) if 'seed' in c else 1 for c in selected) * args.repetitions


def seed_prompts(args, case):
    count = getattr(args, 'history_seed_turns', 1)
    return [case['seed'] if count == 1 else
            case['seed'] + f' Synthetic seed turn {index + 1} of {count}. Keep remembering the marker. Reply only OK.'
            for index in range(count)]


def source_failure_only(case, checks, counts):
    return case['id'] in ('lookup', 'page_read', 'mixed') and counts == {'ui':0, 'canonical':0} and \
        {k for k, v in checks.items() if v == 'fail'} == {'sources_present', 'official_source'} and all(
            checks.get(k) == 'pass' for k in ('answer', 'sources_equal', 'canonical_answer_equal', 'canonical_execution_and_single_ui_row'))


def model_reported_rate_limit(case, result, checks):
    if not source_failure_only(case, checks, {'ui':len(result['sources']), 'canonical':len(result['canonical_sources'])}):
        return False
    text = result['answer'][:8192]
    if re.search(r'\b(?:not|never|no longer|isn.t|wasn.t|aren.t|weren.t)\s+(?:(?:currently|being|been|actually)\s+){0,2}rate[ -]limit(?:ed|ing)?\b|'
                 r'\bno\s+(?:rate[ -]limit(?:ing)?|(?:http\s*)?429)\b|'
                 r'\b(?:http\s*)?429\s+(?:was|is)\s+not\b|'
                 r'\b(?:did not|have not|has not|didn.t|haven.t|hasn.t)\s+(?:hit|encounter|receive|reach|experience)\s+(?:(?:a|the|any)\s+)?rate[ -]limit', text, re.I):
        return False
    return bool(re.search(r'\brate[ -]limited\b|\b(?:is|was|currently|encountered|due to)\s+(?:a\s+)?rate[ -]limiting\b|'
                          r'\brate[ -]limit\s+(?:error|exceeded|reached|hit)\b|'
                          r'\b(?:hit|reached|exceeded|encountered)\s+(?:(?:a|the)\s+)?rate[ -]limit\b|'
                          r'\b(?:http(?:\s+status)?|status(?:\s+code)?|error)\s*[:=]?\s*429\b|'
                          r'\b429\s*[:\-]?\s*too many requests\b', text, re.I))


def validate(case, answer, ui_sources, canonical_sources):
    checks = {'answer': 'pass' if answer.strip() else 'fail',
              'sources_equal': 'pass' if ui_sources == canonical_sources else 'fail'}
    if 'expected' in case:
        checks['answer'] = 'pass' if answer.strip() == case['expected'] else 'fail'
    elif case['id'] == 'coding':
        checks['answer'] = 'pass' if (re.search(r'\bfn\s+\w+', answer) and
            re.search(r'&\s*\[\s*i32\s*\]', answer) and
            re.search(r'->\s*i32', answer) and
            ('.sum' in answer or 'fold' in answer or re.search(r'\bfor\b', answer))) else 'fail'
    if not case['allows_no_sources']:
        checks['sources_present'] = 'pass' if canonical_sources else 'fail'
        checks['semantic_accuracy'] = 'unverified'
    if 'official_hosts' in case:
        checks['official_source'] = 'pass' if all(any(official_url(url, domain)
            for url in canonical_sources) for domain in case['official_hosts']) else 'fail'
    if case.get('requires_page_read'):
        # Neither source attribution nor generated prose is page-read execution evidence.
        checks['page_read'] = 'unverified'
    return checks


def select_a17(listing):
    candidates = []
    for line in listing.splitlines():
        fields = line.split()
        if len(fields) >= 3 and fields[1] == 'device' and 'model:SM_A176B' in fields:
            candidates.append(fields[0])
    if len(candidates) != 1:
        raise Blocked('a17_unique_advertised_transport_required')
    return candidates[0]


class ADB:
    """Bounded argv-only transport; no ADB restart, install, scan or pairing."""
    def __init__(self, executable, ssh=None, remote_adb=None):
        self.executable, self.ssh, self.remote_adb = executable, ssh, remote_adb
        self.device = None

    def argv(self, *args):
        command = [self.remote_adb if self.ssh else self.executable]
        if self.device:
            command += ['-s', self.device]
        command += list(args)
        return ['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '--', self.ssh, shlex.join(command)] if self.ssh else command

    def run(self, *args):
        result = subprocess.run(self.argv(*args), capture_output=True, timeout=20, stdin=subprocess.DEVNULL)
        if result.returncode:
            raise Blocked('adb_operation_failed')
        if len(result.stdout) > 4 * 1024 * 1024:
            raise Blocked('adb_output_limit')
        return result.stdout.decode('utf-8', errors='replace').strip()


@contextmanager
def owned_forward(adb, port, pid, cleanup):
    local, remote = f'tcp:{port}', f'localabstract:webview_devtools_remote_{pid}'
    adb.run('forward', '--no-rebind', local, remote)
    cleanup['forward'] = 'acquired'
    cleanup['forward_acquired'] = True
    try:
        yield
    finally:
        try:
            mappings = [line.split() for line in adb.run('forward', '--list').splitlines()]
            expected = [adb.device, local, remote]
            if expected not in mappings or any(m[1] == local and m != expected for m in mappings if len(m) == 3):
                raise Blocked('owned_forward_changed_not_removed')
            adb.run('forward', '--remove', local)
        except BaseException:
            cleanup['forward'] = 'fail'
            raise
        else:
            cleanup['forward'] = 'pass'


@contextmanager
def owned_tunnel(host, port, cleanup):
    if not host:
        yield
        return
    # Refuse an occupied local port; SSH also checks binding, closing the race.
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', port))
    process = subprocess.Popen(['ssh', '-N', '-o', 'ExitOnForwardFailure=yes',
                                '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '-L',
                                f'127.0.0.1:{port}:127.0.0.1:{port}', '-o', 'ControlMaster=no',
                                '-o', 'ControlPath=none', '--', host],
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    cleanup['tunnel'] = 'fail'
    cleanup['tunnel_acquired'] = True
    try:
        deadline = time.monotonic() + 10
        while True:
            if process.poll() is not None:
                raise Blocked('owned_ssh_tunnel_failed')
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=.1):
                    cleanup['tunnel_ready'] = True
                    break
            except OSError:
                if time.monotonic() >= deadline:
                    raise Blocked('owned_ssh_tunnel_deadline') from None
                time.sleep(.05)
        yield
    finally:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        cleanup['tunnel'] = 'pass'


class BenchmarkParser(argparse.ArgumentParser):
    def parse_args(self, args=None, namespace=None):
        argv = sys.argv[1:] if args is None else args
        parsed = super().parse_args(argv, namespace)
        parsed.explicit_options = {option for a in argv if a.startswith('--')
                                   for option in self._option_string_actions
                                   if option.startswith(a.split('=')[0])}
        return parsed


def parser():
    p = BenchmarkParser(description=__doc__)
    p.add_argument('--platform', required=True, choices=['android', 'desktop', 'wear'])
    p.add_argument('--workload', choices=['typed', 'synthetic-voice', 'typed-speech'], default='typed')
    p.add_argument('--list', action='store_true', help='List synthetic scenarios without transport access')
    p.add_argument('--cases', nargs='+', choices=SCENARIO_IDS, help='Android/Wear subset, executed in canonical suite order')
    p.add_argument('--live', action='store_true')
    p.add_argument('--wear-ui-probe', action='store_true', help='Wear only: prepare the isolated draft and reach Send without generating a reply')
    p.add_argument('--allow-device', action='store_true')
    p.add_argument('--device', choices=['a17', 'watch7'])
    p.add_argument('--adb', help='Explicit local ADB executable')
    p.add_argument('--ssh', help='SSH host alias (not recorded)')
    p.add_argument('--remote-adb', help='Explicit remote ADB executable')
    p.add_argument('--port', type=int, default=9223)
    p.add_argument('--model', default='custom-luna-none', help='Model selected semantically for the run, then restored')
    p.add_argument('--repetitions', type=int, choices=range(1, 4), default=1)
    p.add_argument('--history-seed-turns', type=int, choices=range(1, 9), default=1,
                   help='Android normal UI seed admissions before the history follow-up')
    p.add_argument('--collect-stages', action='store_true', help='Android only: bounded content-free local timing journal summary')
    p.add_argument('--max-live-calls', type=int, default=7)
    p.add_argument('--turn-timeout', type=float, default=180)
    p.add_argument('--output', type=Path, default=ROOT.parent / 'artifacts/benchmarks/typed')
    p.add_argument('--build-variant', choices=['debug', 'release', 'profileable', 'performance', 'unspecified'], default='unspecified')
    p.add_argument('--apk', type=Path, help='Optional local APK; hash only, never installed')
    p.add_argument('--helper-apk', type=Path, help='Synthetic voice only: matching installed helper APK; never installed')
    for name in ('binary', 'tools', 'config', 'offline-evidence'):
        p.add_argument('--' + name, type=Path, help='Desktop qualified lifecycle prerequisite')
    p.add_argument('--desktop-platform', choices=['linux', 'windows'])
    p.add_argument('--windows-manifest', type=Path)
    p.add_argument('--wake-phrase')
    return p


def reject_desktop_voice_options(args):
    names = ('desktop_platform', 'windows_manifest', 'wake_phrase')
    if (any(getattr(args, name, None) is not None for name in names) or
            getattr(args, 'explicit_options', set()) & {'--' + name.replace('_', '-') for name in names}):
        raise Blocked('desktop_voice_options_require_desktop_synthetic_voice')


def authorize(args):
    reject_desktop_voice_options(args)
    if getattr(args, 'helper_apk', None) is not None:
        raise Blocked('helper_apk_requires_synthetic_voice')
    if getattr(args, 'build_variant', None) == 'performance' and args.platform != 'wear':
        raise Blocked('performance_variant_requires_wear_platform')
    if getattr(args, 'wear_ui_probe', False) and args.platform != 'wear':
        raise Blocked('wear_ui_probe_requires_wear_platform')
    selected = select_scenarios(args)
    needed = planned_admissions(args, selected)
    if getattr(args, 'history_seed_turns', 1) not in range(1, 9):
        raise Blocked('history_seed_turns_invalid')
    if args.platform == 'desktop' and getattr(args, 'collect_stages', False):
        raise Blocked('desktop_stage_collection_unsupported')
    if args.platform == 'desktop' and getattr(args, 'history_seed_turns', 1) != 1:
        raise Blocked('desktop_history_seed_count_unsupported')
    if args.platform == 'wear':
        if not args.live or not args.allow_device or args.device != 'watch7':
            raise Blocked('explicit_live_and_device_authorization_required')
        if args.collect_stages:
            raise Blocked('wear_stage_collection_unsupported')
        if args.model != 'custom-luna-none' or args.repetitions != 1 or args.history_seed_turns != 1:
            raise Blocked('wear_fixed_model_repetition_and_history_required')
        if args.build_variant != 'performance':
            raise Blocked('wear_performance_artifact_required')
        if not math.isfinite(args.turn_timeout) or not 10 <= args.turn_timeout <= 180:
            raise Blocked('wear_turn_deadline_invalid')
        if not needed <= args.max_live_calls <= 7:
            raise Blocked('wear_live_call_budget_invalid')
        if args.apk is None or not args.apk.is_file() or args.apk.is_symlink() or not 0 < args.apk.stat().st_size <= 256 * 1024 * 1024:
            raise Blocked('wear_bounded_local_apk_required')
    if not args.live or (args.platform == 'android' and (not args.allow_device or args.device != 'a17')):
        raise Blocked('explicit_live_and_device_authorization_required')
    if args.platform == 'desktop':
        if any(getattr(args, k) is None for k in ('binary', 'tools', 'config', 'offline_evidence')) or \
                args.model != 'custom-luna-none' or args.repetitions != 1 or \
                not math.isfinite(args.turn_timeout) or not 10 <= args.turn_timeout <= 180 or \
                args.allow_device or args.device or args.ssh or args.adb or args.remote_adb or args.apk:
            raise Blocked('desktop_qualified_shared_suite_prerequisites_required')
        if not 7 <= args.max_live_calls <= 12:
            raise Blocked('desktop_shared_suite_admission_budget_invalid')
        return
    if not args.adb or not args.model or (args.ssh and not args.remote_adb):
        raise Blocked('explicit_transport_and_model_required')
    if not 1 <= args.port <= 65535 or not math.isfinite(args.turn_timeout) or not 1 <= args.turn_timeout <= 600:
        raise Blocked('invalid_port_or_turn_deadline')
    if not needed <= args.max_live_calls <= 21:
        raise Blocked('live_call_budget_insufficient_or_unbounded')
    if not re.fullmatch(r'[A-Za-z0-9_.:-]{1,128}', args.model):
        raise Blocked('invalid_public_model_identifier')
    if args.ssh and (args.ssh.startswith('-') or not re.fullmatch(r'[A-Za-z0-9_.@-]{1,128}', args.ssh)):
        raise Blocked('ssh_host_alias_required')


def preflight_output(directory):
    directory = directory.expanduser()
    if any(p.is_symlink() for p in [directory, *directory.parents]):
        raise Blocked('report_symlink_refused')
    directory = directory.resolve()
    if directory.is_relative_to(ROOT):
        raise Blocked('report_must_be_outside_repository')
    directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if directory.stat().st_mode & 0o077:
        raise Blocked('report_directory_must_be_private')
    # Establish writeability before transport; remove only this private probe.
    probe = directory / f'.typed-benchmark-probe-{os.getpid()}-{time.time_ns()}'
    fd = os.open(probe, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        os.write(fd, b'probe')
    finally:
        os.close(fd)
        probe.unlink()
    return directory


def write_report(directory, report):
    directory = preflight_output(directory)
    reports = []
    now = time.time()
    voice_suites = ('synthetic-voice-pipeline', 'synthetic-voice-arithmetic')
    prefix = 'voice-benchmark' if report.get('suite') in voice_suites else 'typed-benchmark'
    for path in directory.glob(prefix + '-*.json'):
        if path.is_symlink() or not path.is_file():
            continue
        if path.stat().st_size > MAX_REPORT_BYTES:
            continue
        try:
            prior = json.loads(path.read_bytes())
        except (OSError, ValueError):
            continue
        if not isinstance(prior, dict) or prior.get('suite') not in (voice_suites if prefix == 'voice-benchmark' else ('typed-model-matrix', 'wear-native-ui-probe')) or prior.get('schema') not in ((1, 2) if prefix == 'voice-benchmark' else (1,)):
            continue
        if now - path.stat().st_mtime > 90 * 86400:
            path.unlink()
        else:
            reports.append(path)
    reports.sort(key=lambda p: p.stat().st_mtime)
    total = sum(p.stat().st_size for p in reports)
    while reports and (len(reports) >= 100 or total > 8 * 1024 * 1024 - MAX_REPORT_BYTES):
        old = reports.pop(0)
        total -= old.stat().st_size
        old.unlink()
    payload = json.dumps(report, indent=2).encode()
    if len(payload) > MAX_REPORT_BYTES:
        raise Blocked('report_size_limit')
    path = directory / f'{prefix}-{time.time_ns()}.json'
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, 'wb') as stream:
            stream.write(payload)
    except BaseException:
        path.unlink(missing_ok=True)
        raise
    return path


def smoke_module():
    spec = importlib.util.spec_from_file_location('android_smoke_shared', ROOT / 'scripts/android-e2e-smoke.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def checked_native_diagnostics(records):
    keys = {'provider', 'provider_retry_after_seconds', 'local_cooldown_seconds', 'provenance', 'provider_reset'}
    if type(records) is not list or len(records) > 2:
        raise Blocked('native_status_observation_invalid')
    providers = set()
    for r in records:
        if type(r) is not dict or set(r) != keys or r['provider'] not in ('Exa', 'Parallel') or \
                r['provider'] in providers or r['provenance'] != 'native_provider_status':
            raise Blocked('native_status_observation_invalid')
        providers.add(r['provider'])
        for key in ('provider_retry_after_seconds', 'local_cooldown_seconds'):
            value = r[key]
            if key == 'provider_retry_after_seconds' and value is None:
                continue
            if type(value) is not int or not 0 <= value <= 31536000:
                raise Blocked('native_status_observation_invalid')
        if r['provider_reset'] != ('unknown' if r['provider_retry_after_seconds'] is None else 'minimum_retry_delay'):
            raise Blocked('native_status_observation_invalid')
    return [dict(r) for r in records]


async def exercise_with_native_status(ui, exercise, model, prompt, *, row, cleanup, web_enabled, **kwargs):
    if not web_enabled:
        return await exercise(ui, model, prompt, **kwargs)
    try:
        installed = await ui.evaluate(NATIVE_INSTALL_JS) is True
    except BaseException:
        cleanup['native_status_observer'] = 'fail'
        try:
            if await ui.evaluate(NATIVE_DETACH_JS) is True:
                cleanup['native_status_observer'] = 'pass'
        except BaseException:
            pass
        raise
    row['native_rate_limit_observation'] = 'unverified'
    if not installed:
        return await exercise(ui, model, prompt, **kwargs)
    read_terminal = False
    async def read():
        try:
            records = checked_native_diagnostics(await ui.evaluate(NATIVE_READ_JS))
            row['native_rate_limits'] = records
            row['native_rate_limit_observation'] = 'observed' if records else 'unverified'
        except Exception:
            row['native_rate_limit_observation'] = 'unverified'
    original = dict(kwargs)
    async def ready():
        await original['confirm_ready']()
        if await ui.evaluate(NATIVE_ARM_JS) is not True:
            raise Blocked('native_status_observer_arm_failed')
    async def submitted():
        await original['submission_observer']()
        await ui.evaluate("(() => { const s=window.__voxBenchmarkRateStatus; if(s) s.submitted=true; })()")
    async def admitted(request):
        await original['admission_observer'](request)
        await ui.evaluate(f"(() => {{ const s=window.__voxBenchmarkRateStatus; if(s) s.admitted={json.dumps(request)}; }})()")
    async def terminal(request, metrics):
        nonlocal read_terminal
        await read()
        read_terminal = True
        await original['terminal_observer'](request, metrics)
    kwargs.update(confirm_ready=ready, submission_observer=submitted,
                  admission_observer=admitted, terminal_observer=terminal)
    try:
        return await exercise(ui, model, prompt, **kwargs)
    finally:
        try:
            if not read_terminal:
                await read()  # Failed/refused/disconnected turns stay failed; never fabricate a blocker.
        finally:
            cleanup['native_status_observer'] = 'fail'
            try:
                if await ui.evaluate(NATIVE_DETACH_JS) is not True:
                    raise Blocked('native_status_observer_detach_failed')
            except BaseException:
                raise Blocked('native_status_observer_detach_failed') from None
            cleanup['native_status_observer'] = 'pass'


async def snapshot_preferences(ui):
    # Filter within the app context: no previous user text leaves the WebView.
    value = await ui.evaluate("""(async () => {
      const s = await window.__TAURI_INTERNALS__.invoke('get_app_snapshot');
      return {preferences:s.preferences, idle:s.active_request === null && s.runtime_phase === 'sleeping'};
    })()""")
    if not isinstance(value, dict) or value.get('idle') is not True or not isinstance(value.get('preferences'), dict):
        raise Blocked('canonical_idle_preferences_unavailable')
    return value['preferences']


async def click(ui, label):
    result = await ui.evaluate(f"""(async () => {{
      {IDLE_FENCE}
      const candidates = Array.from(document.querySelectorAll('button[aria-label]'))
        .filter(e => e.getAttribute('aria-label') === {json.dumps(label)} && e.getClientRects().length);
      if (candidates.length !== 1 || candidates[0].disabled) return false;
      candidates[0].click(); return true;
    }})()""")
    if result is not True:
        raise Blocked('semantic_control_unavailable')


async def wait_condition(predicate, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if await predicate():
            return
        await asyncio.sleep(.1)
    raise Blocked('ui_transition_deadline')


# Shared desktop/phone DOM navigation; state holds exact nodes, never user content.
SETTINGS_NAVIGATION_JS = """(async () => {
  const {action, token, page, label, enabled} = ARGS;
  // Internal retirement only: never click or change a draft outside the fences.
  if (action === 'finalize') {
    const n = window.__voxBenchmarkNavigation;
    if (!n) return true;
    if (n.token !== token) return false;
    n.owner.navigationFailed = true;
    delete window.__voxBenchmarkNavigation;
    return false; // finish did not establish UI cleanup before retirement.
  }
  FENCE
  if (window.__voxBenchmarkOwner !== owner) return false;
  if (document.getElementById('promptComposer')?.value !== '') return false;
  const visible = e => !!e && !!e.getClientRects().length &&
    !e.closest('[hidden], [inert]') && !Array.from(document.querySelectorAll('details:not([open])'))
      .some(d => d.contains(e) && !Array.from(d.children).find(c => c.tagName === 'SUMMARY')?.contains(e));
  const control = (root, name, selector = 'button[aria-label], summary[role="button"][aria-label]') => {
    const matches = Array.from(root.querySelectorAll(selector)).filter(e =>
      e.getAttribute('aria-label') === name && visible(e) && !e.disabled);
    return matches.length === 1 ? matches[0] : null;
  };
  const dialogs = () => Array.from(document.querySelectorAll('[role="dialog"]'));
  if (action === 'begin') {
    if (window.__voxBenchmarkNavigation || dialogs().length) return false;
    const trigger = control(document, 'More actions');
    const menu = trigger?.closest('details');
    if (!menu || menu.open) return false;
    window.__voxBenchmarkNavigation = {token, owner, menu, trigger, dialog:null, page:'overview'};
    trigger.click(); return true;
  }
  const n = window.__voxBenchmarkNavigation;
  if (!n || n.token !== token || n.owner !== owner || !n.menu.isConnected) return false;
  if (action === 'open') {
    if (dialogs().length) return false;
    const invoker = control(n.menu, 'Settings');
    if (!invoker || invoker.getAttribute('aria-expanded') !== 'false') return false;
    n.invoker = invoker;
    n.dialogId = invoker.getAttribute('aria-controls');
    invoker.click(); return true;
  }
  const current = dialogs();
  // Capture even during cleanup after a failed mount observation.
  if (!n.dialog && current.length === 1 && current[0].id === n.dialogId &&
      n.invoker?.isConnected && n.invoker.getAttribute('aria-expanded') === 'true') n.dialog = current[0];
  if (current.some(d => d !== n.dialog) || (n.dialog && current.length &&
      ![n.page, 'overview'].includes(n.dialog.getAttribute('data-settings-view')))) return false;
  if (action === 'closed') return current.length === 0;
  if (action === 'finish') {
    if (current.length) return false;
    if (n.menu.open) n.trigger.click();
    if (n.menu.open) return false;
    delete window.__voxBenchmarkNavigation;
    return true;
  }
  if (action === 'back' || action === 'close') {
    if (!current.length) return true;
    if (action === 'back' && n.dialog.getAttribute('data-settings-view') === 'overview') {
      n.page = 'overview'; return true;
    }
    const e = control(n.dialog, action === 'back' ? 'Back to settings' : 'Close settings');
    if (!e) return false;
    if (action === 'back') n.page = 'overview';
    e.click(); return true;
  }
  if (action === 'overview') return !current.length || n.dialog?.getAttribute('data-settings-view') === 'overview';
  if (!n.dialog || !current.length || !visible(n.dialog)) return false;
  if (action === 'mounted') return n.page === 'overview';
  if (action === 'page') {
    const e = control(n.dialog, page === 'web' ? 'Web research' : 'Voice and audio');
    if (!e || n.page !== 'overview') return false;
    n.page = page; e.click(); return true;
  }
  const e = control(n.dialog, label, 'input[role="switch"][aria-label]');
  if (action === 'ready') return !!e && n.page === page;
  if (action === 'toggle' && e && n.page === page) {
    if (e.checked !== enabled) e.click();
    return true;
  }
  return false;
})()""".replace('FENCE', IDLE_FENCE)


async def settings_navigation(ui, action, **args):
    return await ui.evaluate(SETTINGS_NAVIGATION_JS.replace('ARGS', json.dumps(dict(action=action, **args)))) is True


async def set_setting(ui, key, page, label, enabled):
    if (await snapshot_preferences(ui)).get(key) is enabled:
        return
    token = os.urandom(16).hex()
    async def step(action):
        if not await settings_navigation(ui, action, token=token, page=page, label=label, enabled=enabled):
            raise Blocked('settings_navigation_unavailable')
    async def observe(action):
        async def ready():
            return await settings_navigation(ui, action, token=token, page=page, label=label, enabled=enabled)
        await wait_condition(ready)
    try:
        await step('begin')
        await step('open')
        await observe('mounted')
        await step('page')
        await observe('ready')
        await step('toggle')  # Click once; confirmation only observes canonical preferences.
        async def confirmed():
            return (await snapshot_preferences(ui)).get(key) is enabled
        await wait_condition(confirmed)
    finally:
        try:
            await step('back')
            await observe('overview')
            await step('close')
            await observe('closed')
            await step('finish')
        finally:
            await step('finalize')


async def set_web(ui, enabled):
    await set_setting(ui, 'web_enabled', 'web', 'Allow web access', enabled)


async def set_tts(ui, enabled):
    await set_setting(ui, 'tts_enabled', 'voice', 'Speak replies', enabled)


async def select_model(ui, model):
    if (await snapshot_preferences(ui)).get('instant') == model:
        return
    changed = await ui.evaluate(f"""(async () => {{
      {IDLE_FENCE}
      const e = document.getElementById('composerInstantSelect');
      const o = Array.from(e?.options || []).find(o => o.value === {json.dumps(model)} && !o.disabled);
      const restorable = Array.from(e?.options || []).some(o => o.value === e.value && !o.disabled);
      if (!e || e.disabled || !e.getClientRects().length || !o || !restorable) return false;
      e.value = o.value; e.dispatchEvent(new Event('change', {{bubbles:true}})); return true;
    }})()""")
    if changed is not True:
        raise Blocked('semantic_model_unavailable')
    async def confirmed():
        return (await snapshot_preferences(ui)).get('instant') == model
    await wait_condition(confirmed)


BEGIN_OWNERSHIP_JS = """(async () => {
      const token = OWNER_TOKEN;
      if (window.__voxBenchmarkOwner || window.__voxBenchmarkAcquisition) return false;
      const pending = {token};
      window.__voxBenchmarkAcquisition = pending;
      try {
        const s = await window.__TAURI_INTERNALS__.invoke('get_app_snapshot');
        // CDP can stop waiting while this native promise keeps running.
        if (window.__voxBenchmarkAcquisition !== pending || window.__voxBenchmarkOwner ||
            s.active_request !== null || s.runtime_phase !== 'sleeping' ||
            document.getElementById('promptComposer')?.value !== '' ||
            document.querySelector('[role=dialog]')) return false;
        window.__voxBenchmarkOwner = {token, session:s.session_id, request:null, execution:null,
          baseline:JSON.stringify(s.transcript.map(t => [t.request_id,t.execution_id]))};
        return true;
      } finally {
        if (window.__voxBenchmarkAcquisition === pending) delete window.__voxBenchmarkAcquisition;
      }
    })()"""


async def begin_ownership(ui, token=None):
    token = token or os.urandom(16).hex()
    result = await ui.evaluate(BEGIN_OWNERSHIP_JS.replace('OWNER_TOKEN', json.dumps(token)))
    if result is not True:
        raise Blocked('existing_draft_or_activity_must_not_be_touched')


async def ownership(ui, action='check', request=None):
    if request is not None and (type(request) is not str or not 1 <= len(request) <= 128):
        raise Blocked('admission_identity_invalid')
    result = await ui.evaluate(f"""(async () => {{
      const o = window.__voxBenchmarkOwner;
      const s = await window.__TAURI_INTERNALS__.invoke('get_app_snapshot');
      if (!o || o.session !== s.session_id) return 'foreign';
      const action = {json.dumps(action)}, request = {json.dumps(request)};
      if (action === 'claim') {{
        const t = s.active_request?.request_id === request ? s.active_request :
          s.transcript.find(t => t.request_id === request);
        if (!t && !s.active_request) return 'pending';
        if (!t?.execution_id || t.execution_id.session_id !== o.session ||
            (s.active_request && s.active_request.request_id !== request)) return 'foreign';
        if (o.request !== request) o.before = JSON.stringify(s.transcript.map(t => [t.request_id,t.execution_id]));
        o.request = request; o.execution = JSON.stringify(t.execution_id);
      }}
      if (!s.active_request) {{
        const last = s.transcript.at(-1);
        const unchanged = o.request === null ?
          JSON.stringify(s.transcript.map(t => [t.request_id,t.execution_id])) === o.baseline :
          (last?.request_id === o.request && JSON.stringify(last?.execution_id) === o.execution) ||
            JSON.stringify(s.transcript.map(t => [t.request_id,t.execution_id])) === o.before;
        return !unchanged ? 'foreign' : s.runtime_phase === 'sleeping' ? 'idle' : 'pending';
      }}
      if (s.active_request.request_id !== o.request ||
          JSON.stringify(s.active_request.execution_id) !== o.execution) return 'foreign';
      if (action === 'cancel') {{
        // Address the proven request, never a composer Stop closure that could
        // target a concurrently replaced successor after this snapshot.
        await window.__TAURI_INTERNALS__.invoke('cancel_prompt', {{requestId:o.request}});
      }}
      return 'owned_active';
    }})()""")
    if result not in ('idle', 'owned_active', 'foreign', 'pending'):
        raise Blocked('ownership_observation_invalid')
    return result


async def ensure_owned_idle(ui):
    if await ownership(ui) != 'idle':
        raise Blocked('ownership_fence_not_idle')


async def confirm_configuration(ui, model, web):
    prefs = await snapshot_preferences(ui)
    if any(prefs.get(k) is not v for k, v in
           [('web_enabled', web), ('tts_enabled', False), ('prefetch', False), ('completion', False)]):
        raise Blocked('typed_settings_not_confirmed')
    if prefs.get('instant') != model:
        raise Blocked('existing_model_not_confirmed')
    if prefs.get('deep_enabled') is True or prefs.get('review_enabled') is True:
        raise Blocked('single_generation_profile_required')


async def new_chat(ui):
    await ensure_owned_idle(ui)
    if await ui.evaluate("document.getElementById('promptComposer')?.value === ''") is not True:
        raise Blocked('existing_draft_must_not_be_touched')
    await click(ui, 'New Chat')
    async def empty():
        return await ui.evaluate("""(async () => {
          const s = await window.__TAURI_INTERNALS__.invoke('get_app_snapshot');
          const ready = !!window.__voxBenchmarkOwner && s.session_id !== window.__voxBenchmarkOwner.session &&
            s.active_request === null && s.transcript.length === 0 &&
            !document.querySelector('[data-turn-role]') &&
            document.getElementById('promptComposer')?.value === '';
          if (ready) window.__voxBenchmarkNextSession = s.session_id;
          return ready;
        })()""") is True
    await wait_condition(empty)
    changed = await ui.evaluate("""(async () => {
      const s = await window.__TAURI_INTERNALS__.invoke('get_app_snapshot');
      if (s.session_id !== window.__voxBenchmarkNextSession || s.active_request !== null ||
          s.transcript.length !== 0 || document.getElementById('promptComposer')?.value !== '') return false;
      window.__voxBenchmarkOwner = {token:window.__voxBenchmarkOwner.token,
        navigationFailed:window.__voxBenchmarkOwner.navigationFailed,
        session:s.session_id, request:null, execution:null, baseline:'[]'};
      window.__voxBenchmarkExecutions = [];
      delete window.__voxBenchmarkNextSession;
      return true;
    })()""")
    if changed is not True:
        raise Blocked('new_chat_session_fence_changed')


async def terminal_validation(ui, case, request, metrics):
    result = await ui.evaluate(f"""(async () => {{
      const id = {json.dumps(request)};
      const s = await window.__TAURI_INTERNALS__.invoke('get_app_snapshot');
      const turns = s.transcript.filter(t => t.request_id === id);
      const rows = Array.from(document.querySelectorAll('[data-turn-role="assistant"][data-request-id]'))
        .filter(e => e.dataset.requestId === id);
      if (turns.length !== 1 || rows.length !== 1 || s.active_request !== null) return null;
      const t = turns[0], row = rows[0];
      const execution = JSON.stringify(t.execution_id);
      const owner = window.__voxBenchmarkOwner;
      if (!owner || owner.session !== s.session_id || owner.request !== id || owner.execution !== execution) return null;
      const fresh = Array.isArray(window.__voxBenchmarkExecutions) &&
        !window.__voxBenchmarkExecutions.includes(execution);
      if (!fresh || t.user.content.text !== {json.dumps(case['prompt'])}) return null;
      window.__voxBenchmarkExecutions.push(execution);
      const answer = row.querySelector('.answer-stage__answer, .message__content')?.textContent;
      const sources = Array.from(row.querySelectorAll('.answer-stage__sources a')).map(a => a.getAttribute('href'));
      return {{canonical: t.answer.content.text, answer, sources,
        canonical_sources:t.sources.map(s => s.url),
        execution_valid:!!t.execution_id && t.execution_id.session_id === s.session_id &&
          typeof t.execution_id.generation === 'string' && t.execution_id.generation.length > 0}};
    }})()""")
    if not isinstance(result, dict) or result.get('execution_valid') is not True:
        raise Blocked('canonical_execution_correlation_failed')
    if any(type(result.get(k)) is not str or len(result[k].encode()) > 256 * 1024 for k in ('answer', 'canonical')):
        raise Blocked('bounded_synthetic_answer_unavailable')
    for name in ('sources', 'canonical_sources'):
        if type(result.get(name)) is not list or len(result[name]) > 32 or any(
                type(url) is not str or len(url.encode()) > 4096 for url in result[name]):
            raise Blocked('bounded_synthetic_sources_unavailable')
    metrics['source_counts'] = {'ui':len(result['sources']), 'canonical':len(result['canonical_sources'])}
    checks = validate(case, result['answer'], result['sources'], result['canonical_sources'])
    checks['canonical_answer_equal'] = 'pass' if result['answer'] == result['canonical'] else 'fail'
    checks['canonical_execution_and_single_ui_row'] = 'pass'
    metrics['model_reported_rate_limit'] = model_reported_rate_limit(case, result, checks)
    return checks


# Owner-token-scoped internal release also works after canonical fences change.
# It deliberately leaves all UI and foreign bookkeeping untouched.
RELEASE_OWNERSHIP_JS = """(() => {
  const token = ARGS;
  const owner = window.__voxBenchmarkOwner;
  const n = window.__voxBenchmarkNavigation;
  const pending = window.__voxBenchmarkAcquisition;
  const ownsPending = pending?.token === token;
  if (ownsPending) delete window.__voxBenchmarkAcquisition;
  let clean = !n && (!pending || ownsPending);
  if (n && n.owner.token === token) delete window.__voxBenchmarkNavigation;
  if (!owner) return clean;
  if (owner.token !== token) return false;
  clean = clean && !owner.navigationFailed;
  delete window.__voxBenchmarkExecutions;
  delete window.__voxBenchmarkOwner;
  delete window.__voxBenchmarkNextSession;
  return clean;
})()"""


class DOMSuiteDriver:
    """Default adapter; semantic drivers implement these async operations instead.

    Native drivers additionally provide exercise(model, prompt, **callbacks) and
    read_native_status(request). Exercise must preserve the smoke observer callback
    contract: ready before submission, submitted once, admitted identity, then
    terminal with metrics in the adapter's declared monotonic observer clock.
    Status must be scoped to that execution; never combine host/device clocks.
    Ownership/control methods must fence every mutation against foreign work;
    clear_owned_draft only clears an owned draft matching the supplied prompts.
    """
    def __init__(self, ui, exercise):
        self.ui = ui
        self._exercise = exercise
        self.owner_token = os.urandom(16).hex()

    async def snapshot_preferences(self): return await snapshot_preferences(self.ui)
    async def begin_ownership(self): return await begin_ownership(self.ui, self.owner_token)
    async def ownership(self, action='check', request=None): return await ownership(self.ui, action, request)
    async def set_tts(self, value): return await set_tts(self.ui, value)
    async def set_web(self, value): return await set_web(self.ui, value)
    async def select_model(self, value): return await select_model(self.ui, value)
    async def confirm_configuration(self, model, web): return await confirm_configuration(self.ui, model, web)
    async def new_chat(self): return await new_chat(self.ui)
    async def terminal_validation(self, case, request, metrics):
        return await terminal_validation(self.ui, case, request, metrics)
    async def exercise(self, model, prompt, **kwargs):
        return await self._exercise(self.ui, model, prompt, **kwargs)
    async def clear_owned_draft(self, prompts):
        cleared = await self.ui.evaluate(f"""(async () => {{
          {IDLE_FENCE}
          const e = document.getElementById('promptComposer');
          if (e && {json.dumps(prompts)}.includes(e.value)) {{
            Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(e, '');
            e.dispatchEvent(new Event('input', {{bubbles:true}}));
          }}
          return true;
        }})()""")
        if cleared is not True:
            raise Blocked('cleanup_ownership_fence_changed')
    async def release_ownership(self):
        return await self.ui.evaluate(RELEASE_OWNERSHIP_JS.replace('ARGS', json.dumps(self.owner_token))) is True


async def exercise_semantic_with_native_status(driver, model, prompt, *, row, web_enabled, **kwargs):
    """Read execution-scoped native status before shared terminal classification.

    Invalid/missing observations stay unverified, never manufacture a blocker.
    Failed exercises remain failed and are not retried.
    """
    if web_enabled:
        row['native_rate_limit_observation'] = 'unverified'
        terminal = kwargs['terminal_observer']
        async def observed_terminal(request, metrics):
            try:
                records = checked_native_diagnostics(await driver.read_native_status(request))
            except Exception:
                records = []
            row['native_rate_limits'] = records
            row['native_rate_limit_observation'] = 'observed' if records else 'unverified'
            await terminal(request, metrics)
        kwargs['terminal_observer'] = observed_terminal
    return await driver.exercise(model, prompt, **kwargs)


async def run_suite(ui, args, report, exercise=None, stages=None, *, driver=None):
    # Includes bounded admission/transition overhead; cleanup may additionally need
    # bounded cancellation/control deadlines when a timed-out operation is retired.
    selected = select_scenarios(args, report)
    needed = planned_admissions(args, selected)
    deadline = needed * (args.turn_timeout + 30) + 180
    report['suite_deadline_seconds'] = deadline
    async with asyncio.timeout(deadline):
        await _run_suite(ui, args, report, exercise, stages, driver=driver)


async def _run_suite(ui, args, report, exercise=None, stages=None, *, driver=None):
    semantic = driver is not None
    if semantic and exercise is not None:
        raise Blocked('semantic_driver_owns_exercise')
    if not semantic:
        exercise = exercise or smoke_module().exercise_ui
        driver = DOMSuiteDriver(ui, exercise)
    async def ensure_idle():
        if await driver.ownership() != 'idle':
            raise Blocked('ownership_fence_not_idle')
    cases = select_scenarios(args, report)  # Validate the full source before selection/UI mutation.
    original = await driver.snapshot_preferences()
    # This runner does not manufacture a desktop preference interface. Unsupported
    # enabled settings fail before generating rather than being changed natively.
    if type(original.get('instant')) is not str or original.get('prefetch') is not False or original.get('completion') is not False:
        raise Blocked('preconfigured_prefetch_completion_required')
    if original.get('deep_enabled') is True or original.get('review_enabled') is True:
        raise Blocked('single_generation_profile_required')
    try:
        await driver.begin_ownership()
    except BaseException:
        if not semantic:
            try:
                clean = await driver.release_ownership()
                report['cleanup']['navigation'] = 'pass' if clean else 'fail'
            except BaseException:
                report['cleanup']['navigation'] = 'fail'
        raise
    report.setdefault('admissions', {'submitted':0, 'observed':0, 'budget':getattr(args, 'max_live_calls', report['planned_admissions'])})
    observed_requests = set()
    async def submitted():
        report['admissions']['submitted'] += 1
    async def admitted(request):
        if request not in observed_requests:
            observed_requests.add(request)
            report['admissions']['observed'] += 1
        async def claimed():
            state = await driver.ownership('claim', request)
            if state == 'foreign':
                raise Blocked('admitted_execution_not_owned')
            return state != 'pending'
        await wait_condition(claimed)
    try:
        await ensure_idle()
        await driver.set_tts(False)
        await ensure_idle()
        await driver.select_model(args.model)
        for repetition in range(1, args.repetitions + 1):
            for case in cases:
                row = {'scenario': case['id'], 'repetition': repetition,
                       'web_enabled': case['web_enabled'], 'status': 'fail', 'checks': {}, 'workflow_completed':False}
                report['cases'].append(row)
                if case.get('history_condition'):
                    row['history_condition'] = case['history_condition']
                    row['history_seed_turns'] = getattr(args, 'history_seed_turns', 1)
                    if row['history_seed_turns'] != 1:
                        row['history_condition'] = 'numbered-normal-ui-seeds-plus-followup-not-24-message-host-history'
                setup_started = time.perf_counter()
                await driver.new_chat()
                row['new_chat_ready_observed_ms'] = (time.perf_counter() - setup_started) * 1000
                await ensure_idle()
                await driver.set_web(case['web_enabled'])
                async def confirm():
                    await ensure_idle()
                    if report['admissions']['submitted'] >= report['admissions']['budget']:
                        raise Blocked('ui_admission_budget_exhausted')
                    await driver.confirm_configuration(args.model, case['web_enabled'])
                async def observed_exercise(prompt, target, terminal_observer, native=False):
                    turn = None
                    async def ready():
                        nonlocal turn
                        await confirm()
                        if stages:
                            turn = stages.before(target, args.model, case['web_enabled'])
                    async def observed_terminal(request, metrics):
                        # The shared smoke observer has stopped its host timer
                        # before invoking this callback. Never charge ADB to UI latency.
                        if stages and turn is not None:
                            stages.after(turn)
                        await terminal_observer(request, metrics)
                    kwargs = dict(turn_timeout=args.turn_timeout, confirm_ready=ready,
                                  terminal_observer=observed_terminal, admission_observer=admitted,
                                  submission_observer=submitted)
                    if native:
                        if semantic:
                            return await exercise_semantic_with_native_status(driver, args.model, prompt,
                                row=row, web_enabled=case['web_enabled'], **kwargs)
                        return await exercise_with_native_status(ui, exercise, args.model, prompt,
                            row=row, cleanup=report['cleanup'], web_enabled=case['web_enabled'], **kwargs)
                    return await driver.exercise(args.model, prompt, **kwargs)
                if 'seed' in case:
                    # A normal UI-admitted synthetic seed, never injected native history.
                    row['seed_profiles'] = []
                    for index, prompt in enumerate(seed_prompts(args, case)):
                        profile = {'seed_number':index + 1}
                        row['seed_profiles'].append(profile)
                        seed_case = {'id':'history_seed', 'expected':'OK', 'allows_no_sources':True, 'prompt':prompt}
                        async def seed_terminal(request, metrics):
                            profile.update(checked_metrics(metrics))
                            row['seed_metrics'] = checked_metrics(metrics)
                            try:
                                checks = await driver.terminal_validation(seed_case, request, metrics)
                            finally:
                                profile.update(checked_metrics(metrics))
                                row['seed_metrics'] = checked_metrics(metrics)
                            if 'fail' in checks.values():
                                raise Blocked('history_seed_validation_failed')
                        await observed_exercise(prompt, profile, seed_terminal)
                    row['seed'] = 'pass'
                async def terminal(request, metrics):
                    row.update(checked_metrics(metrics))
                    row['workflow_completed'] = True
                    try:
                        row['checks'] = await driver.terminal_validation(case, request, metrics)
                        native = row.get('native_rate_limits', [])
                        if native and source_failure_only(case, row['checks'], metrics.get('source_counts')):
                            row.update(status='blocked', reason='rate_limited', evidence='native_provider_status',
                                       provenance='native_provider_status', provider_reset=('minimum_retry_delay' if any(
                                           r['provider_retry_after_seconds'] is not None for r in native) else 'unknown'))
                        elif not native and metrics.get('model_reported_rate_limit') is True:
                            row.update(status='blocked', reason='rate_limited', evidence='model_reported',
                                       provenance='model_reported_rate_limit', provider_reset='unknown', retry_after_seconds=None)
                    finally:
                        row.update(checked_metrics(metrics))
                metrics = await observed_exercise(case['prompt'], row, terminal, native=True)
                row.update(checked_metrics(metrics))
                if row['status'] == 'blocked':
                    report['status'] = 'blocked'
                    return  # Cleanup still runs; never retry or admit the remaining cases.
                row['status'] = ('fail' if 'fail' in row['checks'].values() else
                                 'unverified' if 'unverified' in row['checks'].values() else 'pass')
                if row['status'] == 'fail':
                    raise Blocked('scenario_validation_failed')
        report['status'] = 'unverified' if report['coverage'] == 'partial' or any(r['status'] == 'unverified' for r in report['cases']) else 'pass'
    finally:
        try:
            # Never echo preferences. Restore only settings this controller changes.
            report['cleanup']['preferences'] = 'fail'
            errors = False
            try:
                state = await driver.ownership()
                if state == 'foreign':
                    raise Blocked('cleanup_foreign_session_or_execution')
                if state in ('owned_active', 'pending'):
                    if state == 'owned_active' and await driver.ownership('cancel') == 'foreign':
                        raise Blocked('cleanup_foreign_session_or_execution')
                    async def idle():
                        state = await driver.ownership()
                        if state == 'foreign':
                            raise Blocked('cleanup_foreign_session_or_execution')
                        return state == 'idle'
                    await wait_condition(idle, 20)
                await ensure_idle()
                prompts = [c['prompt'] for c in cases] + [prompt for c in cases if 'seed' in c for prompt in seed_prompts(args, c)]
                await driver.clear_owned_draft(prompts)
            except BaseException:
                # A foreign successor must never be stopped or have its preferences changed.
                report['cleanup']['preferences'] = 'blocked_ownership_fence'
                raise Blocked('cleanup_ownership_or_retirement_incomplete') from None
            # Attempt each restoration even if a different control or cancellation failed.
            for restore, value in ((driver.select_model, original['instant']), (driver.set_web, original['web_enabled']), (driver.set_tts, original['tts_enabled'])):
                try:
                    await ensure_idle()
                    await restore(value)
                except BaseException:
                    errors = True
            try:
                restored = await driver.snapshot_preferences()
                errors |= any(restored.get(k) != original.get(k) for k in original)
            except BaseException:
                errors = True
            if errors:
                raise Blocked('cleanup_restore_incomplete')
            report['cleanup']['preferences'] = 'pass'
        finally:
            try:
                released = await driver.release_ownership()
            except BaseException:
                if not semantic:
                    report['cleanup']['navigation'] = 'fail'
                raise
            if not semantic:
                report['cleanup']['navigation'] = 'pass' if released else 'fail'
                if not released:
                    raise Blocked('cleanup_restore_incomplete')


def checked_metrics(metrics):
    keys = ('first_answer_ms', 'completed_ms')
    if type(metrics) is not dict or any(type(metrics.get(k)) not in (float, int) or
            not math.isfinite(metrics[k]) or not 0 <= metrics[k] <= 610000 for k in keys) or metrics[keys[0]] > metrics[keys[1]]:
        raise Blocked('observation_metrics_invalid')
    result = {k:metrics[k] for k in keys}
    if 'source_counts' in metrics:
        counts = metrics['source_counts']
        if type(counts) is not dict or set(counts) != {'ui', 'canonical'} or any(type(v) is not int or not 0 <= v <= 32 for v in counts.values()):
            raise Blocked('source_count_metrics_invalid')
        result['source_counts'] = dict(counts)
    return result


def build_identity(adb, args):
    model = adb.run('shell', 'getprop', 'ro.product.model')
    api = adb.run('shell', 'getprop', 'ro.build.version.sdk')
    abi = adb.run('shell', 'getprop', 'ro.product.cpu.abi')
    if model != 'SM-A176B' or not api.isdecimal() or not MIN_ANDROID_API <= int(api) <= 99 or abi != 'arm64-v8a':
        raise Blocked('a17_runtime_identity_mismatch')
    paths = adb.run('shell', 'pm', 'path', PACKAGE).splitlines()
    if len(paths) != 1 or not paths[0].startswith('package:'):
        raise Blocked('single_installed_apk_required')
    path = paths[0][len('package:'):]
    if not re.fullmatch(r'/[A-Za-z0-9_./=+~\-]+\.apk', path):
        raise Blocked('installed_apk_path_invalid')
    digest = adb.run('shell', 'sha256sum', path).split()[0]
    package = adb.run('shell', 'dumpsys', 'package', PACKAGE)
    version = re.search(r'\bversionCode=(\d+)', package)
    if not re.fullmatch(r'[a-f0-9]{64}', digest) or not version:
        raise Blocked('installed_build_identity_unavailable')
    flags = re.search(r'\b(?:pkgFlags|flags)=\[([^\]\n]*)\]', package)
    result = {'model':'SM-A176B', 'api':int(api), 'abi':abi, 'installed_apk_sha256':digest,
              'version_code':int(version[1]), 'debuggable':('DEBUGGABLE' in flags[1].split()) if flags else None}
    variant = getattr(args, 'build_variant', 'unspecified')
    required_debug = True if variant == 'debug' else False if variant in ('release', 'profileable') else None
    if required_debug is not None and result['debuggable'] is not required_debug:
        raise Blocked('build_variant_debuggability_mismatch')
    if args.apk:
        if not args.apk.is_file() or args.apk.stat().st_size > 512 * 1024 * 1024:
            raise Blocked('provided_apk_regular_bounded_file_required')
        h = hashlib.sha256()
        with args.apk.open('rb') as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b''):
                h.update(chunk)
        result['provided_apk_sha256'] = h.hexdigest()
        if result['provided_apk_sha256'] != digest:
            raise Blocked('provided_installed_apk_hash_mismatch')
    return result


def source_identity():
    revision = subprocess.run(['git', 'rev-parse', 'HEAD'], cwd=ROOT,
                              capture_output=True, text=True, timeout=10)
    dirty = subprocess.run(['git', 'status', '--porcelain'], cwd=ROOT,
                           capture_output=True, text=True, timeout=10)
    harness = hashlib.sha256()
    wear = ROOT / 'apps/wear'
    assistant = wear / 'app/src/main/java/dev/gitblame/voxgolem/android/wear/assistant'
    for path in [Path(__file__), ROOT / 'scripts/android-e2e-smoke.py', Path(__file__).with_name('desktop.py'), Path(__file__).with_name('stages.py'),
                 Path(__file__).with_name('wear.py'), assistant / 'TypedAskBenchmark.kt',
                 assistant / 'AssistantActivity.kt', assistant / 'AssistantPresentation.kt', wear / 'app/assistant-proguard-rules.pro',
                 ROOT / 'crates/assistant-android/src/main/java/dev/gitblame/voxgolem/assistant/runtime/AssistantRuntime.kt',
                 wear / 'app/src/main/AndroidManifest.xml',
                 wear / 'app/src/performance/AndroidManifest.xml',
                 wear / 'voice-benchmark/src/main/AndroidManifest.xml',
                 wear / 'voice-benchmark/src/main/java/dev/gitblame/voxgolem/android/wear/speechtest/TypedAskInstrumentation.java']:
        if path.is_file():
            harness.update(path.name.encode() + b'\0' + path.read_bytes() + b'\0')
    return {'revision': revision.stdout.strip() if revision.returncode == 0 and
            re.fullmatch(r'[0-9a-f]{40,64}\n?', revision.stdout) else 'unverified',
            'dirty': bool(dirty.stdout) if dirty.returncode == 0 else None,
            'scenario_sha256':hashlib.sha256(Path(__file__).with_name('scenarios.json').read_bytes()).hexdigest(),
            'harness_sha256':harness.hexdigest()}


async def ensure_android_webview_focus(ui, adb, report):
    async def focused():
        return await ui.evaluate("document.visibilityState === 'visible' && document.hasFocus()") is True

    try:
        if await focused():
            return
        safe = await ui.evaluate("""(async () => {
          const s = await window.__TAURI_INTERNALS__.invoke('get_app_snapshot');
          const composer = document.getElementById('promptComposer');
          const model = document.getElementById('composerInstantSelect');
          return document.visibilityState === 'visible' && s.active_request === null &&
            s.runtime_phase === 'sleeping' && !document.querySelector('[role=dialog]') &&
            !!composer && composer.value === '' && !composer.disabled && !composer.readOnly &&
            !!composer.getClientRects().length && !!model && !model.disabled &&
            !!model.getClientRects().length && Array.from(model.options).some(
              o => o.value === model.value && o.value !== '' && !o.disabled);
        })()""")
        if safe is not True:
            raise Blocked('webview_not_visible_focused')
        policy = adb.run('shell', 'dumpsys', 'window', 'policy')
        if any(re.findall(r'\b' + key + r'=(true|false)\b', policy) != ['false']
               for key in ('showing', 'inputRestricted')):
            raise Blocked('webview_not_visible_focused')
        windows = adb.run('shell', 'dumpsys', 'window')
        current = [line for line in windows.splitlines() if 'mCurrentFocus=' in line]
        if len(current) != 1 or not re.search(r'\s' + re.escape(PACKAGE) + r'/', current[0]):
            raise Blocked('webview_not_visible_focused')
        # One native focus traversal only, after proving no draft/request/keyguard.
        adb.run('shell', 'input', 'keyevent', 'KEYCODE_TAB')
        report['setup_action'] = 'native_keyboard_tab'
        await wait_condition(focused, timeout=3)
    except Exception:
        raise Blocked('webview_not_visible_focused') from None


async def run_android(args, report):
    try:
        import websockets
    except ImportError:
        raise Blocked('existing_websockets_dependency_required') from None
    adb = ADB(args.adb, args.ssh, args.remote_adb)
    adb.device = select_a17(adb.run('devices', '-l'))
    report['build'] = build_identity(adb, args)
    pid = adb.run('shell', 'pidof', PACKAGE)
    if not pid.isdecimal():
        raise Blocked('existing_single_running_app_required')
    # No installation, data clearing, keyguard dismissal or user text inspection.
    adb.run('shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
    adb.run('shell', 'am', 'start', '-W', '-n', PACKAGE + '/.MainActivity')
    pid = adb.run('shell', 'pidof', PACKAGE)
    if not pid.isdecimal():
        raise Blocked('foreground_single_app_process_required')
    focus = adb.run('shell', 'dumpsys', 'window')
    if not any(PACKAGE in line for line in focus.splitlines() if 'mCurrentFocus=' in line):
        raise Blocked('app_not_focused')
    with owned_forward(adb, args.port, pid, report['cleanup']), owned_tunnel(args.ssh, args.port, report['cleanup']):
        with urllib.request.urlopen(f'http://127.0.0.1:{args.port}/json', timeout=10) as response:
            targets = json.loads(response.read(1024 * 1024))
        candidates = [t for t in targets if t.get('type') == 'page' and
                      urlsplit(t.get('url', '')).hostname in ('tauri.localhost', 'localhost')]
        if len(candidates) != 1:
            raise Blocked('unique_app_webview_required')
        parsed = urlsplit(candidates[0].get('webSocketDebuggerUrl', ''))
        if parsed.scheme != 'ws' or not parsed.path.startswith('/devtools/page/'):
            raise Blocked('invalid_webview_socket')
        url = urlunsplit(('ws', f'127.0.0.1:{args.port}', parsed.path, '', ''))
        async with websockets.connect(url, open_timeout=10, close_timeout=10, max_size=1024*1024) as socket:
            ui = smoke_module().CDP(socket, 10)
            await ensure_android_webview_focus(ui, adb, report)
            stages = None
            if getattr(args, 'collect_stages', False):
                spec = importlib.util.spec_from_file_location('typed_benchmark_stages', Path(__file__).with_name('stages.py'))
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                stages = module.Collector(adb)
                report['stage_collection'] = {'platform':'android', 'clock':'device-boottime-selection-only',
                    'status':'requested', 'boundary':'same-generation-Android-elapsedNanos-and-native-own-clock-durations',
                    'overlap':'inclusive-not-additive-store_total-native_dispatch-projection-refresh_reload-quota',
                    'sqlite':'elapsed-not-pure-CPU-BEGIN-may-busy-wait-commit-WAL-not-isolated-fsync'}
            try:
                await run_suite(ui, args, report, stages=stages)
            finally:
                if stages:
                    stages.finish()
                    profiles = [p for row in report['cases'] for p in [row] + row.get('seed_profiles', [])]
                    report['stage_collection']['status'] = ('observed' if profiles and all(
                        p.get('local_stages', {}).get('coverage') == 'verified' for p in profiles) else 'unverified')
                    report['stage_collection']['repeat_summary'] = module.repeat_summary(report['cases'])
                    module.shape_report(report, MAX_REPORT_BYTES)
                    if report['stage_collection']['status'] == 'unverified' and report['status'] == 'pass':
                        report['status'] = 'unverified'


def main():
    args = parser().parse_args()
    if not (args.platform == 'desktop' and args.workload in ('synthetic-voice', 'typed-speech')):
        try:
            reject_desktop_voice_options(args)
        except Blocked as error:
            print(str(error), file=sys.stderr)
            return 1
    if args.workload == 'typed-speech':
        if args.platform != 'desktop':
            print('typed_speech_requires_desktop_windows', file=sys.stderr)
            return 1
        import desktop_voice
        return desktop_voice.main(args, sys.modules[__name__])
    if args.workload == 'synthetic-voice':
        if args.platform == 'desktop':
            import desktop_voice
            return desktop_voice.main(args, sys.modules[__name__])
        if args.platform == 'android':
            import phone_voice
            return phone_voice.main(args, sys.modules[__name__])
        import wear_voice
        return wear_voice.main(args, sys.modules[__name__])
    if args.list:
        for case in select_scenarios(args):
            print(f"{case['id']} web={'on' if case['web_enabled'] else 'off'}")
        return 0
    report = {'schema': 1, 'suite': 'typed-model-matrix', 'platform': args.platform,
              'status': 'unverified', 'cases': [],
              'cleanup': {'preferences':'not_touched', 'forward':'not_acquired', 'tunnel':'not_acquired',
                          'forward_acquired':False, 'tunnel_acquired':False, 'tunnel_ready':False},
              'measurement': {'clock': 'host-monotonic', 'poll_interval_ms': 100,
                  'boundary': 'UI-submit-to-observed-answer-and-terminal-idle',
                  'transport_overhead': 'included-not-subtracted',
                  'condition': ('new-private-desktop-process-provider-coldwarm-uncontrolled' if args.platform == 'desktop'
                                else 'already-running-app-provider-coldwarm-uncontrolled')},
              'repetitions': args.repetitions, 'model': None, 'build_variant': args.build_variant,
              'admission_budget':args.max_live_calls,
              'limitations': ['No painted-pixel, provider-only, acoustic or startup measurement.',
                  'No matched baseline; no speedup claim.', 'Windows and Wear unsupported.',
                  'Page reads require execution evidence; sources alone do not prove them.']}
    report['limitations'] += ['Synthetic conversations are retained; no app data is deleted.',
                              'Installed signing certificate not independently verified.',
                               'Budget bounds UI admissions, not native internal tool or provider retries.']
    if args.platform == 'wear':
        report['measurement'] = {
            'clock':'device-elapsed-realtime',
            'boundary':'native-UI-Send-action-to-observed-first-projected-text-and-terminal',
            'transport_overhead':'excluded-from-device-timing',
            'condition':'isolated-typed-session-production-config-setup-not-measured-user-selector-flow',
            'accessibility':'separate-evidence-not-painted-pixels'}
        report['limitations'] = [item for item in report['limitations'] if item != 'Windows and Wear unsupported.']
        report['limitations'].append('Windows unsupported; Wear evidence is scoped to the native typed adapter.')
        if args.collect_stages:
            report['stage_collection'] = {'status':'unsupported', 'reason':'wear_collector_unavailable'}
    if args.collect_stages and args.platform == 'desktop':
        report['stage_collection'] = {'status':'unsupported', 'reason':'desktop_collector_unavailable'}
    def interrupted(signum, frame):
        raise Blocked('interrupted')
    previous = {s: signal.signal(s, interrupted) for s in (signal.SIGINT, signal.SIGTERM)}
    output_ready, report_path = False, None
    try:
        try:
            args.output = preflight_output(args.output)
        except OSError:
            raise Blocked('report_preflight_failed') from None
        output_ready = True
        select_scenarios(args, report)
        authorize(args)
        report['source'] = source_identity()
        report['model'] = args.model
        if args.platform == 'desktop':
            # A child must propagate its actual counters. Missing evidence is
            # unknown, never a fabricated zero after possible live execution.
            report['admissions'] = {'submitted':None, 'observed':None, 'budget':args.max_live_calls}
            # The desktop module owns qualification, namespace and fixed ledger gates.
            spec = importlib.util.spec_from_file_location('typed_benchmark_desktop', Path(__file__).with_name('desktop.py'))
            if not Path(spec.origin).is_file():
                raise Blocked('desktop_qualified_adapter_unavailable')
            desktop = importlib.util.module_from_spec(spec)
            sys.modules[spec.name] = desktop
            spec.loader.exec_module(desktop)
            import inspect
            result = desktop.run_desktop(args, report)
            if inspect.isawaitable(result):
                asyncio.run(result)
        elif args.platform == 'wear':
            spec = importlib.util.spec_from_file_location('typed_benchmark_wear', Path(__file__).with_name('wear.py'))
            if not Path(spec.origin).is_file():
                raise Blocked('wear_native_adapter_unavailable')
            wear = importlib.util.module_from_spec(spec)
            sys.modules[spec.name] = wear
            spec.loader.exec_module(wear)
            asyncio.run(wear.run_wear(args, report, sys.modules[__name__]))
        else:
            asyncio.run(run_android(args, report))
    except Blocked as error:
        report['failure_category'] = str(error)
        if hasattr(error, 'native_diagnostics'):
            report['native_failure_diagnostics'] = error.native_diagnostics
        report['status'] = 'fail'
    except (TimeoutError, subprocess.TimeoutExpired):
        report['failure_category'] = 'operation_deadline'
        report['status'] = 'fail'
    except BaseException:
        report['failure_category'] = 'operation_failed_payload_omitted'
        report['status'] = 'fail'
    finally:
        for s, handler in previous.items():
            signal.signal(s, handler)
        try:
            if not output_ready:
                raise Blocked('report_preflight_failed')
            report_path = write_report(args.output, report)
        except BaseException:
            report['status'] = 'fail'
            report['report_write'] = 'fail'
        print(json.dumps({'status': report['status'], 'case_count': len(report['cases']),
                          'completion_count':sum(r.get('workflow_completed') is True for r in report['cases']),
                          **{status + '_count':sum(r.get('status') == status for r in report['cases'])
                             for status in ('pass', 'unverified', 'fail', 'unsupported', 'blocked')},
                          'coverage':report.get('coverage'), 'selected_cases':report.get('selected_cases', []),
                          'admissions':report.get('admissions', {'submitted':0, 'observed':0, 'budget':args.max_live_calls}),
                          'report_path':str(report_path) if report_path else None,
                          'cleanup': report['cleanup'], 'failure_category': report.get('failure_category'),
                          'report_write': report.get('report_write', 'pass')}))
    return {'pass':0, 'fail':1, 'unverified':2, 'blocked':3}.get(report['status'], 1)


if __name__ == '__main__':
    sys.exit(main())
