"""Opt-in, bounded Android journal reader. Raw records and join keys never leave memory."""
import json
import re
import shlex
import statistics
from decimal import Decimal

PACKAGE = 'dev.gitblame.voxgolem.android'
UUID = r'[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'
FILE_NAME = re.compile(r'assistant-timing-(?:0|[1-9][0-9]{0,18})-(' + UUID + r')-[1-9][0-9]{0,9}\.jsonl')
PHASES = set(('sqlite_begin sqlite_read sqlite_write sqlite_commit sqlite_prepare record_encode record_decode '
              'reducer projection refresh_reload quota_check final_notify store_lock_wait store_total '
              'kotlin_request_encode kotlin_response_decode jni_execute native_open native_input_decode '
              'native_registry_wait native_store_wait native_request_decode native_dispatch native_response_encode').split())
CALLS = set(('projection context complete select save_draft allocate_id local_state draft create_conversation '
             'read snapshot replica conversations heads commit projection_still_current publications '
             'receipts_to_publish acknowledge close other').split())
# Completed normal generations require the two executor queue intervals,
# admission, durable prepare/commit, and full settlement. Lock/selection and
# projection subintervals remain optional: some are legitimately conditional.
REQUIRED_INTERVALS = ('admission_queue', 'admission_inclusive', 'settlement_queue',
                      'prepare_commit_inclusive', 'settlement_inclusive')
# The portable producer emits all eleven keys, even when count is zero. An
# absent key is missing evidence, never an inferred zero or complete DB trace.
PORTABLE_PHASES = ('sqlite_begin', 'sqlite_read', 'sqlite_write', 'sqlite_commit', 'sqlite_prepare',
                   'record_encode', 'record_decode', 'reducer', 'projection', 'refresh_reload', 'quota_check')
PAIRS = {
    'admission_bookkeeping': ('REQUESTED', 'ADMISSION_STARTED'),
    'admission_queue': ('ADMISSION_QUEUED', 'ADMISSION_STARTED'),
    'admission_inclusive': ('ADMISSION_STARTED', 'ADMITTED'),
    'admission_lock': ('ADMISSION_LOCK_REQUESTED', 'ADMISSION_LOCK_ACQUIRED'),
    'projection_wrapper': ('CREDENTIAL_CHECKED', 'PROJECTION_REFRESHED'),
    'admission_persist': ('CONTEXT_PREPARED', 'ADMISSION_PERSISTED'),
    'request_serialization': ('ADMISSION_PERSISTED', 'REQUEST_SERIALIZED'),
    'post_provider_inclusive': ('OUTCOME_RECEIVED', 'SETTLEMENT_STARTED'),
    'settlement_queue': ('SETTLEMENT_QUEUED', 'SETTLEMENT_DEQUEUED'),
    'settlement_lock': ('SETTLEMENT_LOCK_REQUESTED', 'SETTLEMENT_LOCK_ACQUIRED'),
    'commit_lock': ('COMMIT_LOCK_REQUESTED', 'COMMIT_LOCK_ACQUIRED'),
    'prepare_commit_inclusive': ('SETTLEMENT_STARTED', 'DURABLE_COMMITTED'),
    'selection_persist': ('DURABLE_COMMITTED', 'SELECTION_PERSISTED'),
    'settlement_projection': ('SELECTION_PERSISTED', 'SETTLEMENT_PROJECTED'),
    'settlement_inclusive': ('SETTLEMENT_STARTED', 'SETTLED'),
}
STARTUP_PAIRS = {
    'activity_to_first_projection': ('ACTIVITY_CREATED', 'FIRST_PROJECTION'),
    'plugin_load': ('PLUGIN_LOAD_STARTED', 'PLUGIN_LOAD_COMPLETED'),
    'runtime_create': ('RUNTIME_CREATE_STARTED', 'RUNTIME_CREATED'),
    'hydration_queue': ('HYDRATION_QUEUED', 'HYDRATION_STARTED'),
    'hydration_inclusive': ('HYDRATION_STARTED', 'HYDRATION_COMPLETED'),
    'initial_snapshot': ('INITIAL_SNAPSHOT_REQUESTED', 'INITIAL_SNAPSHOT_REPLIED'),
    'frontend_ready_ack': ('ACTIVITY_CREATED', 'FRONTEND_READY_ACK'),
}
MAX_FILE = 256 * 1024
MAX_BYTES = 8 * 1024 * 1024


