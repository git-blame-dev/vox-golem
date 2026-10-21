"""Native Wear transport for the shared typed suite (no scenario policy here).

Only a verified same-signer instrumentation session enables the target facade.
Its finite, run-owned mailbox is a transport, never a persistent app control API.
"""
import asyncio
import hashlib
import json
from pathlib import Path
import re
import shlex
import subprocess
import tempfile
import time
import uuid

try:
    from benchmark import Blocked
except ImportError:
    class Blocked(Exception):
        """Payload-free transport failure."""


PACKAGE = 'dev.gitblame.voxgolem.android'
RUNNER = PACKAGE + '.test/dev.gitblame.voxgolem.android.wear.speechtest.TypedAskInstrumentation'
MAX_RESPONSE_BYTES = 1024 * 1024
FAILURE_PHASES = frozenset(('install', 'setup_case', 'native_setup', 'launch_activity', 'await_ready',
    'prepare_draft', 'draft_ready', 'send_ready', 'send_click',
    'turn_observation', 'canonical_validation', 'close'))


def select_watch(listing):
    candidates = []
    for line in listing.splitlines():
        fields = line.split()
        if len(fields) >= 3 and fields[1] == 'device' and 'model:SM_L315F' in fields:
            candidates.append(fields[0])
    if len(candidates) != 1:
        raise Blocked('watch_unique_advertised_transport_required')
    return candidates[0]


def build_identity(adb, args):
    model = adb.run('shell', 'getprop', 'ro.product.model')
    api = adb.run('shell', 'getprop', 'ro.build.version.sdk')
    abi = adb.run('shell', 'getprop', 'ro.product.cpu.abi')
    features = adb.run('shell', 'pm', 'list', 'features').splitlines()
    if model != 'SM-L315F' or api != '36' or abi != 'armeabi-v7a' or \
            'feature:android.hardware.type.watch' not in features:
        raise Blocked('watch_runtime_identity_mismatch')
    paths = adb.run('shell', 'pm', 'path', PACKAGE).splitlines()
    if len(paths) != 1 or not re.fullmatch(r'package:/[A-Za-z0-9_./=+~\-]+\.apk', paths[0]):
        raise Blocked('watch_single_installed_apk_required')
    digest = adb.run('shell', 'sha256sum', paths[0][len('package:'):]).split()[0]
    package = adb.run('shell', 'dumpsys', 'package', PACKAGE)
    flags = re.search(r'\b(?:pkgFlags|flags)=\[([^\]\n]*)\]', package)
    version = re.search(r'\bversionCode=(\d+)', package)
    if not flags or 'DEBUGGABLE' in flags[1].split() or not version:
        raise Blocked('watch_non_debuggable_build_required')
    artifact = Path(args.apk)
    if not artifact.is_file() or artifact.is_symlink() or artifact.stat().st_size > 512 * 1024 * 1024:
        raise Blocked('watch_local_artifact_required')
    local = hashlib.sha256(artifact.read_bytes()).hexdigest()
    if not re.fullmatch(r'[0-9a-f]{64}', digest) or digest != local:
        raise Blocked('watch_installed_artifact_mismatch')
    return {'model':model, 'api':int(api), 'abi':abi, 'debuggable':False,
            'version_code':int(version[1]), 'installed_apk_sha256':digest, 'provided_apk_sha256':local}


