"""Bounded local instrumentation transport, without speech or admission policy."""
import asyncio
import subprocess
import re

from benchmark import Blocked

MAX_OUTPUT_BYTES = 128 * 1024
TRANSPORT_REAP_SECONDS = 5


async def retire_transport(process, cleanup, reap_seconds=TRANSPORT_REAP_SECONDS):
    # Drain while reaping so pipe backpressure cannot keep Process.wait pending.
    async def discard():
        while await process.stdout.read(4096):
            await asyncio.sleep(0)
    drain = asyncio.create_task(discard())
    cleanup['local_transport'] = 'reap_unverified'
    try:
        if process.returncode is None:
            try:
                process.terminate()
            except ProcessLookupError:
                pass
        try:
            await asyncio.wait_for(process.wait(), reap_seconds)
        except TimeoutError:
            try:
                process.kill()
            except ProcessLookupError:
                pass
            await asyncio.wait_for(process.wait(), reap_seconds)
        cleanup['local_transport'] = 'reaped'
    except (TimeoutError, OSError):
        pass
    finally:
        drain.cancel()
        done, _ = await asyncio.wait({drain}, timeout=reap_seconds)
        if drain in done and not drain.cancelled():
            drain.exception()


class PhaseParser:
    """Incremental bounded framing; callbacks observe only terminated STATUS bundles."""
    def __init__(self, *, trial, on_trial=None):
        self.status, self.final = {}, {}
        self.phase = 'status' if trial else 'final'
        self.terminal_code = None
        self.pending = bytearray()
        self.size = 0
        self.on_trial = on_trial
        self.optional_trial = trial == 'optional'

    def line(self, raw):
        try:
            line = raw.decode('utf-8', errors='strict').removesuffix('\r')
            if line.startswith('INSTRUMENTATION_STATUS_CODE: '):
                if self.phase != 'status' or not self.status or line != 'INSTRUMENTATION_STATUS_CODE: 0':
                    raise ValueError()
                self.phase = 'final'
                if self.on_trial is not None:
                    self.on_trial(dict(self.status))
            elif line.startswith('INSTRUMENTATION_CODE: '):
                if self.phase != 'final' or not self.final or not re.fullmatch(r'INSTRUMENTATION_CODE: -?(?:0|[1-9][0-9]{0,8})', line):
                    raise ValueError()
                self.terminal_code = int(line.removeprefix('INSTRUMENTATION_CODE: '))
                self.phase = 'terminal'
            elif line.startswith(('INSTRUMENTATION_FAILED:', 'INSTRUMENTATION_ABORTED:')):
                raise ValueError()
            else:
                for prefix, destination, expected in (('INSTRUMENTATION_STATUS: ', self.status, 'status'),
                        ('INSTRUMENTATION_RESULT: ', self.final, 'final')):
                    if line.startswith(prefix):
                        if (self.optional_trial and prefix == 'INSTRUMENTATION_RESULT: ' and
                                self.phase == 'status' and not self.status):
                            self.phase = 'final'
                        if self.phase != expected:
                            raise ValueError()
                        key, value = line[len(prefix):].split('=', 1)
                        if key in destination:
                            raise ValueError()
                        destination[key] = value
        except (ValueError, UnicodeError):
            raise Blocked('synthetic_protocol_invalid') from None

    def feed(self, chunk):
        self.size += len(chunk)
        if self.size > MAX_OUTPUT_BYTES:
            raise Blocked('synthetic_output_limit')
        self.pending.extend(chunk)
        while b'\n' in self.pending:
            line, _, remaining = self.pending.partition(b'\n')
            self.pending = bytearray(remaining)
            self.line(line)

    def finish(self, *, require_terminal=True):
        if self.pending:
            self.line(bytes(self.pending))
            self.pending.clear()
        if self.phase != 'terminal' and (require_terminal or self.phase != 'final'):
            raise Blocked('synthetic_protocol_invalid')
        return self.status, self.final, self.terminal_code


def records(raw, *, trial, require_terminal=True):
    parser = PhaseParser(trial=trial)
    parser.feed(raw)
    return parser.finish(require_terminal=require_terminal)


def bundles(raw, returncode, *, trial):
    status, final, terminal_code = records(raw, trial=trial)
    if returncode != 0 or terminal_code != -1:
        raise Blocked('synthetic_protocol_invalid')
    return status, final


async def observe(adb, arguments, cleanup, timeout, admissions=None, *, retire=None, on_trial=None, trial=True, on_output=None):
    """Optional synchronous output hook runs after bounds checks, before STATUS parsing."""
    if admissions is not None:
        admissions.update(submitted=None, observed=None)
    deadline = asyncio.get_running_loop().time() + timeout
    async with asyncio.timeout_at(deadline):
        process = await asyncio.create_subprocess_exec(
            *adb.argv('shell', 'am', 'instrument', '-w', '-r', *arguments),
            stdin=subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE, stderr=subprocess.DEVNULL)
    cleanup['local_transport'] = 'owned'
    raw = bytearray()
    parser = PhaseParser(trial=trial, on_trial=on_trial) if on_trial is not None else None
    try:
        async with asyncio.timeout_at(deadline):
            while chunk := await process.stdout.read(4096):
                if len(raw) + len(chunk) > MAX_OUTPUT_BYTES:
                    raise Blocked('synthetic_output_limit')
                raw.extend(chunk)
                if on_output is not None:
                    on_output(chunk)
                if parser is not None:
                    parser.feed(chunk)
            await process.wait()
            if parser is not None:
                parser.finish()
        return bytes(raw), process.returncode
    finally:
        await (retire or retire_transport)(process, cleanup)