def uint(value):
    return type(value) is int and 0 <= value < 2**64


def uptime(adb):
    text = adb.run('shell', 'cat', '/proc/uptime')
    if not re.fullmatch(r'[0-9]{1,12}\.[0-9]{2} [0-9]{1,16}\.[0-9]{2}', text):
        raise ValueError('uptime_unavailable')
    return int(Decimal(text.split()[0]) * 1_000_000_000)


def _command(adb, root, private, script):
    # All directory components are fixed and individually checked; never follow a
    # symlink to arbitrary app data. No caller-supplied path or general exporter.
    parts = root.split('/')
    checks = []
    for i in range(1, len(parts) + 1):
        prefix = '/'.join(parts[:i])
        if prefix:
            checks.append('[ ! -L ' + shlex.quote(prefix) + ' ]')
    body = ' && '.join(checks) + ' && { ' + script + '; }'
    command = ('run-as ' + PACKAGE + ' sh -c ' + shlex.quote(body)) if private else ('sh -c ' + shlex.quote(body))
    return adb.run('shell', command)


def journal_files(adb):
    roots = [('/storage/emulated/0/Android/data/' + PACKAGE + '/files/diagnostics/assistant-timing', False),
             ('no_backup/diagnostics/assistant-timing', True)]
    files, total, count = [], 0, 0
    for root, private in roots:
        q = shlex.quote(root)
        try:
            listing = _command(adb, root, private,
                'n=0; for f in ' + q + '/assistant-timing-*.jsonl; do '
                '[ -e "$f" ] || continue; n=$((n+1)); [ "$n" -le 128 ] || exit 1; '
                '[ ! -L "$f" ] && [ -f "$f" ] || exit 1; '
                'printf "%s " "${f##*/}"; wc -c < "$f"; done')
        except Exception:
            if private:
                continue  # Non-debuggable external-only builds may refuse run-as.
            raise
        if not listing:
            continue
        for line in listing.splitlines():
            fields = line.split()
            if len(fields) != 2 or not FILE_NAME.fullmatch(fields[0]) or not fields[1].isdigit():
                raise ValueError('journal_listing_invalid')
            name, size = fields[0], int(fields[1])
            count += 1
            # Bound each read even if a file grows after stat; refuse racing
            # growth rather than silently presenting a truncated journal as full.
            limit = min(size + 1, MAX_FILE)
            if count > 128 or size > MAX_FILE or total + limit > MAX_BYTES:
                raise ValueError('journal_limit')
            total += limit
            files.append((root, private, name, size))
        if files:
            break  # forContext selects external storage OR noBackup, never both.
    return files


def read_journal(adb, baseline=None):
    records = []
    for root, private, name, size in journal_files(adb):
        limit = min(size + 1, MAX_FILE)
        path = shlex.quote(root + '/' + name)
        raw = _command(adb, root, private,
            '[ ! -L ' + path + ' ] && [ -f ' + path + ' ] && { head -c ' + str(limit) + ' ' + path + '; printf "JOURNAL_END"; }')
        if not raw.endswith('JOURNAL_END'):
            raise ValueError('journal_read_invalid')
        raw = raw[:-len('JOURNAL_END')]
        if len(raw.encode('utf-8')) != size:
            raise ValueError('journal_file_limit')
        session = FILE_NAME.fullmatch(name)[1]
        offset = 0
        for text in raw.splitlines(keepends=True):
            record = json.loads(text, object_pairs_hook=_unique)
            if type(record) is not dict or record.get('session') != session:
                raise ValueError('journal_session_invalid')
            if baseline is not None:
                record['_fresh'] = offset >= baseline.get((root, private, name), 0)
            offset += len(text.encode('utf-8'))
            records.append(record)
    return records