def decode_response(raw, sequence):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate')
            result[key] = value
        return result
    try:
        if len(raw.encode('utf-8')) > MAX_RESPONSE_BYTES:
            raise ValueError('size')
        value = json.loads(raw, object_pairs_hook=unique)
        if type(value) is not dict or type(value.get('sequence')) is not int or value['sequence'] != sequence:
            raise ValueError('sequence')
        if value.get('status') == 'failed' and value.get('phase') in FAILURE_PHASES:
            error = Blocked('wear_native_' + value['phase'] + '_failed')
            details = value.get('readiness_diagnostics')
            if type(details) is dict:
                flags = ('send_present', 'send_visible', 'send_enabled', 'latest_present', 'latest_visible',
                    'latest_enabled', 'latest_requested', 'latest_acked', 'root_target', 'screen_interactive',
                    'keyguard_locked', 'primary_enabled', 'engine_foreground')
                safe = {key:details[key] for key in flags if type(details.get(key)) is bool}
                if type(details.get('branch_count')) is int and 0 <= details['branch_count'] <= 128:
                    safe['branch_count'] = details['branch_count']
                if details.get('primary_action') in ('SEND', 'STOP', 'ASK', 'TALK'):
                    safe['primary_action'] = details['primary_action']
                error.native_diagnostics = safe
            raise error
        if value.get('status') not in ('configured', 'ready', 'complete', 'blocked', 'cleaned'):
            raise ValueError('result')
        return value
    except (ValueError, TypeError, UnicodeError):
        raise Blocked('wear_command_response_invalid_or_failed') from None


