import importlib.util
import json
from pathlib import Path
import unittest


def load():
    spec = importlib.util.spec_from_file_location('benchmark_stages', Path(__file__).with_name('stages.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class StagesTest(unittest.TestCase):
    def setUp(self):
        self.s = load()

    def events(self):
        base = dict(schema=1, session='00000000-0000-0000-0000-000000000001', run=1,
                    operation='GENERATION', observedNanos=1_000_000_000, dropped=0)
        return [dict(base, kind='BEGIN', elapsedNanos=0),
                dict(base, kind='MARK', stage='REQUESTED', elapsedNanos=0),
                dict(base, kind='METADATA', model='custom-luna-none', webEnabled=False),
                dict(base, kind='MARK', stage='ADMISSION_STARTED', elapsedNanos=2_000_000),
                dict(base, kind='END', stage='SETTLED', elapsedNanos=3_000_000)]

    def profile(self, events):
        return self.s.summarize(events, (990_000_000, 1_100_000_000), 'custom-luna-none', False)

    def test_unique_and_missing_stages(self):
        result = self.profile(self.events())
        self.assertEqual(result['correlation'], 'verified')
        self.assertEqual(result['coverage'], 'unverified')
        self.assertEqual(result['storage_coverage'], 'unverified')
        self.assertEqual(result['android_intervals_ms']['admission_bookkeeping'], 2)
        self.assertNotIn('settlement_queue', result['android_intervals_ms'])
        self.assertNotIn('00000000', json.dumps(result))
        self.assertEqual(result['startup']['coverage'], 'unverified')

    def full_events(self):
        events = self.events()
        marks = ('ADMISSION_QUEUED', 'ADMITTED', 'SETTLEMENT_QUEUED', 'SETTLEMENT_DEQUEUED',
                 'SETTLEMENT_STARTED', 'DURABLE_COMMITTED', 'SETTLED')
        for index, stage in enumerate(marks):
            elapsed = 1_000_000 if stage == 'ADMISSION_QUEUED' else (index + 3) * 1_000_000
            events.append(dict(events[0], kind='MARK', stage=stage, elapsedNanos=elapsed))
        phases = ('sqlite_begin sqlite_read sqlite_write sqlite_commit sqlite_prepare record_encode '
                  'record_decode reducer projection refresh_reload quota_check').split()
        events.append(dict(events[0], operation='STORAGE', kind='STORAGE', run=2, parentRun=1,
                           call='complete', outcome='ok', metrics={
                               phase:dict(count=0,total_ns=0,max_ns=0) for phase in phases}))
        return events

    def test_full_required_boundaries_and_explicit_portable_schema(self):
        events = self.full_events()
        profile = self.profile(events)
        self.assertEqual(profile['coverage'], 'verified')
        self.assertEqual(profile['storage_coverage'], 'verified')
        self.assertEqual(profile['missing_required_intervals'], [])
        self.assertEqual(profile['missing_storage_phases'], [])
        self.assertEqual(profile['storage']['metrics']['sqlite_prepare']['count'], 0)
        self.assertIn('commit_lock', profile['missing_intervals'])  # Conditional, not mandatory.
        for phase in list(events[-1]['metrics']):
            partial = json.loads(json.dumps(events))
            del partial[-1]['metrics'][phase]
            result = self.profile(partial)
            self.assertEqual(result['coverage'], 'unverified')
            self.assertEqual(result['correlation'], 'verified')
            self.assertEqual(result['missing_storage_phases'], [phase])
            self.assertNotIn(phase, result['storage']['metrics'])
            self.assertIn('prepare_commit_inclusive', result['android_intervals_ms'])

    def test_missing_mandatory_local_boundary_keeps_coarse_observations(self):
        events = [e for e in self.full_events() if e.get('stage') != 'ADMISSION_QUEUED']
        profile = self.profile(events)
        self.assertEqual(profile['coverage'], 'unverified')
        self.assertEqual(profile['correlation'], 'verified')
        self.assertEqual(profile['storage_coverage'], 'verified')
        self.assertEqual(profile['missing_required_intervals'], ['admission_queue'])
        self.assertEqual(profile['android_intervals_ms']['admission_bookkeeping'], 2)

    def test_full_call_cannot_mask_another_calls_missing_native_diagnostics(self):
        events = self.full_events()
        partial = json.loads(json.dumps(events[-1]))
        partial['run'] = 3
        partial['metrics'] = {'jni_execute':dict(count=1,total_ns=5_000_000,max_ns=5_000_000)}
        profile = self.profile(events + [partial])
        self.assertEqual(profile['coverage'], 'unverified')
        self.assertEqual(profile['storage_coverage'], 'unverified')
        self.assertEqual(profile['incomplete_storage_records'], 1)
        self.assertEqual(profile['missing_storage_phases'], list(self.s.PORTABLE_PHASES))
        self.assertEqual(profile['storage']['metrics']['jni_execute']['total_ms'], 5)
        self.assertIn('sqlite_prepare', profile['storage']['metrics'])

    def test_multiple_jni_calls_require_matching_native_input_counts(self):
        for native_count, coverage in ((1,'unverified'), (2,'verified')):
            events = self.full_events()
            events[-1]['metrics'].update({
                'jni_execute':dict(count=1,total_ns=2_000_000,max_ns=2_000_000),
                'native_open':dict(count=1,total_ns=1_000_000,max_ns=1_000_000),
                'native_input_decode':dict(count=native_count,total_ns=200,max_ns=100)})
            profile = self.profile(events)
            self.assertEqual(profile['coverage'], coverage)
            self.assertEqual(profile['native_boundary_coverage'], coverage)
            self.assertEqual(profile['native_boundary_mismatched_records'], int(native_count == 1))
            self.assertEqual(profile['storage']['metrics']['native_open']['total_ms'], 1)
        portable_only = self.profile(self.full_events())
        self.assertEqual(portable_only['coverage'], 'verified')
        self.assertEqual(portable_only['native_boundary_coverage'], 'unverified')
        self.assertNotIn('native_input_decode', portable_only['storage']['metrics'])

    def test_ambiguous_stale_drop_restart(self):
        events = self.events()
        for bad in ([], [dict(e, observedNanos=100) for e in events],
                    events + [dict(e, run=2) for e in events],
                    events + [dict(events[0], session='00000000-0000-0000-0000-000000000002')],
                    [dict(e, dropped=1) for e in events]):
            self.assertEqual(self.profile(bad)['coverage'], 'unverified')
        # A previous boot can have identical boottime values. Only records
        # beyond the pre-suite metadata watermark may start a measured run.
        stale = [dict(e, _fresh=False) for e in events]
        self.assertEqual(self.s.summarize(stale, (990_000_000,1_100_000_000),
                                         'custom-luna-none', False, require_fresh=True)['coverage'], 'unverified')

    def test_storage_is_nested_not_added(self):
        events = self.events() + [dict(schema=1, session=self.events()[0]['session'], run=2,
            parentRun=1, operation='STORAGE', kind='STORAGE', observedNanos=1_010_000_000,
            dropped=0, call='complete', outcome='ok', metrics={
                'store_total':dict(count=1,total_ns=9_000_000,max_ns=9_000_000),
                'sqlite_commit':dict(count=2,total_ns=4_000_000,max_ns=3_000_000)})]
        result = self.profile(events)
        self.assertEqual(result['storage']['metrics']['sqlite_commit'], dict(count=2,total_ms=4,max_ms=3))
        self.assertEqual(result['storage']['calls']['complete']['ok'], 1)
        self.assertNotIn('total_ms', result['storage'])
        for value in (True, -1, 1.5, 'secret', 2**64):
            bad = json.loads(json.dumps(events))
            bad[-1]['metrics']['store_total']['total_ns'] = value
            self.assertEqual(self.profile(bad)['coverage'], 'unverified')
        events[-1]['metrics']['private_sql'] = dict(count=1,total_ns=1,max_ns=1)
        self.assertEqual(self.profile(events)['coverage'], 'unverified')

    def test_uptime_strict(self):
        class ADB:
            def run(self, *args):
                self.args = args
                return '123.45 999.99'
        adb = ADB()
        self.assertEqual(self.s.uptime(adb), 123_450_000_000)
        self.assertEqual(adb.args, ('shell', 'cat', '/proc/uptime'))

    def test_reader_known_paths_and_bounds(self):
        name = 'assistant-timing-123-00000000-0000-0000-0000-000000000001-1.jsonl'
        class ADB:
            def __init__(self): self.commands = []
            def run(self, *args):
                self.commands.append(args)
                return ''
        adb = ADB()
        self.s.read_journal(adb)
        command = adb.commands[0][-1]
        self.assertIn('/storage/emulated/0/Android/data/dev.gitblame.voxgolem.android/files/diagnostics/assistant-timing', command)
        self.assertIn('no_backup/diagnostics/assistant-timing', adb.commands[1][-1])
        self.assertNotIn('find ', command)
        self.assertIn('! -L', command)
        self.assertIsNotNone(self.s.FILE_NAME.fullmatch(name))
        self.assertIsNone(self.s.FILE_NAME.fullmatch('../' + name))

    def test_reader_validates_size_session_and_budgets_before_read(self):
        name = 'assistant-timing-123-00000000-0000-0000-0000-000000000001-1.jsonl'
        text = json.dumps(self.events()[0]) + '\n'
        class ADB:
            def __init__(self, listing, raw): self.listing, self.raw, self.commands = listing, raw, []
            def run(self, *args):
                self.commands.append(args)
                return self.listing if 'wc -c' in args[-1] else self.raw + 'JOURNAL_END'
        adb = ADB(f'{name} {len(text.encode())}', text)
        self.assertEqual(len(self.s.read_journal(adb)), 1)
        self.assertEqual(len(adb.commands), 2)  # No fallback read once external selected.
        baseline = {('/storage/emulated/0/Android/data/' + self.s.PACKAGE + '/files/diagnostics/assistant-timing', False, name):len(text.encode())}
        self.assertFalse(self.s.read_journal(ADB(f'{name} {len(text.encode())}', text), baseline)[0]['_fresh'])
        for listing in (f'../{name} 1', f'{name} {256*1024+1}', '\n'.join([f'{name} 262144'] * 33)):
            adb = ADB(listing, 'x' * 262144)
            with self.assertRaises((ValueError, json.JSONDecodeError)):
                self.s.read_journal(adb)
        adb = ADB(f'{name} {len(text.encode())}', text + 'x')
        with self.assertRaises(ValueError): self.s.read_journal(adb)

    def test_collector_missing_terminal_and_read_failure_are_unverified(self):
        class ADB:
            def run(self, *args):
                if args == ('shell','cat','/proc/uptime'): return '1.00 2.00'
                raise RuntimeError('synthetic read failure payload must not escape')
        target = {}
        collector = self.s.Collector(ADB())
        collector.before(target, 'custom-luna-none', False)
        collector.finish()
        self.assertEqual(target['local_stages']['coverage'], 'unverified')
        self.assertNotIn('payload', json.dumps(target))
        self.assertEqual(collector.turns, [])

    def test_same_session_startup_and_repeat_summary(self):
        activity = [dict(e, operation='ACTIVITY', run=8, observedNanos=900_000_000) for e in self.events()[:1]]
        activity += [dict(activity[0], kind='MARK', stage='ACTIVITY_CREATED', elapsedNanos=0),
                     dict(activity[0], kind='MARK', stage='FIRST_PROJECTION', elapsedNanos=5_000_000)]
        profile = self.profile(activity + self.events())
        self.assertEqual(profile['startup']['coverage'], 'observed')
        rows = [dict(scenario='arithmetic', local_stages=profile) for _ in range(3)]
        summary = self.s.repeat_summary(rows)
        self.assertEqual(summary['arithmetic:followup']['android:admission_bookkeeping:ms'],
                         dict(samples=3, min=2, median=2, max=2))
        self.assertNotIn('p95', json.dumps(summary))

    def test_largest_allowed_history_report_stays_within_existing_budget(self):
        for seed_count, repetitions, other_cases in ((8,2,1), (1,3,5)):
            self.assertLessEqual((seed_count + 1 + other_cases) * repetitions, 21)
            profile = self.profile(self.events())
            profile['storage'] = {'metrics':{phase:{'count':2**64-1, 'total_ms':(2**64-1)/1e6,
                                                    'max_ms':(2**64-1)/1e6} for phase in self.s.PHASES},
                                  'calls':{call:{'ok':256,'error':256} for call in self.s.CALLS}}
            rows = []
            for repetition in range(repetitions):
                rows.append(dict(scenario='history', repetition=repetition, local_stages=profile,
                    history_seed_turns=seed_count, first_answer_ms=1, completed_ms=2,
                    seed_profiles=[dict(seed_number=i+1, first_answer_ms=1, completed_ms=2,
                                        local_stages=profile) for i in range(seed_count)]))
                for case in range(other_cases):
                    rows.append(dict(scenario=f'synthetic_case_{case}', repetition=repetition,
                                     first_answer_ms=1, completed_ms=2, local_stages=profile))
            # Deep-copy so budget shaping cannot alias the synthetic profiles.
            report = json.loads(json.dumps(dict(cases=rows, status='pass', cleanup={'preferences':'pass'},
                                                stage_collection={'status':'observed'})))
            report['stage_collection']['repeat_summary'] = self.s.repeat_summary(report['cases'])
            self.s.shape_report(report, 128*1024)
            self.assertLessEqual(len(json.dumps(report, indent=2).encode()), 128*1024)
            self.assertEqual(report['cleanup']['preferences'], 'pass')
            self.assertEqual(report['cases'][0]['history_seed_turns'], seed_count)
            self.assertIn('first_answer_ms', report['cases'][0]['seed_profiles'][0])

    def test_emergency_detail_budget_is_explicitly_unverified(self):
        report = {'status':'pass', 'cases':[{'scenario':'arithmetic','local_stages':self.profile(self.events())}],
                  'stage_collection':{'status':'observed'}}
        self.s.shape_report(report, 900)
        self.assertEqual(report['cases'][0]['local_stages']['coverage'], 'unverified')
        self.assertEqual(report['status'], 'unverified')


if __name__ == '__main__': unittest.main()