def _unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('duplicate_key')
        result[key] = value
    return result


def intervals(events, pairs):
    marks = {}
    for event in events:
        if event.get('kind') == 'MARK':
            stage, elapsed = event.get('stage'), event.get('elapsedNanos')
            if not uint(elapsed) or stage in marks:
                raise ValueError('stage_invalid')
            marks[stage] = elapsed
    values = {}
    for label, (start, end) in pairs.items():
        if start in marks and end in marks:
            if marks[end] < marks[start]:
                raise ValueError('stage_order_invalid')
            values[label] = (marks[end] - marks[start]) / 1_000_000
    return values


def summarize(records, window, model, web, require_fresh=False):
    unknown = {'coverage':'unverified', 'drop_check':'unverified', 'reason':'missing_ambiguous_invalid_or_dropped_journal'}
    try:
        if not window or len(window) != 2 or not all(uint(n) for n in window) or window[0] > window[1]:
            return unknown
        # /proc/uptime is centisecond precision. Bounds are only admission
        # selectors, NEVER durations or a host/device clock conversion.
        low, high = max(0, window[0] - 10_000_000), window[1] + 10_000_000
        relevant = [r for r in records if uint(r.get('observedNanos')) and low <= r['observedNanos'] <= high]
        if any(uint(r.get('dropped')) and r['dropped'] > 0 for r in relevant):
            return {'coverage':'unverified', 'drop_check':'fail', 'reason':'dropped_journal_records'}
        if not relevant or any(type(r.get('schema')) is not int or r['schema'] != 1 or not uint(r.get('run')) or
                not uint(r.get('dropped')) or r['dropped'] != 0 or
                not isinstance(r.get('session'), str) or not re.fullmatch(UUID, r['session']) for r in relevant):
            return unknown
        sessions = {r['session'] for r in relevant}
        if len(sessions) != 1:
            return unknown
        candidates = [r for r in relevant if r.get('operation') == 'GENERATION' and r.get('kind') == 'BEGIN' and
                      (not require_fresh or r.get('_fresh') is True)]
        if len(candidates) != 1:
            return unknown
        begin = candidates[0]
        events = [r for r in records if r.get('session') == begin['session'] and r.get('run') == begin['run']]
        if any(r.get('operation') != 'GENERATION' or type(r.get('schema')) is not int or r['schema'] != 1 or
               not uint(r.get('observedNanos')) or r.get('dropped') != 0 for r in events):
            return unknown
        metadata = [r for r in events if r.get('kind') == 'METADATA']
        if not any(r.get('model') == model and r.get('webEnabled') is web for r in metadata):
            return unknown
        if any(('model' in r and r['model'] != model) or ('webEnabled' in r and r['webEnabled'] is not web) for r in metadata):
            return unknown
        if sum(r.get('kind') == 'BEGIN' for r in events) != 1 or sum(r.get('kind') == 'END' for r in events) != 1:
            return unknown
        if sum(r.get('kind') == 'MARK' and r.get('stage') == 'REQUESTED' for r in events) != 1:
            return unknown
        result = {'correlation':'verified', 'coverage':'unverified', 'drop_check':'pass', 'android_intervals_ms':intervals(events, PAIRS),
                  'missing_intervals':[], 'storage':{'metrics':{}, 'calls':{}}, 'storage_coverage':'unverified',
                  'startup':{'coverage':'unverified'}, 'native_model':{}}
        result['missing_intervals'] = [key for key in PAIRS if key not in result['android_intervals_ms']]
        result['missing_required_intervals'] = [key for key in REQUIRED_INTERVALS if key in result['missing_intervals']]
        for record in metadata:
            for key in ('search_ms', 'first_text_ms', 'completed_ms'):
                if key in record:
                    if not uint(record[key]): return unknown
                    if key in result['native_model'] and result['native_model'][key] != record[key]: return unknown
                    result['native_model'][key] = record[key]
            if 'warm' in record:
                if type(record['warm']) is not bool: return unknown
                if 'warm' in result['native_model'] and result['native_model']['warm'] is not record['warm']: return unknown
                result['native_model']['warm'] = record['warm']
        storage = [r for r in records if r.get('session') == begin['session'] and
                   r.get('parentRun') == begin['run'] and r.get('kind') == 'STORAGE']
        seen_storage = set()
        missing_phases = set()
        incomplete_records = boundary_unmeasured = boundary_mismatched = 0
        for record in storage:
            if record.get('operation') != 'STORAGE' or type(record.get('schema')) is not int or record['schema'] != 1 or not uint(record.get('run')) or not uint(record.get('observedNanos')) or not uint(record.get('dropped')) or record['dropped'] != 0 or record.get('call') not in CALLS or record.get('outcome') not in ('ok', 'error') or record['run'] in seen_storage:
                return unknown
            seen_storage.add(record['run'])
            metrics = record.get('metrics')
            if type(metrics) is not dict or not metrics or set(metrics) - PHASES:
                return unknown
            calls = result['storage']['calls'].setdefault(record['call'], {'ok':0, 'error':0})
            calls[record['outcome']] += 1
            target = result['storage']['metrics']
            for phase, metric in metrics.items():
                if type(metric) is not dict or set(metric) != {'count', 'total_ns', 'max_ns'} or not all(uint(v) for v in metric.values()) or metric['max_ns'] > metric['total_ns'] or (metric['count'] == 0 and metric['total_ns'] != 0):
                    return unknown
                aggregate = target.setdefault(phase, {'count':0, 'total_ms':0, 'max_ms':0})
                aggregate['count'] += metric['count']
                aggregate['total_ms'] += metric['total_ns'] / 1_000_000
                aggregate['max_ms'] = max(aggregate['max_ms'], metric['max_ns'] / 1_000_000)
            # Check each API summary before aggregation: one complete call must
            # not conceal another call's omitted optional native diagnostics.
            missing = set(PORTABLE_PHASES) - metrics.keys()
            missing_phases.update(missing)
            incomplete_records += bool(missing)
            boundaries = [metrics[key]['count'] for key in ('jni_execute', 'native_open') if key in metrics]
            if not boundaries:
                boundary_unmeasured += 1
            elif 'native_input_decode' not in metrics or metrics['native_input_decode']['count'] != sum(boundaries):
                # Every observed JNI entry/open should contribute one native
                # input decode. Keep genuine durations, but reject completeness
                # if a multi-entry API lost even one native summary.
                boundary_mismatched += 1
        result['missing_storage_phases'] = [phase for phase in PORTABLE_PHASES if phase in missing_phases or not storage]
        result['incomplete_storage_records'] = incomplete_records
        result['native_boundary_mismatched_records'] = boundary_mismatched
        result['native_boundary_unmeasured_records'] = boundary_unmeasured
        result['native_boundary_coverage'] = ('verified' if storage and not boundary_unmeasured and not boundary_mismatched else 'unverified')
        if storage and not incomplete_records and not boundary_mismatched:
            result['storage_coverage'] = 'verified'
        if result['storage_coverage'] == 'verified' and not result['missing_required_intervals']:
            result['coverage'] = 'verified'
        activities = [r for r in records if r.get('session') == begin['session'] and
                      r.get('operation') == 'ACTIVITY' and r.get('kind') == 'BEGIN' and
                      uint(r.get('observedNanos')) and r['observedNanos'] <= begin['observedNanos']]
        if len(activities) == 1:
            activity = [r for r in records if r.get('session') == begin['session'] and r.get('run') == activities[0]['run']]
            if all(r.get('dropped') == 0 for r in activity):
                breakdown = intervals(activity, STARTUP_PAIRS)
                if breakdown:
                    result['startup'] = {'coverage':'observed', 'condition':'earlier-same-process-startup-not-controller-coldstart', 'android_intervals_ms':breakdown}
        return result
    except (ValueError, TypeError, KeyError, OverflowError):
        return unknown


