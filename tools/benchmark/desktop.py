"""Desktop transport through the existing qualified private-session controller."""
import asyncio
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import uuid
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]


def artifact_root(checkout):
    spec = importlib.util.spec_from_file_location('app_bench_paths',
        Path(__file__).resolve().parents[1] / 'app-bench/paths.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.artifact_root(checkout)


def load_common():
    spec = importlib.util.spec_from_file_location('typed_benchmark_common', Path(__file__).with_name('benchmark.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SeleniumUI:
    def __init__(self, driver):
        self.driver = driver

    async def evaluate(self, expression):
        try:
            result = self.driver.execute_async_script('''const done=arguments[arguments.length-1];
try { Promise.resolve((0,eval)(arguments[0])).then(
value=>done({ok:true,value}),()=>done({ok:false})); }
catch (_) { done({ok:false}); }''', expression)
        except Exception:
            raise RuntimeError('desktop_evaluation_failed_payload_omitted') from None
        if not isinstance(result, dict) or result.get('ok') is not True:
            raise RuntimeError('desktop_javascript_failed_payload_omitted')
        return result.get('value')


def execute_inside(args, runner, common=None):
    """Called only by runner.execute after its namespace/session gates."""
    common = common or load_common()
    report = {'schema': 1, 'suite': 'typed-model-matrix', 'platform': 'desktop',
              'status': 'fail', 'cases': [], 'cleanup': {'preferences': 'not_touched'},
              'foundation_qualification': 'Matching offline three-case typed workflow and safety only; not qualification of the shared six-case suite.'}
    driver = process = None
    try:
        runner.validate_benchmark_binary(args.binary)
        runner.validate_offline_evidence(args.offline_evidence, args.binary, {}, {}, 'typed')
        report['desktop_binary_sha256'] = hashlib.sha256(args.binary.read_bytes()).hexdigest()
        report['desktop_harness_sha256'] = runner.harness_identity()
        reservation = args.output.resolve() / 'reservation.json'
        reserved = runner.reserve_live_budget(runner.ASSIGNMENT_LEDGER, reservation)
        report['budget_debit'] = {'reserved': runner.LIVE_BATCH_ALLOWANCE, 'assignment_reserved': reserved}
        env = runner.isolated_environment(args.output.resolve(), args.config, None,
                                          True, reservation, entry='typed')
        driver, process, launch_ms = runner.start_typed_app(args, env)
        report['launch_to_typed_ready_observed_ms'] = launch_ms
        safe = SimpleNamespace(model='custom-luna-none', repetitions=1, turn_timeout=args.flow_timeout)
        asyncio.run(common.run_suite(SeleniumUI(driver), safe, report))
    except BaseException:
        report['status'] = 'fail'
        report['failure_category'] = 'desktop_shared_execution_failed_payload_omitted'
    finally:
        try:
            cleanup = runner.shutdown_owned(driver, process)
            report['cleanup']['desktop'] = cleanup
            if cleanup.get('app_exit_before_group_termination_observed') is not True:
                report['status'] = 'fail'
        except BaseException:
            report['cleanup']['desktop'] = {'status': 'fail'}
            report['status'] = 'fail'
        runner.private_json(args.output.resolve() / 'report.json', report)
        runner.prune_owned_reports(args.output.resolve().parent)
    return {'pass': 0, 'unverified': 2, 'blocked': 3}.get(report['status'], 1)


async def run_desktop(args, report, common=None):
    common = common or load_common()
    if (not args.live or args.model != 'custom-luna-none' or args.repetitions != 1 or
            not 10 <= args.turn_timeout <= 180 or not 7 <= args.max_live_calls <= 12):
        raise common.Blocked('desktop_fixed_live_profile_required')
    for name in ('binary', 'config', 'offline_evidence'):
        path = getattr(args, name, None)
        if path is None or not Path(path).is_file():
            raise common.Blocked('desktop_prerequisite_file_required')
    tools = getattr(args, 'tools', None)
    if (tools is None or not (Path(tools)/'venv/bin/python').is_file() or
            not (Path(tools)/'bin/tauri-driver').is_file() or
            not (Path(tools)/'webkit/extracted/usr/bin/WebKitWebDriver').is_file() or
            any(shutil.which(tool) is None for tool in ('bwrap', 'dbus-run-session'))):
        raise common.Blocked('desktop_existing_tools_required')
    try:
        parent = artifact_root(ROOT)
    except ValueError:
        raise common.Blocked('desktop_canonical_workspace_unavailable') from None
    if any(p.is_symlink() for p in (parent, *parent.parents)):
        raise common.Blocked('desktop_artifact_symlink_refused')
    parent.mkdir(mode=0o700, exist_ok=True)
    output = parent / ('typed-shared-' + uuid.uuid4().hex)
    command = [str(Path(tools).resolve()/'venv/bin/python'), str(ROOT/'tools/app-bench/runner.py'),
               '--binary', str(Path(args.binary).resolve()), '--tools', str(Path(tools).resolve()),
               '--config', str(Path(args.config)), '--output', str(output),
               '--entry', 'typed', '--live', '--offline-evidence', str(Path(args.offline_evidence).resolve()),
               '--flow-timeout', str(args.turn_timeout), '--shared-suite']
    process = subprocess.Popen(command, start_new_session=True, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL)
    timed_out = False
    try:
        await asyncio.to_thread(process.wait, timeout=7*(args.turn_timeout + 30) + 300)
    except BaseException:
        timed_out = True
        try:
            os.killpg(process.pid, signal.SIGTERM)
            await asyncio.to_thread(process.wait, timeout=10)
        except ProcessLookupError:
            pass
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            await asyncio.to_thread(process.wait, timeout=5)
    path = output/'report.json'
    if path.is_symlink() or not path.is_file() or path.stat().st_size > common.MAX_REPORT_BYTES:
        raise common.Blocked('desktop_bounded_child_report_unavailable')
    child = json.loads(path.read_bytes())
    if (child.get('schema') != 1 or child.get('suite') != 'typed-model-matrix' or
            child.get('platform') != 'desktop' or child.get('status') not in ('pass', 'unverified', 'fail', 'blocked')):
        raise common.Blocked('desktop_child_report_invalid')
    for key in ('status', 'cases', 'desktop_binary_sha256', 'desktop_harness_sha256',
                 'foundation_qualification', 'budget_debit', 'launch_to_typed_ready_observed_ms', 'failure_category',
                 'admissions', 'suite_deadline_seconds', 'coverage', 'requested_cases', 'selected_cases',
                 'omitted_cases', 'suite_case_count', 'selected_case_count', 'planned_admissions'):
        if key in child:
            report[key] = child[key]
    report['cleanup'].update(child.get('cleanup', {}))
    if timed_out or process.returncode not in (0, 2, 3) or child.get('cleanup', {}).get('desktop', {}).get('app_exit_before_group_termination_observed') is not True:
        report['status'] = 'fail'
        report['failure_category'] = 'desktop_child_failed_or_retirement_unverified'
