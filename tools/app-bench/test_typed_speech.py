"""Synthetic Windows typed-speech diagnostics; no app, service or device access."""
import copy
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch, MagicMock

import runner
import windows_effects as windows
from fixtures import arithmetic_profile, FakeServices
import test_windows_effects as windows_tests


def descriptor():
    return dict(protocol_version=1, ready=True, backend='synthetic', default_voice='synthetic-a',
        voices=[dict(id='synthetic-a', name='Synthetic A', aliases=['synthetic-alias'])],
        sample_rate_hz=24000, channels=1, incremental_audio=False)


def config(path, endpoint='http://127.0.0.1:19765', extra=''):
    path.write_text('[tts]\nendpoint='+json.dumps(endpoint)+
                    '\nvoice="synthetic-a"\nstart_command=[]\n'+extra)
    return path


class TypedSpeechTests(unittest.TestCase):
    def test_prepare_and_environment_keep_windows_ownership_without_speech_assets(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            manifest, value = windows_tests.WindowsEffectsTests().staged_bundle(root)
            value.update(schema=2, entry='typed-speech-windows')
            value['roles'] = {k:v for k,v in value['roles'].items()
                              if k in ('binary', 'tauri_driver', 'native_driver', 'config')}
            config(root/'speech.toml')
            value['files'] = {name:windows.file_hash(root/name) for name in value['roles'].values()}
            manifest.write_text(json.dumps(value))
            args = SimpleNamespace(entry='typed-speech', live=False, _test_win32=True,
                cases=['arithmetic'], windows_manifest=manifest, output=root/'private-run',
                binary=root/value['roles']['binary'], config=root/'speech.toml', tools=root/'drivers')
            effects = windows.WindowsEffects(args)
            def private(path, create=True):
                if create: path.mkdir()
            identity = windows.neural_descriptor(descriptor(), 'synthetic-a')
            with patch.object(windows, 'private_directory', side_effect=private), \
                    patch.object(windows, 'verify_native_driver_version', return_value='157.0.4341.42'), \
                    patch.object(windows, 'probe_neural_service', return_value=identity) as probe, \
                    patch.dict(windows.os.environ, {'SystemRoot':str(root/'SyntheticWindows')}, clear=True):
                fixtures, speech, profile = effects.prepare(arithmetic_profile(), runner)
                self.assertEqual(fixtures, {})
                self.assertEqual(speech, identity)
                self.assertEqual(profile['profile'], 'typed-speech-windows-diagnostic-v1')
                self.assertEqual(profile['external_shared_service_lifecycle'], 'unverified')
                env = effects.environment('http://127.0.0.1:1/responses', runner)
            probe.assert_called_once()
            self.assertEqual(env['VOXGOLEM_BENCH_ENTRY'], 'typed-speech-windows')
            self.assertNotIn('PIPER_ESPEAKNG_DATA_DIRECTORY', env)
            self.assertEqual(env['WEBVIEW2_USER_DATA_FOLDER'], str(args.output/'webview'))
            self.assertTrue(effects.private_storage_verified)

    def test_manifest_two_roles_hashes_siblings_and_synthetic_config(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            manifest, value = windows_tests.WindowsEffectsTests().staged_bundle(root)
            value.update(schema=2, entry='typed-speech-windows')
            value['roles'] = {k:v for k,v in value['roles'].items()
                              if k in ('binary', 'tauri_driver', 'native_driver', 'config')}
            config(root/'speech.toml')
            value['files']['speech.toml'] = windows.file_hash(root/'speech.toml')
            manifest.write_text(json.dumps(value))
            loaded, roles = windows.load_manifest(manifest, arithmetic_profile(), 'typed-speech')
            self.assertEqual(set(roles), {'binary', 'tauri_driver', 'native_driver', 'config'})
            with self.assertRaises(ValueError): windows.load_manifest(manifest, arithmetic_profile(), 'voice-arithmetic')
            for update in ({'schema':True}, {'entry':'voice-arithmetic-windows'},
                           {'roles':dict(value['roles'], tts_model='assets/tts.onnx')},
                           {'contract_sha256':'0'*64}):
                manifest.write_text(json.dumps(dict(value, **update)))
                with self.assertRaises(ValueError): windows.load_manifest(manifest, arithmetic_profile(), 'typed-speech')
            manifest.write_text(json.dumps(value))
            (root/'app/undeclared.dll').write_bytes(b'synthetic')
            with self.assertRaises(ValueError): windows.load_manifest(manifest, arithmetic_profile(), 'typed-speech')

    def test_configuration_refuses_nonliteral_remote_credentials_commands_and_assets(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)/'synthetic.toml'
            self.assertEqual(set(windows.neural_configuration(config(path))), {'endpoint', 'voice', 'start_command'})
            for endpoint in ('https://127.0.0.1:19765', 'http://localhost:19765',
                             'http://192.0.2.1:19765', 'http://synthetic@127.0.0.1:19765',
                             'http://127.0.0.1:19765/path', 'http://127.0.0.1:19765?',
                             'http://127.0.0.1:19765#', 'http://127.0.0.1'):
                with self.subTest(endpoint=endpoint), self.assertRaises(ValueError):
                    windows.neural_configuration(config(path, endpoint))
            for extra in ('provider="neural"', 'model_path="synthetic.onnx"', '[custom_openai]\nendpoint="synthetic"'):
                with self.assertRaises(ValueError): windows.neural_configuration(config(path, extra=extra))
            config(path)
            path.write_text(path.read_text().replace('start_command=[]', 'start_command=["synthetic"]'))
            with self.assertRaises(ValueError): windows.neural_configuration(path)

    def test_descriptor_identity_alias_and_guard_failures(self):
        expected = dict(engine='neural', protocol_version=1, backend='synthetic', voice='synthetic-a',
                        sample_rate_hz=24000, incremental_audio=False)
        self.assertEqual(windows.neural_descriptor(descriptor(), 'synthetic-alias'), expected)
        self.assertTrue(runner.speech_identity_matches(expected.copy(), expected))
        for update in ({'engine':'piper'}, {'voice':'synthetic-other'}, {'protocol_version':True},
                       {'incremental_audio':0}, {'sample_rate_hz':22050}, {'extra':True}):
            self.assertFalse(runner.speech_identity_matches(dict(expected, **update), expected))
        self.assertFalse(runner.speech_identity_matches(None, expected))
        for update in ({'ready':False}, {'protocol_version':True}, {'sample_rate_hz':True},
                       {'sample_rate_hz':96001}, {'channels':2}, {'incremental_audio':1},
                       {'voices':[]}, {'default_voice':'missing'}, {'backend':'bad/value'}):
            with self.subTest(update=update), self.assertRaises(ValueError):
                windows.neural_descriptor(dict(descriptor(), **update), 'synthetic-a')
        for voice in [dict(id='synthetic-a', name='Synthetic', aliases=['synthetic-a']),
                      dict(id='synthetic-a', name='\nSynthetic'),
                      dict(id='synthetic-a', name='Synthetic', aliases=['x']*33)]:
            with self.assertRaises(ValueError):
                windows.neural_descriptor(dict(descriptor(), voices=[voice]), 'synthetic-a')
        with self.assertRaises(ValueError): windows.neural_descriptor(descriptor(), 'missing')

    def test_probe_is_bounded_get_only_no_proxy_or_redirect(self):
        response = MagicMock()
        response.__enter__.return_value = response
        response.status = 200
        response.read.return_value = json.dumps(descriptor()).encode()
        opener = MagicMock()
        opener.open.return_value = response
        with patch.object(windows.urllib.request, 'build_opener', return_value=opener) as build:
            identity = windows.probe_neural_service(dict(endpoint='http://127.0.0.1:19765', voice='synthetic-a'))
            self.assertEqual(identity['voice'], 'synthetic-a')
            self.assertEqual(build.call_args.args[0].proxies, {})
            self.assertIsNone(build.call_args.args[1].redirect_request(None, None, None, None, None, None))
            opener.open.assert_called_once_with('http://127.0.0.1:19765/v1/synthesis/capabilities', timeout=5)
            response.read.assert_called_once_with(65537)
            response.read.return_value = b' ' * 65537
            with self.assertRaises(ValueError):
                windows.probe_neural_service(dict(endpoint='http://127.0.0.1:19765', voice='synthetic-a'))

    def test_windows_options_forbid_live_assets_and_foreign_modes(self):
        base = dict(entry='typed-speech', cases=['arithmetic'], windows_manifest=Path('/synthetic/manifest'))
        windows.validate_options(SimpleNamespace(**base), 'win32')
        for update in ({'live':True}, {'wake_phrase':'synthetic'},
                       {'cases':['quick']}, {'shared_suite':True}, {'extended':True}, {'poll_interval':0.2}):
            with self.assertRaises(ValueError): windows.validate_options(SimpleNamespace(**dict(base, **update)), 'win32')
        with self.assertRaises(ValueError): windows.validate_options(SimpleNamespace(**base), 'linux')

    def test_generated_environment_uses_neural_without_capture_assets(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root/'run').mkdir()
            source = config(root/'source.toml')
            env = runner.isolated_environment(root/'run', source, 'http://127.0.0.1:1/responses',
                entry='typed-speech', output_route='windows-diagnostic', base_environment={})
            import tomllib
            generated = tomllib.loads((root/'run/config/config.toml').read_text())
            self.assertEqual(generated['tts']['start_command'], [])
            self.assertNotIn('provider', generated['tts'])
            self.assertNotIn('wake_word_model_path', generated)
            self.assertFalse(generated['web_enabled'])
            self.assertEqual(env['VOXGOLEM_BENCH_MODE'], 'offline')
            self.assertNotIn('VOXGOLEM_BENCH_PERMITS', env)
            self.assertIn('tts_enabled = true', (root/'run/config/state.toml').read_text())
            for kwargs in ({'live':True}, {'reservation':root/'synthetic-reservation'}):
                with self.assertRaises(ValueError):
                    runner.isolated_environment(root/'run', source, 'http://127.0.0.1:1/responses',
                                                entry='typed-speech', **kwargs)

    def test_same_native_clock_missing_ambiguous_reversed_or_reordered_stages(self):
        names = ['prompt_command_started', 'first_model_text', 'reply_synthesis_requested',
                 'reply_synthesis_valid', 'reply_output_submitted', 'reply_output_activated']
        events = [dict(stage=name, elapsed_us=i*1000, round=1) for i,name in enumerate(names)]
        result = runner.stage_breakdown(events)
        self.assertEqual(result['prompt_to_first_model_text_ms'], 1)
        self.assertEqual(result['prompt_to_output_activated_ms'], 5)
        self.assertEqual(result['synthesis_request_to_valid_pcm_ms'], 1)
        for bad in (events[:3], events + [events[3]], list(reversed(events)),
                    [dict(e, elapsed_us=-1) if e['stage']=='reply_synthesis_valid' else e for e in events]):
            self.assertIsNone(runner.stage_breakdown(bad)['synthesis_request_to_valid_pcm_ms'])

    def test_real_observer_word_boundary_terminal_and_same_clock_receipt(self):
        node = shutil.which('node')
        if not node: self.skipTest('Node required for injected observer execution')
        script = '''const assert=require('node:assert/strict');
global.window={};let now=0,submit,mutation,textValue='',outcome='streaming';
global.performance={now:()=>++now};
const text={get textContent(){return textValue},querySelector:()=>null};
const user={dataset:{requestId:'synthetic'}};
const row={dataset:{requestId:'synthetic',get turnOutcome(){return outcome}},querySelector:()=>text};
global.document={body:{},addEventListener:(name,fn)=>{submit=fn},removeEventListener:()=>{},
 querySelectorAll:selector=>selector.includes('user')?[user]:[row]};
global.MutationObserver=class{constructor(fn){mutation=fn}observe(){}disconnect(){}};
const install=new Function('''+json.dumps(runner.INSTALL_TYPED_OBSERVER)+''');
install([]);submit();textValue='  Syn';mutation();
assert.equal(window.__voxBenchObserve.read().first_word_ms,null);
const first=window.__voxBenchObserve.read().first_text_ms;
textValue='  Synthetic ';mutation();
assert.ok(window.__voxBenchObserve.read().first_word_ms>first);
window.__voxBenchObserve.speech({events:[{stage:'reply_output_submitted',elapsed_us:999999}]});
assert.equal(window.__voxBenchObserve.read().speech_observation_ms,null);
window.__voxBenchObserve.speech({events:[{stage:'reply_output_activated',elapsed_us:999999}]});
const receipt=window.__voxBenchObserve.read().speech_observation_ms;assert.ok(receipt<100);
window.__voxBenchObserve.speech({events:[{stage:'reply_output_activated'}]});
assert.equal(window.__voxBenchObserve.read().speech_observation_ms,receipt);
install([]);submit();textValue='210';mutation();assert.equal(window.__voxBenchObserve.read().first_word_ms,null);
outcome='completed';mutation();assert.ok(window.__voxBenchObserve.read().first_word_ms!==null);
'''
        result = subprocess.run([node, '-e', script], capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr.decode())

    def test_exercise_dispatches_shared_profile_and_waits_for_local_retirement(self):
        profile = arithmetic_profile()
        events = [dict(stage=s, elapsed_us=i*1000, round=1) for i,s in enumerate(
            ['prompt_command_started', 'prompt_admitted', 'generation_dispatched', 'first_model_text',
             'canonical_turn_committed', 'reply_synthesis_requested', 'reply_synthesis_valid',
             'reply_output_submitted', 'reply_output_activated', 'reply_output_completed'])]
        speech = dict(admitted=1, synthesis_valid=1, output_submitted=1, output_completed=1, failed=0, retired=True)
        native = dict(events=events, dropped=0, readable_pages=0, tool_failures=0, generation={}, reply_speech=speech)
        turn = dict(request_id='synthetic', answer=dict(content=dict(text=profile.answer)))
        observation = dict(users=1, inflight_text=True, submit_ms=10, first_text_ms=20,
                           first_word_ms=30, terminal_ms=30, speech_observation_ms=40)
        class Driver:
            def execute_script(self, script, *args):
                if script == runner.OBSERVE: return dict(outcome='completed', stage='completed', kind='text')
                if 'return {text:' in script: return dict(text=profile.answer, urls=[])
                if 'Observe.read()' in script: return observation
                return []
            def find_element(self, *args): return SimpleNamespace(click=lambda:None)
        calls = []
        def invoke(driver, command, args=None, observer=False):
            calls.append((command, observer))
            if command == 'get_app_snapshot': return dict(transcript=[] if len(calls)==1 else [turn])
            if command == 'benchmark_snapshot':
                result = copy.deepcopy(native)
                result['reply_speech']['retired'] = sum(c == ('benchmark_snapshot', True) for c in calls) >= 3
                return result
        service = FakeServices(profile=profile, gated_stream=True)
        service.snapshot = lambda: dict(generation_requests=1, expected_model_rounds=1,
            searches=0, expected_searches=0, fetches=0, expected_fetches=0,
            readable_sources=0, expected_readable_sources=0, failures=0, terminal=True,
            model_matches=True, transcript_matches=True, history_matches=True, stream_gate_timed_out=False)
        with patch.object(runner, 'invoke', side_effect=invoke), patch.object(runner, 'fill_composer') as fill:
            result = runner.exercise_typed(Driver(), 'arithmetic', service, profile=profile)
        fill.assert_called_once_with(unittest.mock.ANY, profile.cases['arithmetic'])
        self.assertTrue(result['workflow_completed'], result['checks'])
        self.assertEqual(result['ui_submit_to_first_text_dom_ms'], 10)
        self.assertEqual(result['ui_submit_to_first_complete_word_dom_ms'], 20)
        self.assertEqual(result['ui_submit_to_speech_observation_ms'], 30)
        self.assertTrue(result['checks']['local_speech_completed_and_retired'])
        self.assertIn(('benchmark_snapshot', True), calls)
