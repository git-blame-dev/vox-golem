"""Standard-library tests; all transports and UI boundaries are synthetic fakes."""
import asyncio
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch, MagicMock
from types import SimpleNamespace
import tempfile
import json
import io
import shutil
import subprocess
from contextlib import redirect_stdout, nullcontext, asynccontextmanager

SPEC = importlib.util.spec_from_file_location('typed_benchmark', Path(__file__).with_name('benchmark.py'))
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class SuiteTests(unittest.TestCase):
    def test_wear_cli_gates(self):
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / 'synthetic.apk'
            apk.write_bytes(b'synthetic-artifact')
            flags = ['--platform','wear','--device','watch7','--live','--allow-device',
                '--adb','synthetic-adb','--apk',str(apk),'--build-variant','performance']
            bench.authorize(bench.parser().parse_args(flags))
            for extra in (['--model','synthetic-model'], ['--device','a17'],
                    ['--build-variant','debug'], ['--build-variant','release'],
                    ['--repetitions','2'], ['--history-seed-turns','2'],
                    ['--collect-stages'], ['--max-live-calls','8'], ['--max-live-calls','6'],
                    ['--turn-timeout','9'], ['--turn-timeout','181'],
                    ['--apk',str(apk.parent)], ['--apk',str(apk.parent / 'missing.apk')],
                    ['--ssh','synthetic-host']):
                with self.subTest(extra=extra), self.assertRaises(bench.Blocked):
                    bench.authorize(bench.parser().parse_args(flags + extra))
            for missing in ('--live', '--allow-device'):
                with self.assertRaises(bench.Blocked):
                    bench.authorize(bench.parser().parse_args([f for f in flags if f != missing]))
            args = bench.parser().parse_args(flags + ['--cases','arithmetic','--max-live-calls','1'])
            bench.authorize(args)
            for key, value in (('adb', None), ('apk', None), ('turn_timeout', float('nan'))):
                invalid = SimpleNamespace(**vars(args))
                setattr(invalid, key, value)
                with self.assertRaises(bench.Blocked): bench.authorize(invalid)
            for size in (0, 256 * 1024 * 1024 + 1):
                with apk.open('wb') as stream: stream.truncate(size)
                with self.assertRaisesRegex(bench.Blocked, 'wear_bounded_local_apk_required'):
                    bench.authorize(args)

    def test_wear_dispatch_shares_common_module_and_measurement(self):
        async def run_wear(args, report, common):
            self.assertIs(common, bench)
            self.assertIs(common.Blocked, bench.Blocked)
            self.assertEqual(report['measurement']['clock'], 'device-elapsed-realtime')
            self.assertEqual(report['measurement']['transport_overhead'], 'excluded-from-device-timing')
            self.assertNotIn('Windows and Wear unsupported.', report['limitations'])
            report['status'] = 'unverified'
        loader = SimpleNamespace(exec_module=lambda module: setattr(module, 'run_wear', run_wear))
        spec = SimpleNamespace(name='synthetic_wear_adapter', origin=__file__, loader=loader)
        with tempfile.TemporaryDirectory() as directory:
            apk = Path(directory) / 'synthetic.apk'
            apk.write_bytes(b'synthetic-artifact')
            argv = ['benchmark','--platform','wear','--device','watch7','--live','--allow-device',
                '--adb','synthetic-adb','--apk',str(apk),'--build-variant','performance','--output',directory]
            with patch.object(bench.sys, 'argv', argv), \
                    patch.dict(bench.sys.modules, {bench.__name__:bench}), \
                    patch.object(bench.importlib.util, 'spec_from_file_location', return_value=spec), \
                    patch.object(bench.importlib.util, 'module_from_spec', return_value=SimpleNamespace()), \
                    patch.object(bench, 'source_identity', return_value={}), \
                    patch.object(bench, 'run_android', side_effect=AssertionError('phone dispatch')), \
                    redirect_stdout(io.StringIO()):
                self.assertEqual(bench.main(), 2)

    def test_wear_native_sources_participate_in_harness_identity(self):
        real_read = Path.read_bytes
        names = ('wear.py', 'TypedAskBenchmark.kt', 'TypedAskInstrumentation.java',
                 'AssistantActivity.kt', 'assistant-proguard-rules.pro', 'AndroidManifest.xml')
        baseline = bench.source_identity()['harness_sha256']
        for name in names:
            def changed(path, name=name):
                return b'synthetic-source-change' if path.name == name else real_read(path)
            # Include forthcoming files even when the parent's implementation has
            # not reached disk yet; never create or alter its owned files.
            real_is_file = Path.is_file
            def exists(path): return path.name == name or real_is_file(path)
            with self.subTest(name=name), patch.object(Path, 'read_bytes', changed), patch.object(Path, 'is_file', exists):
                self.assertNotEqual(bench.source_identity()['harness_sha256'], baseline)

    def test_wear_list_has_no_transport(self):
        with patch.object(bench.sys, 'argv', ['benchmark','--platform','wear','--list','--cases','mixed','page_read']), \
                patch.object(bench.subprocess, 'run', side_effect=AssertionError('transport')), \
                redirect_stdout(io.StringIO()) as output:
            self.assertEqual(bench.main(), 0)
        self.assertEqual(output.getvalue().splitlines(), ['page_read web=on','mixed web=on'])

    def test_semantic_driver_scenarios_budget_and_cleanup(self):
        class Driver:
            def __init__(self):
                self.preferences = dict(instant='synthetic-original', web_enabled=True,
                    tts_enabled=True, prefetch=False, completion=False)
                self.prompts, self.cleared, self.released = [], None, False
            async def snapshot_preferences(self): return dict(self.preferences)
            async def begin_ownership(self): pass
            async def ownership(self, action='check', request=None): return 'idle'
            async def set_tts(self, value): self.preferences['tts_enabled'] = value
            async def set_web(self, value): self.preferences['web_enabled'] = value
            async def select_model(self, value): self.preferences['instant'] = value
            async def confirm_configuration(self, model, web):
                self_test.assertEqual(self.preferences['instant'], model)
                self_test.assertIs(self.preferences['web_enabled'], web)
            async def new_chat(self): pass
            async def terminal_validation(self, case, request, metrics):
                return {'answer':'pass'}
            async def clear_owned_draft(self, prompts): self.cleared = prompts
            async def release_ownership(self): self.released = True
            async def read_native_status(self, request): return []
            async def exercise(self, model, prompt, **kwargs):
                await kwargs['confirm_ready']()
                await kwargs['submission_observer']()
                self.prompts.append(prompt)
                request = f'synthetic-{len(self.prompts)}'
                await kwargs['admission_observer'](request)
                metrics = dict(first_answer_ms=1., completed_ms=2.)
                await kwargs['terminal_observer'](request, metrics)
                return metrics
        self_test = self
        for budget in (3, 2):
            driver = Driver()
            original = dict(driver.preferences)
            args = bench.parser().parse_args(['--platform','android','--cases','history','arithmetic',
                '--max-live-calls',str(budget)])
            report = {'cases':[], 'cleanup':{}}
            if budget == 3:
                asyncio.run(bench.run_suite(None, args, report, driver=driver))
                cases = bench.load_scenarios()
                self.assertEqual(driver.prompts, [cases[0]['prompt'], cases[5]['seed'], cases[5]['prompt']])
                self.assertEqual(report['admissions'], {'submitted':3, 'observed':3, 'budget':3})
            else:
                with self.assertRaisesRegex(bench.Blocked, 'ui_admission_budget_exhausted'):
                    asyncio.run(bench.run_suite(None, args, report, driver=driver))
                self.assertEqual(len(driver.prompts), 2)
            self.assertEqual(driver.preferences, original)
            self.assertTrue(driver.released)
            self.assertIn(bench.load_scenarios()[5]['seed'], driver.cleared)
            self.assertEqual(report['cleanup']['preferences'], 'pass')

        # Native provider evidence uses the same shared blocker classifier and
        # stops before mixed; integrity failures must not be hidden as blockage.
        for integrity in ('pass', 'fail'):
            driver = Driver()
            async def status(request):
                return [{'provider':'Exa', 'provider_retry_after_seconds':12,
                    'local_cooldown_seconds':2, 'provenance':'native_provider_status',
                    'provider_reset':'minimum_retry_delay'}]
            async def validation(case, request, metrics):
                metrics['source_counts'] = {'ui':0, 'canonical':0}
                return dict(answer='pass', sources_present='fail', official_source='fail',
                    sources_equal='pass', canonical_answer_equal=integrity,
                    canonical_execution_and_single_ui_row='pass', page_read='unverified')
            driver.read_native_status, driver.terminal_validation = status, validation
            args = bench.parser().parse_args(['--platform','android','--cases','page_read','mixed'])
            report = {'cases':[], 'cleanup':{}}
            if integrity == 'pass':
                asyncio.run(bench.run_suite(None, args, report, driver=driver))
                self.assertEqual(report['status'], 'blocked')
                self.assertEqual(report['cases'][0]['evidence'], 'native_provider_status')
            else:
                with self.assertRaisesRegex(bench.Blocked, 'scenario_validation_failed'):
                    asyncio.run(bench.run_suite(None, args, report, driver=driver))
                self.assertNotEqual(report['cases'][0]['status'], 'blocked')
            self.assertEqual(len(driver.prompts), 1)
            self.assertTrue(driver.released)
            self.assertEqual(report['cleanup']['preferences'], 'pass')

    def test_semantic_native_status_invalid_or_missing_is_unverified(self):
        for records in ([], [{'unsafe':'synthetic'}]):
            row, terminal_calls = {}, []
            async def read(request): return records
            async def terminal(request, metrics): terminal_calls.append(request)
            async def exercise(model, prompt, **kwargs):
                await kwargs['terminal_observer']('synthetic-request', {})
            driver = SimpleNamespace(exercise=exercise, read_native_status=read)
            asyncio.run(bench.exercise_semantic_with_native_status(driver, 'synthetic-model',
                'synthetic-prompt', row=row, web_enabled=True, terminal_observer=terminal))
            self.assertEqual(row['native_rate_limit_observation'], 'unverified')
            self.assertEqual(row['native_rate_limits'], [])
            self.assertEqual(terminal_calls, ['synthetic-request'])

    def test_android_native_focus_setup(self):
        class UI:
            def __init__(self, focused=False, safe=True, tab_works=True):
                self.focused, self.safe, self.tab_works = focused, safe, tab_works
            async def evaluate(self, script):
                if 'get_app_snapshot' in script:
                    return self.safe
                return self.focused

        class ADB:
            def __init__(self, ui, policy='showing=false inputRestricted=false', foreign=False):
                self.ui, self.policy, self.foreign, self.calls = ui, policy, foreign, []
            def run(self, *args):
                self.calls.append(args)
                if args == ('shell', 'dumpsys', 'window', 'policy'):
                    return self.policy
                if args == ('shell', 'dumpsys', 'window'):
                    return 'mCurrentFocus=Window{abc u0 ' + ('synthetic.foreign' if self.foreign else bench.PACKAGE) + '/.MainActivity}'
                if args == ('shell', 'input', 'keyevent', 'KEYCODE_TAB'):
                    self.ui.focused = self.ui.tab_works
                    return ''
                raise AssertionError(args)

        for focused, safe, policy, foreign, works, passes, tabs in (
                (True, False, '', True, True, True, 0),
                (False, True, 'showing=false inputRestricted=false', False, True, True, 1),
                (False, True, 'showing=true inputRestricted=false', False, True, False, 0),
                (False, True, 'showing=false inputRestricted=true', False, True, False, 0),
                (False, True, 'showing=false', False, True, False, 0),
                (False, True, '', False, True, False, 0),
                (False, True, 'showing=false inputRestricted=false', True, True, False, 0),
                (False, False, 'showing=false inputRestricted=false', False, True, False, 0),
                (False, True, 'showing=false inputRestricted=false', False, False, False, 1)):
            with self.subTest(focused=focused, safe=safe, policy=policy, foreign=foreign, works=works):
                ui = UI(focused, safe, works)
                adb, report = ADB(ui, policy, foreign), {}
                async def immediate_wait(predicate, timeout):
                    self.assertEqual(timeout, 3)
                    if not await predicate():
                        raise bench.Blocked('ui_transition_deadline')
                with patch.object(bench, 'wait_condition', immediate_wait):
                    if passes:
                        asyncio.run(bench.ensure_android_webview_focus(ui, adb, report))
                    else:
                        with self.assertRaisesRegex(bench.Blocked, 'webview_not_visible_focused'):
                            asyncio.run(bench.ensure_android_webview_focus(ui, adb, report))
                self.assertEqual(adb.calls.count(('shell', 'input', 'keyevent', 'KEYCODE_TAB')), tabs)
                self.assertEqual(report, {'setup_action':'native_keyboard_tab'} if tabs else {})
                if focused:
                    self.assertEqual(adb.calls, [])

    def test_history_depth_budget_and_desktop_stage_rejection(self):
        args = bench.parser().parse_args(['--platform','android','--live','--allow-device','--device','a17',
            '--adb','synthetic-adb','--cases','history','--history-seed-turns','8','--max-live-calls','9'])
        bench.authorize(args)
        report = {}
        bench.select_scenarios(args, report)
        self.assertEqual(report['planned_admissions'], 9)
        args.max_live_calls = 8
        with self.assertRaises(bench.Blocked): bench.authorize(args)
        args.platform, args.cases, args.collect_stages = 'desktop', None, True
        with self.assertRaisesRegex(bench.Blocked, 'desktop_stage_collection_unsupported'):
            bench.authorize(args)

    def test_selection_canonical_order_coverage_and_duplicates(self):
        args = bench.parser().parse_args(['--platform','android','--cases','mixed','page_read'])
        report = {}
        self.assertEqual([c['id'] for c in bench.select_scenarios(args, report)], ['page_read','mixed'])
        self.assertEqual(report['requested_cases'], ['mixed','page_read'])
        self.assertEqual(report['omitted_cases'], ['arithmetic','coding','lookup','history'])
        self.assertEqual(report['coverage'], 'partial')
        self.assertEqual(report['planned_admissions'], 2)
        for cases in (['mixed','mixed'], ['unknown'], []):
            args.cases = cases
            with self.assertRaises(bench.Blocked):
                bench.select_scenarios(args)
        args.cases = None
        bench.select_scenarios(args, report)
        self.assertEqual(report['coverage'], 'full')
        self.assertEqual(report['planned_admissions'], 7)
        self.assertEqual(report['omitted_cases'], [])

    def test_model_rate_limit_never_hides_wrong_answers_or_integrity_failures(self):
        case = bench.load_scenarios()[3]
        result = {'answer':'The tool is rate-limited: HTTP 429.', 'sources':[], 'canonical_sources':[]}
        checks = bench.validate(case, result['answer'], [], [])
        checks.update(canonical_answer_equal='pass', canonical_execution_and_single_ui_row='pass')
        self.assertTrue(bench.model_reported_rate_limit(case, result, checks))
        for text in ('An ordinary source-free answer.', 'The tool is not rate limited.',
                     "The tool isn't rate-limited.", 'No HTTP 429 occurred.', 'HTTP 429 was not returned.',
                     'Rate limits are a general possibility.'):
            self.assertFalse(bench.model_reported_rate_limit(case, {**result,'answer':text}, checks))
        for key in ('answer','sources_equal','canonical_answer_equal','canonical_execution_and_single_ui_row'):
            self.assertFalse(bench.model_reported_rate_limit(case, result, {**checks,key:'fail'}))
        self.assertFalse(bench.model_reported_rate_limit(bench.load_scenarios()[0], result, checks))
        self.assertFalse(bench.model_reported_rate_limit(case, {**result,'sources':['https://synthetic.invalid']}, checks))

    def test_selected_admission_budget_includes_only_selected_history_seed(self):
        args = bench.parser().parse_args(['--platform','android','--live','--allow-device','--device','a17','--adb','synthetic-adb'])
        args.cases, args.max_live_calls = ['arithmetic'], 1
        bench.authorize(args)
        args.cases, args.max_live_calls = ['history'], 1
        with self.assertRaises(bench.Blocked):
            bench.authorize(args)
        args.max_live_calls = 2
        bench.authorize(args)

    def test_blocked_subset_partial_report_and_exit_three(self):
        async def blocked(args, report):
            report['status'] = 'blocked'
            report['cases'] = [{'scenario':'page_read', 'status':'blocked', 'workflow_completed':True,
                                'reason':'rate_limited', 'evidence':'model_reported',
                                'provenance':'model_reported_rate_limit', 'provider_reset':'unknown',
                                'retry_after_seconds':None, 'first_answer_ms':1., 'completed_ms':2.}]
        with tempfile.TemporaryDirectory() as directory:
            argv = ['benchmark','--platform','android','--live','--allow-device','--device','a17',
                    '--adb','synthetic-adb','--cases','mixed','page_read','--output',directory]
            with patch.object(bench.sys, 'argv', argv), patch.object(bench, 'run_android', blocked), redirect_stdout(io.StringIO()) as output:
                self.assertEqual(bench.main(), 3)
            summary = json.loads(output.getvalue())
            self.assertEqual(summary['blocked_count'], 1)
            self.assertEqual(summary['completion_count'], 1)
            report = json.loads(Path(summary['report_path']).read_text())
            self.assertEqual(report['selected_cases'], ['page_read','mixed'])
            self.assertEqual(report['coverage'], 'partial')
            self.assertEqual(report['selected_case_count'], 2)
            self.assertEqual(len(report['cases']), 1)

    def test_selected_list_is_transport_free(self):
        with patch.object(bench.sys, 'argv', ['benchmark','--platform','android','--list','--cases','mixed','page_read']), \
                patch.object(bench.subprocess, 'run', side_effect=AssertionError('unexpected transport')), \
                redirect_stdout(io.StringIO()) as output:
            self.assertEqual(bench.main(), 0)
        self.assertEqual(output.getvalue().splitlines(), ['page_read web=on','mixed web=on'])

    def test_desktop_rejects_partial_selection(self):
        args = bench.parser().parse_args(['--platform','desktop','--live','--binary','/synthetic/app',
            '--tools','/synthetic/tools','--config','/synthetic/config','--offline-evidence','/synthetic/evidence'])
        args.cases = ['page_read','mixed']
        with self.assertRaises(bench.Blocked):
            bench.authorize(args)
    def test_android_focus_uses_complete_window_dump(self):
        calls = []
        def run(*args):
            calls.append(args)
            if args == ('devices', '-l'):
                return 'synthetic device model:SM_A176B'
            if args == ('shell', 'pidof', bench.PACKAGE):
                return '123'
            if args == ('shell', 'dumpsys', 'window'):
                return 'mCurrentFocus=Window{synthetic ' + bench.PACKAGE + '/.MainActivity}'
            return ''
        adb = SimpleNamespace(run=run)
        args = SimpleNamespace(adb='synthetic-adb', ssh=None, remote_adb=None, port=9223)
        with patch.dict(bench.sys.modules, {'websockets':SimpleNamespace()}), \
                patch.object(bench, 'ADB', return_value=adb), \
                patch.object(bench, 'build_identity', return_value={}), \
                patch.object(bench, 'owned_forward', side_effect=bench.Blocked('synthetic_focus_passed')):
            with self.assertRaisesRegex(bench.Blocked, '^synthetic_focus_passed$'):
                asyncio.run(bench.run_android(args, {'cleanup':{}}))
        self.assertIn(('shell', 'dumpsys', 'window'), calls)

    def test_strict_scenarios_reject_unknown_policies_and_oversized_input(self):
        original = bench.load_scenarios()
        mutations = [lambda c:c[0].update(extra='unsafe'),
                     lambda c:c[1].update(id='arithmetic'),
                     lambda c:c[4].update(official_hosts=['github.com']),
                     lambda c:c[3].update(requires_page_read=1),
                     lambda c:c[5].update(seed='x'*1025),
                     lambda c:c[0].update(prompt='synthetic\ncontrol'),
                     lambda c:c[0].update(seed='unexpected history')]
        raws = []
        for mutate in mutations:
            cases = json.loads(json.dumps(original))
            mutate(cases)
            raws.append(json.dumps(cases).encode())
        raws += [b'x'*32769, b'[{"id":"arithmetic","id":"coding"}]', b'{bad json']
        for raw in raws:
            with self.subTest(size=len(raw)), patch.object(bench.Path, 'open', return_value=io.BytesIO(raw)):
                with self.assertRaises(bench.Blocked):
                    bench.load_scenarios()

    def test_malformed_scenarios_block_before_transport(self):
        with tempfile.TemporaryDirectory() as directory:
            argv = ['benchmark', '--platform', 'android', '--allow-device', '--live', '--device', 'a17',
                    '--adb', '/synthetic/adb', '--output', directory]
            with patch.object(bench.sys, 'argv', argv), patch.object(bench, 'load_scenarios', side_effect=bench.Blocked('scenario_contract_invalid')), \
                    patch.object(bench, 'run_android') as transport, redirect_stdout(io.StringIO()):
                self.assertEqual(bench.main(), 1)
            transport.assert_not_called()

    def test_desktop_common_authorization_does_not_admit_physical_flags(self):
        flags = ['--platform','desktop','--live','--binary','/synthetic/app', '--tools','/synthetic/tools',
                 '--config','/synthetic/config','--offline-evidence','/synthetic/evidence']
        bench.authorize(bench.parser().parse_args(flags))
        for extra in (['--allow-device'], ['--device','a17'], ['--model','custom-sol-high'], ['--repetitions','2'], ['--turn-timeout','9']):
            with self.assertRaises(bench.Blocked):
                bench.authorize(bench.parser().parse_args(flags + extra))

    def test_metrics_reject_unbounded_nonmonotonic_and_non_numeric_values(self):
        for metrics in ({'first_answer_ms':float('nan'), 'completed_ms':2},
                        {'first_answer_ms':3, 'completed_ms':2},
                        {'first_answer_ms':True, 'completed_ms':2},
                        {'first_answer_ms':1, 'completed_ms':999999999}):
            with self.assertRaises(bench.Blocked):
                bench.checked_metrics(metrics)
    def test_strict_scenarios_reject_numeric_boolean(self):
        cases = bench.load_scenarios()
        cases[0]['web_enabled'] = 0
        with patch.object(bench.json, 'loads', return_value=cases):
            with self.assertRaises(bench.Blocked):
                bench.load_scenarios()

    def test_mixed_accepts_only_specific_official_async_std_paths(self):
        case = bench.load_scenarios()[4]
        for official in ('https://docs.rs/async-std/latest/async_std/', 'https://github.com/async-rs/async-std/blob/main/README.md'):
            sources = ['https://tokio.rs/synthetic', official]
            self.assertEqual(bench.validate(case, 'Synthetic comparison', sources, sources)['official_source'], 'pass')
        sources = ['https://tokio.rs/synthetic', 'https://github.com/synthetic-user/async-std']
        self.assertEqual(bench.validate(case, 'Synthetic comparison', sources, sources)['official_source'], 'fail')
        sources = ['https://docs.rs/tokio/latest/tokio/', 'https://github.com/async-rs/async-std']
        self.assertEqual(bench.validate(case, 'Synthetic comparison', sources, sources)['official_source'], 'pass')
        for unsafe in ('https://github.com/async-rs/async-std-other',
                       'https://github.com/async-rs/async-std/../../synthetic',
                       'https://github.com/async-rs/async-std/%2e%2e/synthetic',
                       'http://docs.rs/async-std'):
            sources = ['https://tokio.rs/synthetic', unsafe]
            self.assertEqual(bench.validate(case, 'Synthetic comparison', sources, sources)['official_source'], 'fail')

    def test_unverified_has_distinct_exit_and_report_path(self):
        async def unverified(args, report):
            report['status'] = 'unverified'
            report['cases'] = [{'status':'unverified', 'workflow_completed':True}]
        with tempfile.TemporaryDirectory() as directory:
            argv = ['benchmark', '--platform', 'android', '--allow-device', '--live', '--device', 'a17',
                    '--adb', '/synthetic/adb', '--model', 'synthetic-model', '--output', directory]
            with patch.object(bench.sys, 'argv', argv), patch.object(bench, 'run_android', unverified), redirect_stdout(io.StringIO()) as output:
                self.assertEqual(bench.main(), 2)
            summary = json.loads(output.getvalue())
            self.assertEqual(summary['completion_count'], 1)
            self.assertTrue(Path(summary['report_path']).is_file())

    def test_unwritable_report_location_blocks_before_transport(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'not-directory'
            path.write_text('synthetic')
            argv = ['benchmark', '--platform', 'android', '--allow-device', '--live', '--device', 'a17',
                    '--adb', '/synthetic/adb', '--model', 'synthetic-model', '--output', str(path)]
            with patch.object(bench.sys, 'argv', argv), patch.object(bench, 'run_android') as transport, redirect_stdout(io.StringIO()):
                self.assertEqual(bench.main(), 1)
            transport.assert_not_called()

    def test_scenarios_and_history(self):
        cases = bench.load_scenarios()
        self.assertEqual([c['id'] for c in cases], ['arithmetic', 'coding', 'lookup', 'page_read', 'mixed', 'history'])
        self.assertTrue(cases[1]['web_enabled'])
        self.assertFalse(cases[-1]['web_enabled'])
        self.assertEqual(cases[-1]['expected'], 'SYNTHETIC-ORBIT-42')

    def test_exact_and_structural_answers(self):
        cases = bench.load_scenarios()
        self.assertEqual(bench.validate(cases[0], '210', [], [])['answer'], 'pass')
        self.assertEqual(bench.validate(cases[0], '210 extra', [], [])['answer'], 'fail')
        self.assertEqual(bench.validate(cases[1], 'fn sum(xs: &[i32]) -> i32 { xs.iter().sum() }', [], [])['answer'], 'pass')
        self.assertEqual(bench.validate(cases[1], 'Rust can sum numbers.', [], [])['answer'], 'fail')

    def test_sources_do_not_prove_page_read(self):
        case = bench.load_scenarios()[3]
        sources = ['https://www.federalreserve.gov/synthetic']
        checks = bench.validate(case, 'Synthetic answer', sources, sources)
        self.assertEqual(checks['official_source'], 'pass')
        self.assertEqual(checks['page_read'], 'unverified')
        self.assertEqual(bench.validate(case, 'Synthetic answer', sources, [])['sources_equal'], 'fail')
        self.assertEqual(bench.validate(case, 'Synthetic answer', ['https://federalreserve.gov.invalid/'], ['https://federalreserve.gov.invalid/'])['official_source'], 'fail')

    def test_device_selection_ignores_other_models(self):
        listing = 'List of devices attached\nother device model:Pixel_8\nsynthetic device model:SM_A176B\n'
        self.assertEqual(bench.select_a17(listing), 'synthetic')
        for listing in ('other device model:Pixel_8', 'a device model:SM_A176B\nb device model:SM_A176B'):
            with self.assertRaises(bench.Blocked):
                bench.select_a17(listing)

    def test_no_live_authorization_without_both_flags(self):
        for flags in ([], ['--live'], ['--allow-device']):
            args = bench.parser().parse_args(['--platform', 'android', '--device', 'a17', *flags])
            with self.assertRaises(bench.Blocked):
                bench.authorize(args)

    def test_remote_command_is_argv_not_local_shell(self):
        transport = bench.ADB('/synthetic/adb', 'synthetic-host', '/remote/adb path')
        argv = transport.argv('devices', '-l')
        self.assertEqual(argv[:7], ['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', '--', 'synthetic-host'])
        self.assertEqual(argv[-1], "'/remote/adb path' devices -l")

    def test_no_rebind_ownership_on_failure_and_conflict(self):
        calls, cleanup = [], {}
        def run(*args):
            calls.append(args)
            return 'synthetic tcp:9223 localabstract:webview_devtools_remote_123' if args == ('forward', '--list') else ''
        adb = SimpleNamespace(run=run, device='synthetic')
        with self.assertRaisesRegex(bench.Blocked, 'synthetic'):
            with bench.owned_forward(adb, 9223, '123', cleanup):
                raise bench.Blocked('synthetic')
        self.assertIn('--no-rebind', calls[0])
        self.assertEqual(calls[-1], ('forward', '--remove', 'tcp:9223'))
        self.assertEqual(cleanup, {'forward':'pass', 'forward_acquired':True})
        calls.clear()
        def conflict(*args):
            calls.append(args)
            raise bench.Blocked('conflict')
        with self.assertRaises(bench.Blocked):
            with bench.owned_forward(SimpleNamespace(run=conflict), 9223, '123', {}):
                self.fail('must not acquire conflicting mapping')
        self.assertEqual(len(calls), 1)

    def test_replaced_forward_is_not_removed(self):
        calls = []
        def run(*args):
            calls.append(args)
            return 'other tcp:9223 localabstract:other'
        cleanup = {}
        with self.assertRaises(bench.Blocked):
            with bench.owned_forward(SimpleNamespace(run=run, device='synthetic'), 9223, '123', cleanup):
                pass
        self.assertFalse(any('--remove' in c for c in calls))
        self.assertEqual(cleanup['forward'], 'fail')

    def test_owned_ssh_tunnel_is_loopback_and_closed_after_failure(self):
        process = MagicMock()
        process.poll.return_value = None
        cleanup = {}
        with patch.object(bench.socket, 'socket'), patch.object(bench.socket, 'create_connection'), \
                patch.object(bench.subprocess, 'Popen', return_value=process) as start:
            with self.assertRaisesRegex(bench.Blocked, 'synthetic'):
                with bench.owned_tunnel('synthetic-host', 9223, cleanup):
                    raise bench.Blocked('synthetic')
        self.assertIn('127.0.0.1:9223:127.0.0.1:9223', start.call_args.args[0])
        self.assertNotIn('shell', start.call_args.kwargs)
        process.terminate.assert_called_once()
        process.wait.assert_called_once_with(timeout=5)
        self.assertEqual(cleanup['tunnel'], 'pass')

    def test_installed_build_identity_and_wrong_model_refusal(self):
        calls = []
        def run(*args):
            calls.append(args)
            return {('shell','getprop','ro.product.model'):'SM-A176B',
                    ('shell','getprop','ro.build.version.sdk'):'35',
                    ('shell','getprop','ro.product.cpu.abi'):'arm64-v8a',
                    ('shell','pm','path',bench.PACKAGE):'package:/data/app/~~synthetic==/synthetic/base.apk',
                    ('shell','sha256sum','/data/app/~~synthetic==/synthetic/base.apk'):'a'*64 + ' base.apk',
                    ('shell','dumpsys','package',bench.PACKAGE):'versionCode=123 flags=[ DEBUGGABLE HAS_CODE ]'}[args]
        args = SimpleNamespace(apk=None)
        result = bench.build_identity(SimpleNamespace(run=run), args)
        self.assertTrue(result['debuggable'])
        self.assertEqual(result['version_code'], 123)
        self.assertNotIn('synthetic/base', json.dumps(result))
        args.build_variant = 'release'
        with self.assertRaisesRegex(bench.Blocked, 'debuggability_mismatch'):
            bench.build_identity(SimpleNamespace(run=run), args)
        args.build_variant = 'debug'
        self.assertTrue(bench.build_identity(SimpleNamespace(run=run), args)['debuggable'])
        def old_api(*args):
            return '34' if args == ('shell','getprop','ro.build.version.sdk') else run(*args)
        with self.assertRaises(bench.Blocked):
            bench.build_identity(SimpleNamespace(run=old_api), args)
        with self.assertRaises(bench.Blocked):
            bench.build_identity(SimpleNamespace(run=lambda *args:'wrong'), args)

    def test_list_has_zero_transport_activity(self):
        with patch.object(bench.sys, 'argv', ['benchmark', '--platform', 'android', '--list']), \
                patch.object(bench.subprocess, 'run', side_effect=AssertionError('unexpected transport')), \
                redirect_stdout(io.StringIO()) as output:
            self.assertEqual(bench.main(), 0)
        self.assertIn('history web=off', output.getvalue())

    def test_partial_failure_report_is_private_and_payload_free(self):
        async def fail(args, report):
            report['cleanup']['forward'] = 'pass'
            raise ValueError('synthetic private provider payload')
        with tempfile.TemporaryDirectory() as directory:
            argv = ['benchmark', '--platform', 'android', '--allow-device', '--live', '--device', 'a17',
                    '--adb', '/synthetic/adb', '--model', 'synthetic-model', '--output', directory]
            with patch.object(bench.sys, 'argv', argv), patch.object(bench, 'run_android', fail), \
                    redirect_stdout(io.StringIO()) as output:
                self.assertEqual(bench.main(), 1)
            files = list(Path(directory).glob('*.json'))
            self.assertEqual(len(files), 1)
            self.assertEqual(files[0].stat().st_mode & 0o777, 0o600)
            report = json.loads(files[0].read_text())
            self.assertEqual(report['cleanup']['forward'], 'pass')
            self.assertNotIn('private provider', files[0].read_text() + output.getvalue())

    def test_report_rejects_repository_and_symlink(self):
        with self.assertRaises(bench.Blocked):
            bench.write_report(bench.ROOT / 'synthetic-results', {})
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'alias'
            path.symlink_to(directory)
            with self.assertRaises(bench.Blocked):
                bench.write_report(path, {})


class ControllerTests(unittest.IsolatedAsyncioTestCase):
    async def test_requested_stage_collection_downgrades_ui_pass_without_storage(self):
        session = '00000000-0000-0000-0000-000000000001'
        name = f'assistant-timing-123-{session}-1.jsonl'
        base = dict(schema=1, session=session, run=1, operation='GENERATION',
                    observedNanos=1_000_000_000, dropped=0)
        # Legacy coarse marks remain useful, but cannot certify the new DB/queue boundaries.
        events = [dict(base, kind='BEGIN', elapsedNanos=0),
                  dict(base, kind='MARK', stage='REQUESTED', elapsedNanos=0),
                  dict(base, kind='METADATA', model='custom-luna-none', webEnabled=False),
                  dict(base, kind='MARK', stage='ADMISSION_STARTED', elapsedNanos=1_000_000),
                  dict(base, kind='END', stage='SETTLED', elapsedNanos=2_000_000)]
        class ADB:
            raw = ''
            def run(self, *args):
                if args == ('devices','-l'): return 'synthetic device model:SM_A176B'
                if args == ('shell','pidof',bench.PACKAGE): return '123'
                if args == ('shell','dumpsys','window'): return 'mCurrentFocus=' + bench.PACKAGE
                if args == ('shell','cat','/proc/uptime'): return '1.00 2.00'
                if 'wc -c' in args[-1]: return f'{name} {len(self.raw.encode())}' if self.raw else ''
                if 'head -c' in args[-1]: return self.raw + 'JOURNAL_END'
                return ''
        adb = ADB()
        class UI:
            async def evaluate(self, expression): return True
        @asynccontextmanager
        async def connect(*args, **kwargs): yield object()
        async def suite(ui, args, report, stages):
            row = dict(scenario='arithmetic', status='pass', workflow_completed=True,
                       first_answer_ms=1, completed_ms=2)
            report['cases'].append(row)
            turn = stages.before(row, args.model, False)
            adb.raw = ''.join(json.dumps(e) + '\n' for e in events)
            stages.after(turn)
            report['status'] = 'pass'
        response = io.BytesIO(json.dumps([dict(type='page', url='http://tauri.localhost',
            webSocketDebuggerUrl='ws://localhost/devtools/page/synthetic')]).encode())
        args = bench.parser().parse_args(['--platform','android','--adb','synthetic-adb','--collect-stages'])
        report = {'cleanup':{}, 'cases':[]}
        with patch.dict(bench.sys.modules, {'websockets':SimpleNamespace(connect=connect)}), \
                patch.object(bench, 'ADB', return_value=adb), patch.object(bench, 'build_identity', return_value={}), \
                patch.object(bench, 'owned_forward', return_value=nullcontext()), \
                patch.object(bench, 'owned_tunnel', return_value=nullcontext()), \
                patch.object(bench.urllib.request, 'urlopen', return_value=response), \
                patch.object(bench, 'smoke_module', return_value=SimpleNamespace(CDP=lambda *a:UI())), \
                patch.object(bench, 'run_suite', suite):
            await bench.run_android(args, report)
        self.assertEqual(report['cases'][0]['status'], 'pass')
        self.assertEqual(report['cases'][0]['local_stages']['correlation'], 'verified')
        self.assertEqual(report['cases'][0]['local_stages']['android_intervals_ms']['admission_bookkeeping'], 1)
        self.assertEqual(report['stage_collection']['status'], 'unverified')
        self.assertEqual(report['status'], 'unverified')

    """Fake evaluate and settings boundaries, not real Tauri/WebView acceptance."""
    async def test_correlated_source_free_research_reply_can_report_rate_limit(self):
        class UI:
            async def evaluate(self, expression):
                return {'execution_valid':True, 'answer':'The page tool is rate-limited: HTTP 429.',
                        'canonical':'The page tool is rate-limited: HTTP 429.', 'sources':[], 'canonical_sources':[]}
        metrics = {}
        checks = await bench.terminal_validation(UI(), bench.load_scenarios()[3], 'synthetic-request', metrics)
        self.assertEqual(checks['sources_present'], 'fail')
        self.assertIs(metrics.get('model_reported_rate_limit'), True)

    async def test_settings_mount_is_observed_before_toggle(self):
        events = []
        snapshots = iter([{'web_enabled':False}, {'web_enabled':True}])
        async def snapshot(ui):
            return next(snapshots)
        async def click(ui, label):
            events.append(label)
        class UI:
            async def evaluate(self, expression):
                if 'e.checked' in expression:
                    events.append('toggle')
                    return True
                events.append('mounted')
                return events.count('mounted') > 1
        with patch.object(bench, 'snapshot_preferences', snapshot), patch.object(bench, 'click', click):
            await bench.set_web(UI(), True)
        self.assertEqual(events, ['Settings', 'mounted', 'mounted', 'toggle', 'Close settings'])

    async def run_controller(self, fail=False, foreign=False, invalid_answer=False,
                             active_failure=False, before_admission=False, cases=None, blocked_reply=False,
                             history_seed_turns=1, stages=None):
        original = dict(instant='synthetic-original', web_enabled=False, tts_enabled=True,
                        prefetch=False, completion=False)
        prefs = dict(original)
        calls = []
        class UI:
            async def evaluate(self, expression):
                if expression == bench.NATIVE_READ_JS:
                    return []
                if '__voxBenchmarkRateStatus' in expression:
                    return True
                return True if 'promptComposer' in expression else False
        async def snapshot(ui):
            return dict(prefs)
        async def web(ui, enabled):
            prefs['web_enabled'] = enabled
            calls.append(('web', enabled))
        async def tts(ui, enabled):
            prefs['tts_enabled'] = enabled
            calls.append(('tts', enabled))
        async def model(ui, value):
            prefs['instant'] = value
            calls.append(('model', value))
        async def begin(ui):
            pass
        changed = False
        active = False
        async def owner(ui, action='check', request=None):
            nonlocal active
            calls.append(('ownership', action))
            if changed:
                return 'foreign'
            if action == 'cancel':
                active = False
                return 'owned_active'
            return 'owned_active' if active else 'idle'
        async def new(ui):
            calls.append(('new',))
        async def validation(ui, case, request, metrics):
            calls.append(('validated', case['id']))
            if blocked_reply and case['id'] == 'page_read':
                native_mode = isinstance(blocked_reply, str) and blocked_reply.startswith('native')
                result = {'answer':('Synthetic unavailable page.' if native_mode and blocked_reply != 'native_with_prose' else
                                    'The page tool is rate-limited: HTTP 429.'), 'sources':[], 'canonical_sources':[]}
                if blocked_reply == 'native_success':
                    result['sources'] = result['canonical_sources'] = ['https://www.federalreserve.gov/synthetic']
                elif blocked_reply == 'native_source_mismatch':
                    result['canonical_sources'] = ['https://www.federalreserve.gov/synthetic']
                checks = bench.validate(case, result['answer'], result['sources'], result['canonical_sources'])
                checks.update(canonical_answer_equal='pass', canonical_execution_and_single_ui_row='pass')
                if blocked_reply == 'native_mismatch':
                    checks['canonical_answer_equal'] = 'fail'
                metrics['source_counts'] = {'ui':len(result['sources']), 'canonical':len(result['canonical_sources'])}
                if native_mode:
                    report['cases'][-1]['native_rate_limits'] = [{'provider':'Exa', 'provider_retry_after_seconds':60,
                        'local_cooldown_seconds':300, 'provenance':'native_provider_status', 'provider_reset':'minimum_retry_delay'}]
                metrics['model_reported_rate_limit'] = bench.model_reported_rate_limit(case, result, checks)
                return checks
            return {'synthetic_boundary':'fail' if invalid_answer else 'pass'}
        async def exercise(ui, model, prompt, **kwargs):
            nonlocal changed, active
            await kwargs['confirm_ready']()
            calls.append(('submit', prompt))
            await kwargs['submission_observer']()
            if before_admission:
                changed = foreign
                raise bench.Blocked('synthetic_disconnect_before_admission')
            await kwargs['admission_observer']('synthetic-request-' + str(len(calls)))
            if fail:
                changed = foreign
                active = active_failure
                if fail == 'cancel':
                    raise asyncio.CancelledError()
                raise bench.Blocked('synthetic_turn_failure')
            metrics = {'first_answer_ms':1., 'completed_ms':2.}
            await kwargs['terminal_observer']('synthetic-request', metrics)
            return metrics
        args = SimpleNamespace(model='synthetic-model', repetitions=1, turn_timeout=1, cases=cases,
                               history_seed_turns=history_seed_turns)
        report = {'cases':[], 'cleanup':{}}
        with patch.object(bench, 'snapshot_preferences', snapshot), patch.object(bench, 'set_web', web), \
                patch.object(bench, 'set_tts', tts), patch.object(bench, 'new_chat', new), \
                patch.object(bench, 'select_model', model), patch.object(bench, 'begin_ownership', begin), \
                patch.object(bench, 'ownership', owner), patch.object(bench, 'terminal_validation', validation):
            if fail or invalid_answer or before_admission or blocked_reply in ('native_mismatch','native_source_mismatch'):
                with self.assertRaises(asyncio.CancelledError if fail == 'cancel' else bench.Blocked):
                    await bench.run_suite(UI(), args, report, exercise, stages=stages)
            else:
                await bench.run_suite(UI(), args, report, exercise, stages=stages)
        if foreign:
            self.assertNotEqual(prefs, original)
            self.assertEqual(report['cleanup']['preferences'], 'blocked_ownership_fence')
        else:
            self.assertEqual(prefs, original)
            self.assertEqual(report['cleanup']['preferences'], 'pass')
        return calls, report

    async def test_history_seed_order_and_shared_six_cases(self):
        calls, report = await self.run_controller()
        cases = bench.load_scenarios()
        submissions = [c[1] for c in calls if c[0] == 'submit']
        self.assertEqual(submissions, [c['prompt'] for c in cases[:-1]] + [cases[-1]['seed'], cases[-1]['prompt']])
        self.assertEqual(len(report['cases']), 6)
        self.assertIn('not-24', report['cases'][-1]['history_condition'])
        self.assertEqual(report['status'], 'pass')
        self.assertEqual(report['admissions']['submitted'], 7)
        self.assertEqual(report['admissions']['observed'], 7)

    async def test_eight_history_seeds_each_has_profile_and_window(self):
        class Stages:
            def __init__(self): self.before_count, self.after_count = 0, 0
            def before(self, target, model, web):
                self.before_count += 1
                target['local_stages'] = {'coverage':'synthetic'}
                return object()
            def after(self, turn): self.after_count += 1
        stages = Stages()
        calls, report = await self.run_controller(cases=['history'], history_seed_turns=8, stages=stages)
        submitted = [c[1] for c in calls if c[0] == 'submit']
        self.assertEqual(len(submitted), 9)
        for index, prompt in enumerate(submitted[:-1]):
            self.assertTrue(prompt.startswith(bench.load_scenarios()[-1]['seed']))
            self.assertIn(f'Synthetic seed turn {index + 1} of 8', prompt)
        row = report['cases'][0]
        self.assertEqual(len(row['seed_profiles']), 8)
        self.assertEqual(row['history_seed_turns'], 8)
        self.assertEqual(row['seed_metrics']['completed_ms'], 2)
        self.assertEqual(report['admissions']['submitted'], 9)
        self.assertEqual((stages.before_count, stages.after_count), (9, 9))

    async def test_default_disabled_has_no_stage_profile(self):
        with patch.object(bench.ADB, 'run', side_effect=AssertionError('unexpected ADB')):
            _, report = await self.run_controller(cases=['history'])
        self.assertNotIn('local_stages', report['cases'][0])
        self.assertNotIn('local_stages', report['cases'][0]['seed_profiles'][0])

    async def test_restore_preferences_on_turn_failure(self):
        calls, report = await self.run_controller(fail=True)
        self.assertEqual(len(report['cases']), 1)
        self.assertEqual(report['cases'][0]['status'], 'fail')
        self.assertIn(('model', 'synthetic-original'), calls)
        self.assertIn(('tts', True), calls)
        self.assertEqual(report['admissions']['submitted'], 1)

    async def test_foreign_successor_is_never_stopped_or_restored(self):
        calls, report = await self.run_controller(fail=True, foreign=True)
        self.assertNotIn(('ownership', 'cancel'), calls)
        self.assertNotIn(('model', 'synthetic-original'), calls)
        self.assertNotIn(('tts', True), calls)

    async def test_owned_active_failure_cancels_then_restores(self):
        calls, report = await self.run_controller(fail=True, active_failure=True)
        self.assertEqual(calls.count(('ownership', 'cancel')), 1)
        self.assertLess(calls.index(('ownership', 'cancel')), calls.index(('model', 'synthetic-original')))

    async def test_unclaimed_disconnect_never_cancels_unknown_active_request(self):
        calls, report = await self.run_controller(before_admission=True, foreign=True)
        self.assertNotIn(('ownership', 'claim'), calls)
        self.assertNotIn(('ownership', 'cancel'), calls)
        self.assertEqual(report['admissions']['submitted'], 1)
        self.assertEqual(report['admissions']['observed'], 0)

    async def test_async_cancellation_attempts_model_web_tts_restoration(self):
        calls, report = await self.run_controller(fail='cancel')
        self.assertIn(('model', 'synthetic-original'), calls)
        self.assertIn(('tts', True), calls)

    async def test_answer_failure_preserves_completion_metrics(self):
        calls, report = await self.run_controller(invalid_answer=True)
        self.assertTrue(report['cases'][0]['workflow_completed'])
        self.assertEqual(report['cases'][0]['completed_ms'], 2.)
        self.assertEqual(report['cases'][0]['status'], 'fail')

    async def test_blocked_page_stops_mixed_and_restores_preferences(self):
        calls, report = await self.run_controller(cases=['mixed','page_read'], blocked_reply=True)
        self.assertEqual(report['status'], 'blocked')
        self.assertEqual([r['scenario'] for r in report['cases']], ['page_read'])
        row = report['cases'][0]
        self.assertTrue(row['workflow_completed'])
        self.assertEqual(row['status'], 'blocked')
        self.assertEqual(row['checks']['sources_present'], 'fail')
        self.assertEqual(row['provenance'], 'model_reported_rate_limit')
        self.assertIsNone(row['retry_after_seconds'])
        self.assertEqual(row['provider_reset'], 'unknown')
        self.assertEqual(row['completed_ms'], 2.)
        self.assertEqual(report['admissions']['submitted'], 1)
        self.assertIn(('model','synthetic-original'), calls)

    async def test_subset_success_is_not_full_suite_pass(self):
        calls, report = await self.run_controller(cases=['arithmetic'])
        self.assertEqual(report['status'], 'unverified')
        self.assertEqual(report['coverage'], 'partial')
        self.assertEqual(report['suite_deadline_seconds'], 211)

    async def test_native_diagnostic_blocks_without_model_rate_limit_prose(self):
        calls, report = await self.run_controller(cases=['page_read','mixed'], blocked_reply='native')
        self.assertEqual(report['status'], 'blocked')
        self.assertEqual(report['cases'][0]['evidence'], 'native_provider_status')
        self.assertEqual(report['cases'][0]['provider_reset'], 'minimum_retry_delay')
        self.assertEqual(report['admissions']['submitted'], 1)

    async def test_native_evidence_precedes_model_claim_but_cannot_mask_integrity_or_fallback_success(self):
        calls, report = await self.run_controller(cases=['page_read'], blocked_reply='native_with_prose')
        self.assertEqual(report['cases'][0]['evidence'], 'native_provider_status')
        for mode in ('native_mismatch','native_source_mismatch'):
            calls, report = await self.run_controller(cases=['page_read'], blocked_reply=mode)
            self.assertEqual(report['cases'][0]['status'], 'fail')
            self.assertTrue(report['cases'][0]['native_rate_limits'])
        calls, report = await self.run_controller(cases=['page_read'], blocked_reply='native_success')
        self.assertNotEqual(report['cases'][0]['status'], 'blocked')
        self.assertEqual(report['cases'][0]['checks']['sources_present'], 'pass')
        self.assertTrue(report['cases'][0]['native_rate_limits'])

    async def test_canonical_single_row_execution_and_answer_correlation(self):
        result = {'execution_valid':True, 'answer':'210', 'canonical':'210', 'sources':[], 'canonical_sources':[]}
        class UI:
            async def evaluate(self, expression):
                self.expression = expression
                return result
        ui = UI()
        checks = await bench.terminal_validation(ui, bench.load_scenarios()[0], 'synthetic-request', {})
        self.assertEqual(set(checks.values()), {'pass'})
        self.assertIn('turns.length !== 1 || rows.length !== 1', ui.expression)
        self.assertIn('window.__voxBenchmarkExecutions.includes(execution)', ui.expression)
        result['canonical'] = 'different synthetic answer'
        self.assertEqual((await bench.terminal_validation(ui, bench.load_scenarios()[0], 'synthetic-request', {}))['canonical_answer_equal'], 'fail')
        result['execution_valid'] = False
        with self.assertRaises(bench.Blocked):
            await bench.terminal_validation(ui, bench.load_scenarios()[0], 'synthetic-request', {})


class NativeObservationTests(unittest.IsolatedAsyncioTestCase):
    """Fake evaluate/native-callback boundaries; no CDP, app or provider access."""
    def record(self):
        return {'provider':'Parallel', 'provider_retry_after_seconds':None, 'local_cooldown_seconds':60,
                'provenance':'native_provider_status', 'provider_reset':'unknown'}

    def test_only_bounded_provider_records_can_enter_reports(self):
        self.assertEqual(bench.checked_native_diagnostics([self.record()]), [self.record()])
        for change in ({'provider':'Synthetic'}, {'provider_retry_after_seconds':-1},
                       {'provider_retry_after_seconds':31536001}, {'provider_retry_after_seconds':True},
                       {'local_cooldown_seconds':31536001}, {'local_cooldown_seconds':True},
                       {'local_cooldown_seconds':None}, {'raw_header':'synthetic-secret'}):
            with self.assertRaises(bench.Blocked):
                bench.checked_native_diagnostics([{**self.record(), **change}])
        with self.assertRaises(bench.Blocked):
            bench.checked_native_diagnostics([self.record(), self.record()])

    async def run_observation(self, fail=False, detach=True, cancel_read=False):
        row, cleanup, calls = {}, {}, []
        record = self.record()
        class UI:
            async def evaluate(self, expression):
                if expression == bench.NATIVE_READ_JS:
                    calls.append('read')
                    if cancel_read and calls.count('read') == 1:
                        raise asyncio.CancelledError()
                    return [record]
                if expression == bench.NATIVE_DETACH_JS:
                    calls.append('detach')
                    return detach
                calls.append('setup')
                return True
        async def ready():
            pass
        async def submitted():
            calls.append('submitted')
        async def admitted(request):
            calls.append('admitted')
        async def terminal(request, metrics):
            calls.append('terminal')
            self.assertEqual(row['native_rate_limits'], [record])
        async def exercise(ui, model, prompt, **kwargs):
            await kwargs['confirm_ready']()
            await kwargs['submission_observer']()
            await kwargs['admission_observer']('synthetic-request')
            if fail:
                raise RuntimeError('synthetic failed turn')
            await kwargs['terminal_observer']('synthetic-request', {'first_answer_ms':1, 'completed_ms':2})
            return {'first_answer_ms':1, 'completed_ms':2}
        task = bench.exercise_with_native_status(UI(), exercise, 'synthetic-model', 'Synthetic prompt',
            row=row, cleanup=cleanup, web_enabled=True, confirm_ready=ready, submission_observer=submitted,
            admission_observer=admitted, terminal_observer=terminal)
        if not detach:
            with self.assertRaisesRegex(bench.Blocked, 'detach_failed'):
                await task
        elif cancel_read:
            with self.assertRaises(asyncio.CancelledError):
                await task
        elif fail:
            with self.assertRaisesRegex(RuntimeError, 'synthetic failed turn'):
                await task
        else:
            await task
        self.assertEqual(calls.count('detach'), 1)
        return row, cleanup, calls

    async def test_terminal_and_failure_capture_then_disconnect(self):
        for fail in (False, True):
            row, cleanup, calls = await self.run_observation(fail=fail)
            self.assertEqual(row['native_rate_limit_observation'], 'observed')
            self.assertEqual(cleanup['native_status_observer'], 'pass')
            self.assertLess(calls.index('read'), calls.index('detach'))

    async def test_detach_failure_is_not_certified_cleanup(self):
        row, cleanup, calls = await self.run_observation(detach=False)
        self.assertEqual(cleanup['native_status_observer'], 'fail')

    async def test_cancellation_is_not_swallowed_and_still_detaches(self):
        row, cleanup, calls = await self.run_observation(cancel_read=True)
        self.assertNotIn('terminal', calls)
        self.assertEqual(calls.count('submitted'), 1)
        self.assertEqual(cleanup['native_status_observer'], 'pass')


JS_RUNTIME = shutil.which('node') or shutil.which('bun')


@unittest.skipUnless(JS_RUNTIME, 'existing Node/Bun needed for generated-JS fake boundary tests')
class GeneratedNativeStatusTests(unittest.TestCase):
    """Executes observer JS against fake DOM/native snapshots, never an app runtime."""
    def js(self, script):
        result = subprocess.run([JS_RUNTIME, '-e', script], capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, 'synthetic native-status JS failed')
        return json.loads(result.stdout)

    def test_exact_native_grammar_and_numeric_bounds(self):
        texts = ['Rate limited · Exa · provider wait 60 s · local wait 300 s',
                 'Rate limited · Parallel · provider wait unknown · local wait 60 s',
                 'Rate limited · Exa · provider wait 31536000 s · local wait 0 s',
                 'Rate limited · Exa · provider wait 31536001 s · local wait 0 s',
                 'Rate limited · Exa · provider wait -1 s · local wait 0 s',
                 'Rate limited · Exa · provider wait unknown s · local wait 60 s',
                 'Rate limited · Synthetic · provider wait 1 s · local wait 1 s',
                 'Synthetic answer: Rate limited · Exa · provider wait 1 s · local wait 1 s',
                 'Rate limited · Exa · provider wait 1 s · local wait 1 s\n']
        parsed = self.js(bench.NATIVE_PARSE_JS + '\nconsole.log(JSON.stringify(' + json.dumps(texts) + '.map(t=>parseNativeRate(t,1000))));')
        self.assertEqual(parsed[0]['providerWait'], 60)
        self.assertIsNone(parsed[1]['providerWait'])
        self.assertEqual(parsed[2]['providerWait'], 31536000)
        self.assertEqual(parsed[3:], [None]*6)

    def test_scoped_observer_retains_removed_status_without_resetting_wait(self):
        script = """let now=1000, callback, disconnected=0, nodes=[];
global.performance={now:()=>now};
const form={nodeType:1, matches:()=>false, closest:()=>null,
 querySelectorAll:selector=>{if(!selector.includes('.composer__actions--secondary')||!selector.includes('.composer__status'))throw Error('unscoped selector');return nodes.filter(n=>n.scope)}, contains:n=>n===form||n.inForm};
const node=(scope,text)=>({scope,inForm:true,nodeType:1, matches:()=>scope,
 closest:function(){return scope?this:null},querySelectorAll:()=>[],get textContent(){if(!scope)throw Error('unrelated content read');return text}});
const answer=node(false,'synthetic answer'), unrelated=node(false,'synthetic unrelated status');
const previous=node(true,'synthetic prior status');
Object.defineProperty(previous,'textContent',{get(){throw Error('prior activity read')}});
nodes=[answer,unrelated,previous];
global.document={getElementById:id=>id==='promptComposer'?{closest:()=>form}:null};
const execution={session_id:'synthetic-session',generation:'1'};
const owner={session:'synthetic-session',request:'synthetic-request',execution:JSON.stringify(execution),before:'[]'};
global.window={__voxBenchmarkOwner:owner,__TAURI_INTERNALS__:{invoke:async()=>({session_id:owner.session,
 active_request:null,transcript:[{request_id:owner.request,execution_id:execution}]})}};
global.MutationObserver=class {constructor(fn){callback=fn}observe(root){if(root!==form)throw Error('wrong root')}
 takeRecords(){return []}disconnect(){disconnected++}};
"""
        script += 'if(!eval(' + json.dumps(bench.NATIVE_INSTALL_JS) + '))throw Error("install");\n'
        script += 'eval(' + json.dumps(bench.NATIVE_ARM_JS) + ');\n'
        script += """window.__voxBenchmarkRateStatus.submitted=true;window.__voxBenchmarkRateStatus.admitted=owner.request;
callback([{target:unrelated,addedNodes:[],removedNodes:[]}]);
previous.inForm=false;nodes=[answer,unrelated];callback([{target:form,addedNodes:[],removedNodes:[previous]}]);
if(window.__voxBenchmarkRateStatus.records.size!==0)throw Error('prior status captured');
const status=node(true,'Rate limited · Exa · provider wait 60 s · local wait 300 s');
const parallel=node(true,'Rate limited · Parallel · provider wait unknown · local wait 60 s');
nodes.push(status,parallel);callback([{target:form,addedNodes:[status,parallel],removedNodes:[]}]);
now=3100;status.inForm=false;parallel.inForm=false;nodes=[answer,unrelated];
callback([{target:form,addedNodes:[],removedNodes:[status,parallel]}]);
"""
        script += '(async()=>{const records=await eval(' + json.dumps(bench.NATIVE_READ_JS) + ');eval(' + json.dumps(bench.NATIVE_DETACH_JS) + ');console.log(JSON.stringify({records,disconnected,deleted:!window.__voxBenchmarkRateStatus}));})();'
        result = self.js(script)
        self.assertEqual(result['records'][0]['provider_retry_after_seconds'], 58)
        self.assertEqual(result['records'][0]['local_cooldown_seconds'], 298)
        self.assertEqual(len(result['records']), 2)
        self.assertIsNone(result['records'][1]['provider_retry_after_seconds'])
        self.assertEqual(result['records'][1]['local_cooldown_seconds'], 58)
        self.assertEqual(result['records'][1]['provider_reset'], 'unknown')
        self.assertEqual(result['disconnected'], 1)
        self.assertTrue(result['deleted'])


@unittest.skipUnless(JS_RUNTIME, 'optional existing Node/Bun needed for generated-JS fake boundary tests')
class GeneratedGuardTests(unittest.IsolatedAsyncioTestCase):
    """Actual generated JS, but a synthetic in-memory native/DOM boundary, not Tauri."""
    def ui(self, active=False, foreign=False):
        execution = {'session_id':'synthetic-session', 'generation':'1'}
        state = {'owner':{'session':'synthetic-session', 'request':'synthetic-request',
                          'execution':json.dumps(execution, separators=(',', ':')), 'before':'[]', 'baseline':'[]'},
                 'snapshot':{'session_id':'synthetic-session', 'runtime_phase':'sleeping', 'transcript':[],
                             'active_request':None}, 'calls':[]}
        if active:
            state['snapshot']['active_request'] = {'request_id':'synthetic-other' if foreign else 'synthetic-request',
                                                   'execution_id':{**execution, 'generation':'2' if foreign else '1'}}
            state['snapshot']['runtime_phase'] = 'executing'
        class UI:
            async def evaluate(self, expression):
                script = f"""const state = {json.dumps(state)};
global.document = {{getElementById:()=>({{value:state.draft||''}}), querySelector:()=>null}};
global.window = {{__voxBenchmarkOwner:state.owner, __voxBenchmarkNextSession:state.nextSession,
 __voxBenchmarkExecutions:state.executions, __TAURI_INTERNALS__:{{invoke:async(command,args)=>{{
  if(command==='get_app_snapshot') return structuredClone(state.snapshot);
  state.calls.push([command,args]);
  if(command==='cancel_prompt') {{state.snapshot.active_request=null; state.snapshot.runtime_phase='sleeping';}}
  return null;
}}}}}};
(async()=>{{const value=await (0,eval)({json.dumps(expression)});
 state.owner=window.__voxBenchmarkOwner; state.nextSession=window.__voxBenchmarkNextSession??null;
 state.executions=window.__voxBenchmarkExecutions??null; console.log(JSON.stringify({{value,state}}));}})()
 .catch(()=>process.exit(3));"""
                result = subprocess.run([JS_RUNTIME, '-e', script], capture_output=True, text=True, timeout=5)
                if result.returncode:
                    raise AssertionError('generated fake-boundary JS failed')
                value = json.loads(result.stdout)
                state.update(value['state'])
                return value['value']
        return UI(), state

    async def test_cancelled_owned_request_need_not_be_a_committed_turn(self):
        ui, state = self.ui()
        self.assertEqual(await bench.ownership(ui), 'idle')
        self.assertTrue(await ui.evaluate('(async()=>{' + bench.IDLE_FENCE + 'return true;})()'))

    async def test_native_cancel_is_addressed_only_to_exact_owned_request(self):
        ui, state = self.ui(active=True)
        self.assertEqual(await bench.ownership(ui, 'cancel'), 'owned_active')
        self.assertEqual(state['calls'], [['cancel_prompt', {'requestId':'synthetic-request'}]])
        self.assertEqual(await bench.ownership(ui), 'idle')

    async def test_foreign_successor_cannot_be_cancelled_or_mutated(self):
        ui, state = self.ui(active=True, foreign=True)
        self.assertEqual(await bench.ownership(ui, 'cancel'), 'foreign')
        self.assertEqual(state['calls'], [])
        self.assertFalse(await ui.evaluate('(async()=>{' + bench.IDLE_FENCE + 'return true;})()'))

    async def test_same_request_different_execution_is_foreign(self):
        ui, state = self.ui(active=True)
        state['snapshot']['active_request']['execution_id']['generation'] = '2'
        self.assertEqual(await bench.ownership(ui, 'cancel'), 'foreign')
        self.assertEqual(state['calls'], [])

    async def test_foreign_completed_successor_is_not_idle_owned_history(self):
        ui, state = self.ui()
        state['snapshot']['transcript'] = [{'request_id':'synthetic-other',
            'execution_id':{'session_id':'synthetic-session', 'generation':'2'}}]
        self.assertEqual(await bench.ownership(ui), 'foreign')
        self.assertFalse(await ui.evaluate('(async()=>{' + bench.IDLE_FENCE + 'return true;})()'))

    async def test_new_chat_cannot_accept_unchanged_empty_old_session(self):
        ui, state = self.ui()
        async def click(ui, label):
            pass  # Fake UI action acknowledged, but native reset never happened.
        async def one_observation(predicate, timeout=10):
            if not await predicate():
                raise bench.Blocked('synthetic_unchanged_session')
        with patch.object(bench, 'click', click), patch.object(bench, 'wait_condition', one_observation):
            with self.assertRaisesRegex(bench.Blocked, 'unchanged_session'):
                await bench.new_chat(ui)
        self.assertEqual(state['owner']['session'], 'synthetic-session')

    async def test_new_chat_claims_only_observed_changed_empty_session(self):
        ui, state = self.ui()
        async def click(ui, label):
            state['snapshot']['session_id'] = 'synthetic-new-session'
        with patch.object(bench, 'click', click):
            await bench.new_chat(ui)
        self.assertEqual(state['owner']['session'], 'synthetic-new-session')
        self.assertEqual(state['executions'], [])


if __name__ == '__main__':
    unittest.main()
