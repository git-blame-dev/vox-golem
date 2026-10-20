#!/usr/bin/env python3
"""Observe a UI-admitted Android turn without reading existing conversation text.

Requires transcript rows with data-request-id, data-turn-role, data-turn-outcome,
data-stage-status and data-content-kind, and a .message__content or
.answer-stage__answer child. Missing observation metadata fails closed.
Uses an explicitly selected local ADB transport; no remote routing is inferred.
Live CDP execution requires Python 3 and the existing third-party Python
`websockets` module exposing connect(open_timeout=..., close_timeout=...).
Standard-library fake-CDP tests do not require that module.
"""
import argparse
import asyncio
from contextlib import contextmanager
import json
import math
import subprocess
import time
import urllib.request


@contextmanager
def port_forward(adb, device, port, timeout=10):
    def command(*args):
        return subprocess.run([adb, '-s', device, *args], check=True,
                              capture_output=True, text=True, timeout=timeout)
    pid = command('shell', 'pidof', 'dev.gitblame.voxgolem.android').stdout.strip()
    if not pid.isdecimal():
        raise RuntimeError('Expected exactly one running app process')
    command('forward', '--no-rebind', f'tcp:{port}', f'localabstract:webview_devtools_remote_{pid}')
    try:
        yield
    finally:
        command('forward', '--remove', f'tcp:{port}')


class CDP:
    def __init__(self, ws, timeout):
        self.ws, self.timeout, self.counter = ws, timeout, 0

    async def evaluate(self, expression):
        async def exchange():
            self.counter += 1
            call_id = self.counter
            await self.ws.send(json.dumps({'id': call_id, 'method': 'Runtime.evaluate',
                                          'params': {'expression': expression, 'returnByValue': True,
                                                     'awaitPromise': True}}))
            while True:
                message = json.loads(await self.ws.recv())
                if message.get('id') != call_id:
                    continue
                result = message.get('result', {})
                if 'error' in message or 'exceptionDetails' in result:
                    raise RuntimeError('CDP evaluation failed (payload omitted)')
                return result.get('result', {}).get('value')
        return await asyncio.wait_for(exchange(), self.timeout)


BASELINE = "Array.from(document.querySelectorAll('[data-turn-role=\"user\"][data-request-id]')).map(e => e.dataset.requestId)"
OBSERVE = """(() => {
 const s = window.__voxSmoke;
 const runtimeStatus = document.querySelector('.shell[data-runtime-status]')?.getAttribute('data-runtime-status');
 const users = Array.from(document.querySelectorAll('[data-turn-role="user"][data-request-id]'))
   .filter(e => !s.ids.includes(e.dataset.requestId));
 if (users.length !== 1) return {admitted: false};
 const submitted = users[0].querySelector('.message__content');
 if (submitted?.textContent !== s.prompt) return {admitted: false};
 const id = users[0].dataset.requestId;
 const answers = Array.from(document.querySelectorAll('[data-turn-role="assistant"][data-request-id]'))
   .filter(e => e.dataset.requestId === id);
 if (answers.length !== 1) return {admitted: true, request: id};
 const row = answers[0];
 const text = row.querySelector('.answer-stage__answer, .message__content');
 return {admitted: true, request: id, outcome: row.dataset.turnOutcome,
   stage: row.dataset.stageStatus, kind: row.dataset.contentKind, runtimeStatus,
   nonblank: !!text?.textContent?.trim() && !text.querySelector('.message__thinking')};
})()"""