class Collector:
    def __init__(self, adb):
        self.adb, self.turns = adb, []
        # A metadata-only watermark excludes prior-boot journals whose boottime
        # values happen to collide. Read journal CONTENT only once after the suite.
        try:
            self.baseline = {(root, private, name):size for root, private, name, size in journal_files(adb)}
        except Exception:
            self.baseline = None

    def before(self, target, model, web):
        target['local_stages'] = {'coverage':'unverified', 'reason':'terminal_window_missing'}
        turn = [target, model, web, None, None]
        self.turns.append(turn)
        try: turn[3] = uptime(self.adb)
        except Exception: pass
        return turn

    def after(self, turn):
        try: turn[4] = uptime(self.adb)
        except Exception: pass

    def finish(self):
        try:
            if self.baseline is None:
                raise ValueError('journal_watermark_unavailable')
            records = read_journal(self.adb, self.baseline)
        except Exception:
            for target, *_ in self.turns:
                target['local_stages'] = {'coverage':'unverified', 'reason':'bounded_journal_unavailable'}
            self.turns.clear()
            return
        for target, model, web, before, after in self.turns:
            target['local_stages'] = summarize(records, (before, after), model, web, require_fresh=True)
        self.turns.clear()


def repeat_summary(rows):
    """Only equivalent case/seed positions; no sums of overlapping phases or tiny-N p95."""
    groups = {}
    for row in rows:
        for position, profile in [('followup', row)] + [('seed_' + str(p['seed_number']), p) for p in row.get('seed_profiles', [])]:
            stages = profile.get('local_stages', {})
            if stages.get('correlation') != 'verified':
                continue
            group = groups.setdefault(row['scenario'] + ':' + position, {})
            for phase, value in stages['android_intervals_ms'].items():
                group.setdefault('android:' + phase + ':ms', []).append(value)
            for phase, metrics in stages['storage']['metrics'].items():
                for metric, value in metrics.items():
                    group.setdefault('storage:' + phase + ':' + metric, []).append(value)
    return {group:{metric:{'samples':len(values), 'min':min(values), 'median':statistics.median(values), 'max':max(values)}
                   for metric, values in metrics.items() if len(values) > 1} for group, metrics in groups.items()}


