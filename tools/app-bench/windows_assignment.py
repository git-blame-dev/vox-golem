"""WSL authority for Windows assignments; never creates a Windows budget ledger.

Also embedded in the native claimant with paths.py. Only stdlib is used by claim.
Reservation imports the existing controller and its canonical assignment ceiling.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys

if 'artifact_root' not in globals():
    from paths import artifact_root


def unique(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError('Duplicate assignment field')
        value[key] = item
    return value


def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()


def open_private(path, write=False, limit=8192):
    for part in (path, *path.parents):
        if part.is_symlink():
            raise ValueError('Assignment symlink refused')
    descriptor = os.open(path, (os.O_RDWR if write else os.O_RDONLY) | os.O_NOFOLLOW)
    try:
        metadata = os.fstat(descriptor)
        named = path.lstat()
        if (not stat.S_ISREG(metadata.st_mode) or metadata.st_size > limit or
                metadata.st_mode & 0o777 != 0o600 or metadata.st_uid != os.getuid() or
                metadata.st_nlink != 1 or (named.st_dev, named.st_ino) != (metadata.st_dev, metadata.st_ino)):
            raise ValueError('Assignment storage is not private')
        return os.fdopen(descriptor, 'r+' if write else 'r')
    except BaseException:
        os.close(descriptor)
        raise


def read_private(path, limit=8192):
    with open_private(path, limit=limit) as stream:
        raw = stream.read(limit+1)
        if len(raw) > limit:
            raise ValueError('Assignment storage oversized')
        return json.loads(raw, object_pairs_hook=unique)


def location(source, grant):
    if sys.platform != 'linux' or not re.fullmatch('[0-9a-f]{32}', grant):
        raise ValueError('Canonical WSL assignment required')
    source = Path(source)
    if not source.is_absolute() or source.resolve(strict=True) != source:
        raise ValueError('Canonical source root required')
    root = artifact_root(source)
    for part in (root, *root.parents):
        if part.is_symlink():
            raise ValueError('Canonical storage symlink refused')
    return root/'unified-app-bench-live-budget.json', root/'windows-assignments'/f'{grant}.json'


def reserve(source, grant, assignment):
    import runner
    ledger, reservation = location(source, grant)
    folder = reservation.parent
    folder.mkdir(mode=0o700, exist_ok=True)
    if folder.stat().st_mode & 0o077 or folder.stat().st_uid != os.getuid():
        raise ValueError('Assignment directory is not private')
    descriptor = os.open(folder.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    if ledger.exists():
        with open_private(ledger):
            pass
    if (set(assignment) != {'binding', 'windows_run'} or
            not isinstance(assignment['binding'], dict) or
            assignment['binding'].get('harness_sha256') != runner.windows_harness_identity() or
            not isinstance(assignment['windows_run'], str) or len(assignment['windows_run']) > 4096):
        raise ValueError('Assignment does not match the canonical harness')
    return runner.reserve_live_budget(ledger, reservation, assignment=(grant, assignment))


def claim(source, grant, executable, windows_run, binding):
    import fcntl
    ledger, reservation = location(source, grant)
    value = read_private(reservation)
    if (set(value) != {'schema', 'allowance', 'binding', 'windows_run'} or
            value['schema'] != 2 or type(value['allowance']) is not int or value['allowance'] != 12 or
            value['binding'] != binding or value['windows_run'] != windows_run or
            reservation.parent.stat().st_mode & 0o077):
        raise ValueError('Assignment binding mismatch')
    # WSL resolves the actual executable supplied by native current_exe(), never
    # a controller-provided hash standing in for the launched executable.
    mapped = subprocess.run(['/usr/bin/wslpath', '-u', executable], check=True,
        capture_output=True, text=True, timeout=10).stdout.strip()
    binary = Path(mapped)
    if not binary.is_file() or binary.stat().st_size > 256*1024*1024:
        raise ValueError('Assignment executable unavailable')
    with binary.open('rb') as stream:
        actual = hashlib.file_digest(stream, 'sha256').hexdigest()
    if actual != binding.get('binary_sha256'):
        raise ValueError('Assignment executable mismatch')
    with open_private(ledger, write=True) as stream:
        fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
        raw = stream.read(8193)
        if len(raw) > 8192:
            raise ValueError('Canonical ledger oversized')
        budget = json.loads(raw, object_pairs_hook=unique)
        if (budget.get('schema') != 1 or type(budget.get('reserved')) is not int or
                not 12 <= budget['reserved'] <= 100 or
                budget.get('windows_assignments', {}).get(grant) != digest(value)):
            raise ValueError('Assignment is not registered in the canonical ledger')
        # This marker is in WSL canonical storage, so copying/recreating the
        # Windows handoff or run cannot replay it. Never delete or refund it.
        marker = reservation.with_suffix('.claimed')
        with open(marker, 'x', opener=lambda p, flags: os.open(p, flags, 0o600)) as claimed:
            claimed.flush()
            os.fsync(claimed.fileno())
        descriptor = os.open(marker.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)
    return value['allowance']


def main():
    try:
        operation, source, grant = sys.argv[1:4]
        raw = sys.argv[6].encode() if operation == 'claim' and len(sys.argv) == 7 else sys.stdin.buffer.read(16385)
        if len(raw) > 16384:
            raise ValueError('Assignment input oversized')
        payload = json.loads(raw, object_pairs_hook=unique)
        if operation == 'reserve' and len(sys.argv) == 4:
            # Source is an explicitly selected, trusted checkout, not PATH or
            # a Windows staging copy. Native claim never imports its code.
            sys.path.insert(0, source+'/tools/app-bench')
            result = {'reserved': reserve(source, grant, payload)}
        elif operation == 'claim' and len(sys.argv) == 7:
            result = {'allowance': claim(source, grant, sys.argv[4], sys.argv[5], payload)}
        else:
            raise ValueError('Unknown assignment operation')
        print(json.dumps(result))
    except Exception:
        print('{"blocked":true}')
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
