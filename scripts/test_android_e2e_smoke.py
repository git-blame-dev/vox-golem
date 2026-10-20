import asyncio
import importlib.util
import json
import io
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('android_smoke', Path(__file__).with_name('android-e2e-smoke.py'))
smoke = importlib.util.module_from_spec(spec)
spec.loader.exec_module(smoke)


class FakeSocket:
    """In-memory CDP wire, not an ADB or WebView integration."""
    def __init__(self, states=(), sent=True):
        self.states = iter(states)
        self.sent = sent
        self.expressions = []

    async def send(self, raw):
        call = json.loads(raw)
        expression = call['params']['expression']
        self.expressions.append(expression)
        if expression == smoke.BASELINE:
            value = ['synthetic-old-request']
        elif expression == smoke.OBSERVE:
            value = next(self.states, {'admitted': False})
        elif 'button.click()' in expression:
            value = self.sent
        else:
            value = True
        self.response = json.dumps({'id': call['id'], 'result': {'result': {'value': value}}})

    async def recv(self):
        return self.response


def terminal(**changes):
    return dict(admitted=True, request='synthetic-new-request', outcome='completed',
                stage='completed', kind='text', nonblank=True, runtimeStatus='sleeping', **changes)


class SmokeTests(unittest.IsolatedAsyncioTestCase):
    async def run_ui(self, socket):
        return await smoke.exercise_ui(smoke.CDP(socket, .1), 'synthetic-model',
                                       'Synthetic question', turn_timeout=.02, poll_interval=.001)

    async def test_disabled_send_fails_without_polling_old_conversation(self):
        socket = FakeSocket([terminal()], sent=False)
        with self.assertRaisesRegex(RuntimeError, 'not admitted'):
            await self.run_ui(socket)
        self.assertNotIn(smoke.OBSERVE, socket.expressions)
        self.assertEqual(socket.expressions[-1], 'delete window.__voxSmoke')

    async def test_unrelated_dom_growth_and_sleeping_cannot_pass(self):
        socket = FakeSocket([{'admitted': False, 'sleeping': True, 'bodyGrew': True}])
        with self.assertRaises(TimeoutError):
            await self.run_ui(socket)
        self.assertFalse(any('document.body.innerText' in expression for expression in socket.expressions))

    async def test_correlated_success_reports_observed_answer_not_ttfb(self):
        state = terminal()
        state['outcome'] = 'pending'
        socket = FakeSocket([state, terminal()])
        metrics = await self.run_ui(socket)
        self.assertGreaterEqual(metrics['completed_ms'], metrics['first_answer_ms'])
        self.assertGreaterEqual(metrics['first_answer_ms'], 0)

    async def test_completed_answer_waits_for_shell_sleeping(self):
        executing = terminal()
        executing['runtimeStatus'] = 'executing'
        socket = FakeSocket([executing, terminal()])
        await self.run_ui(socket)
        self.assertEqual(socket.expressions.count(smoke.OBSERVE), 2)
        for status in ('executing', 'speaking', None):
            with self.subTest(status=status):
                state = terminal()
                state['runtimeStatus'] = status
                with self.assertRaises(TimeoutError):
                    await self.run_ui(FakeSocket([state] * 100))

    async def test_failure_refusal_and_missing_terminal_metadata_fail_closed(self):
        for changes in ({'stage': 'failed'}, {'outcome': 'error'}, {'outcome': 'cancelled'},
                        {'kind': 'refusal'}, {'kind': 'error'}, {'stage': None},
                        {'kind': None}, {'nonblank': False}):
            with self.subTest(changes=changes):
                state = terminal()
                state.update(changes)
                with self.assertRaises(RuntimeError):
                    await self.run_ui(FakeSocket([state]))

    async def test_changing_request_identity_fails(self):
        pending = terminal()
        pending['outcome'] = 'pending'
        changed = terminal()
        changed['request'] = 'synthetic-other-request'
        with self.assertRaisesRegex(RuntimeError, 'correlation'):
            await self.run_ui(FakeSocket([pending, changed]))

    async def test_optional_confirmation_precedes_send_and_terminal_observer_gets_correlation(self):
        socket = FakeSocket([terminal()])
        events = []
        async def ready():
            self.assertFalse(any('button.click()' in e for e in socket.expressions))
            events.append('ready')
        async def terminal_observer(request, metrics):
            self.assertEqual(request, 'synthetic-new-request')
            self.assertGreaterEqual(metrics['completed_ms'], metrics['first_answer_ms'])
            events.append('terminal')
        await smoke.exercise_ui(smoke.CDP(socket, .1), 'synthetic-model', 'Synthetic question',
                                turn_timeout=.02, poll_interval=.001,
                                confirm_ready=ready, terminal_observer=terminal_observer)
        self.assertEqual(events, ['ready', 'terminal'])

    async def test_confirmation_failure_never_submits_and_removes_probe(self):
        socket = FakeSocket([terminal()])
        async def ready():
            raise RuntimeError('synthetic configuration mismatch')
        with self.assertRaisesRegex(RuntimeError, 'configuration mismatch'):
            await smoke.exercise_ui(smoke.CDP(socket, .1), 'synthetic-model', 'Synthetic question',
                                    poll_interval=.001, confirm_ready=ready)
        self.assertFalse(any('button.click()' in e for e in socket.expressions))
        self.assertEqual(socket.expressions[-1], 'delete window.__voxSmoke')

    async def test_admission_callback_runs_before_failed_turn_raises(self):
        state = terminal()
        state['outcome'] = 'failed'
        socket = FakeSocket([state])
        events = []
        async def submitted():
            events.append('submitted')
        async def admitted(request):
            events.append(request)
        with self.assertRaisesRegex(RuntimeError, 'failed'):
            await smoke.exercise_ui(smoke.CDP(socket, .1), 'synthetic-model', 'Synthetic question',
                                    poll_interval=.001, submission_observer=submitted,
                                    admission_observer=admitted)
        self.assertEqual(events, ['submitted', 'synthetic-new-request'])

    async def test_unbounded_cdp_events_are_bounded_by_deadline(self):
        class Events(FakeSocket):
            async def recv(self):
                await asyncio.sleep(.001)
                return '{"method":"Synthetic.event"}'
        with self.assertRaises(TimeoutError):
            await smoke.CDP(Events(), .01).evaluate('synthetic')

    async def test_protocol_and_js_errors_omit_payload(self):
        for response in ({'id': 1, 'error': {'message': 'synthetic private content'}},
                         {'id': 1, 'result': {'exceptionDetails': {'text': 'synthetic private content'}}}):
            socket = FakeSocket()
            async def send(raw):
                socket.response = json.dumps(response)
            socket.send = send
            with self.assertRaisesRegex(RuntimeError, 'payload omitted'):
                await smoke.CDP(socket, .1).evaluate('synthetic')

    async def test_http_closed_and_socket_routed_through_same_observer(self):
        response = io.BytesIO(json.dumps([{'id': 'synthetic-target',
            'webSocketDebuggerUrl': 'ws://synthetic-advertised.invalid:4321/devtools/page/synthetic-target'}]).encode())
        routes = []
        socket = FakeSocket([terminal()])
        class Connection:
            async def __aenter__(self):
                return socket
            async def __aexit__(self, *args):
                routes.append('closed')
        def connect(url, **kwargs):
            routes.append((url, kwargs))
            return Connection()
        library = types.ModuleType('websockets')
        library.connect = connect
        with patch.dict(sys.modules, websockets=library), patch.object(smoke.urllib.request, 'urlopen', return_value=response) as http, patch('builtins.print') as output:
            await smoke.run_smoke(9222, 'synthetic-target', 'synthetic-model', 'Synthetic question', .1)
        http.assert_called_once_with('http://127.0.0.1:9222/json', timeout=.1)
        self.assertTrue(response.closed)
        self.assertEqual(routes[0], ('ws://127.0.0.1:9222/devtools/page/synthetic-target', {'open_timeout': .1, 'close_timeout': .1}))
        self.assertEqual(routes[-1], 'closed')
        printed = str(output.call_args_list)
        self.assertIn('Status: PASS', printed)
        self.assertNotIn('Synthetic question', printed)
        self.assertNotIn('First Byte', printed)


class ForwardTests(unittest.TestCase):
    def test_owned_forward_is_removed_after_failure(self):
        calls = []
        def run(argv, **kwargs):
            calls.append(argv)
            self.assertGreater(kwargs['timeout'], 0)
            return type('Result', (), {'stdout': '123\n'})()
        with patch.object(smoke.subprocess, 'run', side_effect=run):
            with self.assertRaisesRegex(RuntimeError, 'synthetic failure'):
                with smoke.port_forward('synthetic-adb', 'synthetic-device', 9222):
                    raise RuntimeError('synthetic failure')
        self.assertIn('--no-rebind', calls[1])
        self.assertEqual(calls[-1][-3:], ['forward', '--remove', 'tcp:9222'])

    def test_failed_forward_does_not_remove_someone_elses_forward(self):
        with patch.object(smoke.subprocess, 'run', side_effect=[type('Result', (), {'stdout': '123'})(), RuntimeError('busy')]) as run:
            with self.assertRaises(RuntimeError):
                with smoke.port_forward('synthetic-adb', 'synthetic-device', 9222):
                    self.fail('Forward should not be acquired')
            self.assertEqual(run.call_count, 2)


if __name__ == '__main__':
    unittest.main()