class Mailbox:
    def __init__(self, adb, budget, timeout, model):
        self.adb, self.budget, self.timeout, self.model = adb, budget, timeout, model
        self.run = str(uuid.uuid4())
        self.directory = f'/sdcard/Android/data/{PACKAGE}/files/diagnostics/typed-benchmark/{self.run}'
        self.sequence = 0
        self.process = None
        self.log = None
        self.cleaned = False

    async def start(self):
        # Anonymous host file: bounded categorical instrumentation output only.
        self.log = tempfile.TemporaryFile()
        self.process = subprocess.Popen(self.adb.argv('shell', 'am', 'instrument', '-w', '-r',
            '-e', 'run', self.run, '-e', 'liveProvider', 'true',
            '-e', 'maxTurns', str(self.budget), '-e', 'timeout', str(min(1800, int(self.budget * (self.timeout + 30) + 180))),
            '-e', 'turnTimeoutSeconds', str(int(self.timeout)),
            '-e', 'model', self.model, RUNNER), stdout=self.log, stderr=self.log,
            stdin=subprocess.DEVNULL)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise Blocked('wear_instrumentation_start_failed')
            ready = await asyncio.to_thread(self.adb.run, 'shell',
                f'if [ -f {shlex.quote(self.directory + "/ready.json")} ]; then cat {shlex.quote(self.directory + "/ready.json")}; fi')
            if ready:
                try:
                    value = json.loads(ready)
                    if value.get('status') != 'ready' or value.get('protocol') != 1 or \
                            value.get('max_turns') != self.budget or value.get('model') != self.model or \
                            value.get('turn_timeout_seconds') != int(self.timeout) or \
                            value.get('timeout_seconds') != min(1800, int(self.budget * (self.timeout + 30) + 180)):
                        raise ValueError()
                except (ValueError, TypeError, AttributeError):
                    raise Blocked('wear_ready_protocol_invalid') from None
                return
            await asyncio.sleep(.2)
        raise Blocked('wear_instrumentation_ready_timeout')

    async def command(self, operation, **arguments):
        if not re.fullmatch(r'[a-z_]{1,32}', operation):
            raise Blocked('wear_command_invalid')
        self.sequence += 1
        payload = json.dumps({'sequence':self.sequence, 'command':operation, **arguments},
                             separators=(',', ':'), allow_nan=False).encode()
        if len(payload) > 64 * 1024:
            raise Blocked('wear_command_size_limit')
        target = self.directory + f'/command-{self.sequence}.json'
        shell = f'cat > {shlex.quote(target + ".tmp")} && mv {shlex.quote(target + ".tmp")} {shlex.quote(target)}'
        result = await asyncio.to_thread(subprocess.run, self.adb.argv('shell', shlex.join(['sh', '-c', shell])),
            input=payload, capture_output=True, timeout=20)
        if result.returncode:
            raise Blocked('wear_command_write_failed')
        response = self.directory + f'/response-{self.sequence}.json'
        deadline = time.monotonic() + self.timeout + 30
        while time.monotonic() < deadline:
            raw = await asyncio.to_thread(self.adb.run, 'shell',
                f'if [ -f {shlex.quote(response)} ]; then cat {shlex.quote(response)}; fi')
            if raw:
                return decode_response(raw, self.sequence)
            if self.process.poll() is not None:
                raise Blocked('wear_instrumentation_exited_before_response')
            await asyncio.sleep(.2)
        raise Blocked('wear_command_deadline')

    async def close(self):
        if self.cleaned:
            return
        try:
            if self.process is not None and self.process.poll() is None:
                result = await self.command('close')
                if result.get('status') != 'cleaned':
                    raise Blocked('wear_native_cleanup_incomplete')
                # Acknowledge retrieval only after the native close result is checked.
                await asyncio.to_thread(self.adb.run, 'shell', 'rm', self.directory + '/ready.json')
                try:
                    await asyncio.to_thread(self.process.wait, timeout=15)
                except subprocess.TimeoutExpired:
                    raise Blocked('wear_instrumentation_retirement_timeout') from None
                if self.process.returncode != 0:
                    raise Blocked('wear_instrumentation_exit_failed')
                self.log.seek(0)
                output = self.log.read(64 * 1024 + 1)
                if len(output) > 64 * 1024 or b'typed_benchmark=cleaned' not in output or b'INSTRUMENTATION_CODE: -1' not in output:
                    raise Blocked('wear_instrumentation_completion_unverified')
                self.cleaned = True
            elif self.process is not None:
                raise Blocked('wear_native_cleanup_not_observed')
        finally:
            # Terminating the local transport is not proof native work retired.
            if self.process is not None and self.process.poll() is None:
                self.process.terminate()
                try:
                    await asyncio.to_thread(self.process.wait, timeout=5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    await asyncio.to_thread(self.process.wait, timeout=5)
            if self.log is not None and not self.log.closed:
                self.log.close()


class NativeSuiteDriver:
    """Shared-suite semantics over a private, disposable native session.

    User preferences are never loaded into or modified by this session. Configuration
    is acknowledged by setup_case before each UI submission, not a selector test.
    """
    def __init__(self, mailbox, common, report):
        self.mailbox, self.common, self.report = mailbox, common, report
        self.web = False
        self.configured = False
        self.owned = False
        self.last = None
        self.failed = False
        self.rate_limited = False
        self.requests = set()
        self.disposed = False

    async def snapshot_preferences(self):
        return {'instant':self.mailbox.model, 'web_enabled':self.web, 'tts_enabled':False,
                'prefetch':False, 'completion':False, 'deep_enabled':False, 'review_enabled':False}

    async def begin_ownership(self):
        if self.owned:
            raise Blocked('wear_owner_already_claimed')
        self.owned = True

    async def ownership(self, action='check', request=None):
        if not self.owned:
            return 'foreign'
        if action == 'claim':
            return 'idle' if self.last and self.last.get('request') == request and self.last.get('execution_valid') is True else 'foreign'
        if action == 'cancel':
            # No foreign engine can enter this private namespace. Dispose its
            # entire owner and wait for actual retirement, not an acknowledgement.
            await self.mailbox.close()
            self.disposed = True
            return 'idle'
        if self.disposed or not self.configured:
            return 'idle'
        state = await self.mailbox.command('status')
        if state.get('failed') is True and not self.failed:
            raise Blocked('wear_isolated_engine_failed')
        return 'owned_active' if state.get('busy') is True else 'idle'

    async def set_tts(self, value):
        if value is not False:
            raise Blocked('wear_typed_tts_must_remain_off')

    async def select_model(self, value):
        if value != self.mailbox.model:
            raise Blocked('wear_fixed_model_required')

    async def set_web(self, value):
        if type(value) is not bool:
            raise Blocked('wear_web_policy_invalid')
        if self.failed:
            # Only requested disposable configuration is reset during failure
            # cleanup; native work is retired by release_ownership below.
            self.web = value
            return
        if self.web != value:
            self.web = value
            await self._setup()

    async def _setup(self):
        result = await self.mailbox.command('setup_case', web_enabled=self.web)
        if result.get('status') != 'configured' or result.get('tts') is not False or \
                result.get('configuration') != 'isolated_policy_not_ui':
            raise Blocked('wear_case_configuration_unverified')
        self.configured = True
        self.last = None

    async def new_chat(self):
        await self._setup()

    async def confirm_configuration(self, model, web):
        if not self.configured or self.failed or model != self.mailbox.model or web is not self.web or await self.ownership() != 'idle':
            raise Blocked('wear_configuration_not_confirmed')

    async def exercise(self, model, prompt, **kwargs):
        await kwargs['confirm_ready']()
        # Conservatively count an attempt before handing control to the native
        # runner; its independent budget is consumed before any UI action.
        await kwargs['submission_observer']()
        try:
            result = await self.mailbox.command('submit', prompt=prompt)
        except BaseException:
            self.failed = True
            self.report['admission_observation'] = 'uncertain_after_submit_failure'
            raise
        self.last = result
        self.rate_limited = result.get('native_rate_limit_observed') is True
        if self.rate_limited:
            self.failed = True
            raise Blocked('wear_native_rate_limited')
        if result.get('status') != 'complete' or result.get('submitted') is not True or result.get('execution_valid') is not True:
            self.failed = True
            if result.get('ancestry_valid') is False:
                raise Blocked('wear_native_canonical_ancestry_failed')
            raise Blocked('wear_native_turn_not_complete')
        request = result.get('request')
        if type(request) is not str or not re.fullmatch(r'turn-[1-9][0-9]?', request) or request in self.requests:
            self.failed = True
            raise Blocked('wear_native_execution_identity_invalid')
        self.requests.add(request)
        metrics = self.common.checked_metrics(result)
        await kwargs['admission_observer'](request)
        await kwargs['terminal_observer'](request, metrics)
        return metrics

    async def terminal_validation(self, case, request, metrics):
        value = self.last
        if not value or value.get('request') != request or value.get('execution_valid') is not True:
            raise Blocked('wear_terminal_execution_not_owned')
        for field in ('canonical_answer', 'canonical_prompt', 'projected_answer', 'projected_prompt'):
            if type(value.get(field)) is not str or len(value[field].encode()) > 256 * 1024:
                raise Blocked('wear_bounded_synthetic_projection_unavailable')
        if value['canonical_prompt'] != case['prompt'] or value['projected_prompt'] != case['prompt']:
            raise Blocked('wear_terminal_prompt_mismatch')
        answer = value['canonical_answer']
        checks = self.common.validate(case, answer, [], [])
        # No native sources field exists in this projection. Unknown observation
        # must not be represented as an observed empty provider source list.
        for key in ('sources_equal', 'sources_present', 'official_source', 'page_read'):
            if key in checks:
                checks[key] = 'unverified'
        checks['canonical_projection_equal'] = 'pass' if value['projected_answer'] == answer and value.get('projection_equal') is True else 'fail'
        checks['canonical_ancestry'] = 'pass' if value.get('ancestry_valid') is True else 'fail'
        checks['canonical_execution'] = 'pass'
        checks['accessibility_visible_answer_equal'] = 'pass' if value.get('ui_visible_equal') is True and value.get('ui_visible_status') == 'pass' else 'unverified'
        checks['single_rendered_ui_row'] = 'unverified'
        return checks

    async def read_native_status(self, request):
        # The helper only exposes a categorical execution-scoped observation,
        # not provider Retry-After metadata. Never invent provider timing.
        return []

    async def clear_owned_draft(self, prompts):
        # The entire isolated store (including any uncertain draft) is retired
        # on close. No production draft is read or modified.
        return

    async def release_ownership(self):
        await self.mailbox.close()
        self.owned = False


async def run_wear(args, report, common):
    global Blocked
    Blocked = common.Blocked
    adb = common.ADB(args.adb, args.ssh, args.remote_adb)
    adb.device = select_watch(await asyncio.to_thread(adb.run, 'devices', '-l'))
    report['build'] = await asyncio.to_thread(build_identity, adb, args)
    # Starting instrumentation can kill the old app process before the facade
    # can inspect it. Refuse *all* existing processes, including recordings or
    # pending user work, rather than inferring idle from a new process snapshot.
    existing = await asyncio.to_thread(adb.run, 'shell', f'pidof {PACKAGE} || true')
    if existing.strip():
        raise Blocked('wear_existing_app_process_must_be_retired_before_instrumentation')
    report['preflight'] = 'target_process_absent_before_instrumentation'
    await asyncio.to_thread(adb.run, 'shell', 'input', 'keyevent', 'KEYCODE_WAKEUP')
    report['setup_action'] = 'verified_watch_wakeup_without_keyguard_dismissal'
    mailbox = Mailbox(adb, args.max_live_calls, args.turn_timeout, args.model)
    driver = NativeSuiteDriver(mailbox, common, report)
    report['limitations'] = [v for v in report['limitations'] if v not in (
        'Installed signing certificate not independently verified.',
        'Synthetic conversations are retained; no app data is deleted.')]
    report['limitations'] += ['Sources/page-read execution not exposed by the native Wear projection.',
        'Full accessibility answer equality and single rendered row may remain unverified.',
        'Disposable native model/Web configuration is not production settings-control acceptance.']
    report['cleanup']['native_session'] = 'not_started'
    try:
        await mailbox.start()
        report['build']['same_signer_instrumentation'] = 'verified_by_guarded_target_facade'
        report['cleanup']['native_session'] = 'pending'
        if getattr(args, 'wear_ui_probe', False):
            report.update(suite='wear-native-ui-probe', coverage='probe_only', selected_cases=[],
                          requested_cases=[], omitted_cases=[], selected_case_count=0, suite_case_count=0,
                          planned_admissions=0, admissions={'submitted':0, 'observed':0, 'budget':args.max_live_calls})
            report['measurement'] = {'boundary':'not_timed_no_send_ui_probe', 'clock':'not_applicable'}
            await mailbox.command('setup_case', web_enabled=False)
            probe_prompt = ' '.join([common.load_scenarios()[0]['prompt']] * 16)
            probe = await mailbox.command('probe', prompt=probe_prompt)
            if probe.get('status') != 'ready' or probe.get('ui_probe') is not True or probe.get('send_ready') is not True or probe.get('latest_restored') is not True:
                raise Blocked('wear_no_send_ui_probe_failed')
            report['ui_probe'] = 'pass'
            await mailbox.close()
            report['cleanup']['native_session'] = 'pass'
            report['cleanup']['preferences'] = 'not_modified_isolated_session'
            report['status'] = 'pass'
            return
        try:
            await common.run_suite(None, args, report, driver=driver)
        except common.Blocked:
            if not driver.rate_limited or not mailbox.cleaned:
                raise
            report['status'] = 'blocked'
            if report['cases']:
                report['cases'][-1].update(status='blocked', reason='rate_limited',
                    evidence='native_provider_status', provider_reset='unknown')
        report['cleanup']['native_session'] = 'pass'
        report['cleanup']['preferences'] = 'not_modified_isolated_session'
    finally:
        # Also covers failure before the common suite acquired ownership.
        if driver.owned or not report['cleanup']['native_session'] == 'pass':
            try:
                await mailbox.close()
                report['cleanup']['native_session'] = 'pass'
            except BaseException:
                report['cleanup']['native_session'] = 'fail'
                raise