def shape_report(report, budget):
    """Keep existing report budget, never discover oversize after live admissions."""
    def fits():
        return len(json.dumps(report, indent=2).encode()) <= budget
    if fits():
        return
    collection = report['stage_collection']
    collection.pop('repeat_summary', None)
    collection['detail_truncated'] = ['repeat_summary']
    collection['detail_coverage'] = 'unverified'
    if fits():
        return
    # Primary per-turn phase totals/counts are more valuable than repeated
    # startup/call-category detail. Maxima are optional in compact seed trends.
    for row in report['cases']:
        for profile in [row] + row.get('seed_profiles', []):
            stages = profile.get('local_stages', {})
            stages['detail_truncated'] = True
            stages.pop('startup', None)
            stages.get('storage', {}).pop('calls', None)
            for metric in stages.get('storage', {}).get('metrics', {}).values():
                metric.pop('max_ms', None)
    collection['detail_truncated'].append('startup-call-categories-and-maxima')
    if fits():
        return
    # Fail closed on unexpected future vocab growth, preserving host metrics,
    # admissions, checks, cleanup, and explicit missing timing coverage.
    for row in report['cases']:
        for profile in [row] + row.get('seed_profiles', []):
            if 'local_stages' in profile:
                profile['local_stages'] = {'coverage':'unverified', 'reason':'report_detail_budget', 'detail_truncated':True}
    collection['status'] = 'unverified'
    collection['detail_truncated'].append('local-stage-details')
    if report.get('status') == 'pass':
        report['status'] = 'unverified'