async def exercise_ui(cdp, model, prompt, turn_timeout=30, poll_interval=0.1,
                      confirm_ready=None, terminal_observer=None, admission_observer=None,
                      submission_observer=None):
    evaluate = cdp.evaluate
    ids = await evaluate(BASELINE)
    if not isinstance(ids, list):
        raise RuntimeError('Transcript observation contract unavailable')
    try:
        ready = await evaluate(f"""(() => {{
 const select = document.getElementById('composerInstantSelect');
 const option = Array.from(select?.options || []).find(o => o.value === {json.dumps(model)} && !o.disabled);
 if (!option) return false;
 select.value = option.value;
 select.dispatchEvent(new Event('change', {{bubbles: true}}));
 window.__voxSmoke = {{ids: {json.dumps(ids)}, prompt: {json.dumps(prompt)}}};
 const ta = document.getElementById('promptComposer');
 if (!ta) return false;
 Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set.call(ta, {json.dumps(prompt)});
 ta.dispatchEvent(new Event('input', {{bubbles: true}}));
 return true;
}})()""")
        if ready is not True:
            raise RuntimeError('Composer or selected model unavailable')
        await asyncio.sleep(poll_interval)
        if confirm_ready is not None:
            await confirm_ready()
        started = time.perf_counter()
        sent = await evaluate("""(() => {
 const ta = document.getElementById('promptComposer');
 const button = ta?.closest('form')?.querySelector('button[type="submit"]');
 if (!button || button.disabled) return false;
 button.click(); return true;
})()""")
        if sent is not True:
            raise RuntimeError('UI submission not admitted: Send unavailable')
        if submission_observer is not None:
            await submission_observer()
        first = None
        request = None
        while time.perf_counter() - started < turn_timeout:
            state = await evaluate(OBSERVE)
            now = time.perf_counter()
            if not isinstance(state, dict):
                raise RuntimeError('Invalid turn observation')
            if state.get('admitted'):
                current = state.get('request')
                if not current or (request is not None and current != request):
                    raise RuntimeError('Turn correlation changed')
                if request is None and admission_observer is not None:
                    await admission_observer(current)
                request = current
                if state.get('outcome') in ('error', 'failed', 'cancelled', 'refused', 'refusal') or state.get('stage') == 'failed' or state.get('kind') in ('error', 'refusal'):
                    raise RuntimeError('New turn failed or was refused')
                if state.get('nonblank') is True and state.get('kind') == 'text' and first is None:
                    first = now
                if state.get('outcome') == 'completed':
                    if first is None or state.get('stage') != 'completed' or state.get('kind') != 'text':
                        raise RuntimeError('Terminal turn lacks a successful text answer')
                    if state.get('runtimeStatus') == 'sleeping':
                        metrics = {'first_answer_ms': (first - started) * 1000,
                                   'completed_ms': (now - started) * 1000}
                        if terminal_observer is not None:
                            await terminal_observer(request, metrics)
                        return metrics
            await asyncio.sleep(poll_interval)
        raise TimeoutError('No correlated successful terminal answer before deadline')
    finally:
        await evaluate('delete window.__voxSmoke')


async def run_smoke(port, target_id, model, prompt, timeout):
    # Existing runtime dependency is deliberately lazy: unit tests need no library.
    try:
        import websockets
    except ModuleNotFoundError as error:
        if error.name != 'websockets':
            raise
        raise RuntimeError('Live CDP execution requires the Python websockets module '
                           'in this interpreter; no dependency installation is performed.') from None
    with urllib.request.urlopen(f'http://127.0.0.1:{port}/json', timeout=timeout) as response:
        targets = json.load(response)
    matches = [target for target in targets if target.get('id') == target_id]
    if len(matches) != 1:
        raise RuntimeError('Explicit DevTools target not found uniquely')
    # Route the advertised socket through the same local observer as ADB forwarding.
    from urllib.parse import urlsplit, urlunsplit
    advertised = urlsplit(matches[0]['webSocketDebuggerUrl'])
    if advertised.scheme != 'ws':
        raise RuntimeError('Expected local WebView websocket')
    url = urlunsplit(('ws', f'127.0.0.1:{port}', advertised.path, advertised.query, ''))
    async with websockets.connect(url, open_timeout=timeout, close_timeout=timeout) as ws:
        metrics = await exercise_ui(CDP(ws, timeout), model, prompt)
    print('Status: PASS')
    print(f"First new answer observed: {metrics['first_answer_ms']:.1f} ms")
    print(f"Successful terminal answer observed: {metrics['completed_ms']:.1f} ms")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--adb', required=True, help='Local ADB executable')
    parser.add_argument('--device', required=True, help='Explicit local ADB transport')
    parser.add_argument('--port', required=True, type=int)
    parser.add_argument('--target-id', required=True, help='Explicit DevTools target ID')
    parser.add_argument('--model', required=True)
    parser.add_argument('--prompt', default='What is three plus five? Reply only with the number.')
    parser.add_argument('--timeout', type=float, default=10)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535 or not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error('Port must be 1..65535 and timeout must be positive')
    with port_forward(args.adb, args.device, args.port, args.timeout):
        asyncio.run(run_smoke(args.port, args.target_id, args.model, args.prompt, args.timeout))


if __name__ == '__main__':
    main()
