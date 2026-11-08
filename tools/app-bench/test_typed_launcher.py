"""Launcher failure ownership with synthetic drivers and no process/network access."""
from pathlib import Path
import json
from types import SimpleNamespace
import sys
import types
import unittest
from unittest.mock import Mock, patch

import runner


class TypedLauncherTests(unittest.TestCase):
    def test_windows_startup_readiness_and_preference_failures_retire_once_and_preserve_error(self):
        class Options:
            def __init__(self): self._caps = {}
            def set_capability(self, key, value): self._caps[key] = value
        driver = SimpleNamespace(set_script_timeout=Mock(), execute_script=lambda *args: 'custom-luna-none')
        job = object()  # A Windows Job has no subprocess.poll() interface.
        for stage in ('startup', 'readiness', 'preferences'):
            for broken_diagnostic in (False, True):
                error = RuntimeError('synthetic '+stage+' failure')
                platform = SimpleNamespace(start=Mock(return_value=job),
                    application_options=lambda: {}, observe_app=Mock(), shutdown=Mock())
                remote = Mock(side_effect=error) if stage == 'startup' else Mock(return_value=driver)
                selenium = types.ModuleType('selenium')
                selenium.webdriver = SimpleNamespace(Remote=remote)
                modules = {'selenium':selenium, 'selenium.webdriver':selenium.webdriver,
                    'selenium.webdriver.common':types.ModuleType('selenium.webdriver.common'),
                    'selenium.webdriver.common.options':SimpleNamespace(BaseOptions=Options)}
                def wait(predicate, timeout, name):
                    if stage == 'readiness' and name == 'typed_model_ready_without_capture': raise error
                    return predicate()
                def invoke(_driver, command):
                    if command == 'get_app_snapshot': raise error
                    return dict(entry='typed-qualified-windows', capture_active=False, phase='sleeping')
                with self.subTest(stage=stage, broken_diagnostic=broken_diagnostic), \
                        patch.dict(sys.modules, modules), patch.object(runner, 'free_port', return_value=32123), \
                        patch.object(runner, 'can_connect', return_value=True), patch.object(runner, 'wait_for', side_effect=wait), \
                        patch.object(runner, 'invoke', side_effect=invoke), \
                        patch.object(runner, 'print', create=True, side_effect=RuntimeError('synthetic print failure') if broken_diagnostic else None) as diagnostic:
                    with self.assertRaises(RuntimeError) as raised:
                        runner.start_typed_app(SimpleNamespace(), {}, platform)
                    self.assertIs(raised.exception, error)
                    platform.shutdown.assert_called_once_with(None if stage == 'startup' else driver, job)
                    if not broken_diagnostic:
                        receipt = json.loads(diagnostic.call_args.args[0])
                        self.assertFalse(receipt['driver_status_observed'])
                        self.assertIsNone(receipt['driver_exit_code'])

    def test_status_and_cleanup_failures_cannot_replace_startup_error(self):
        class Options:
            def __init__(self): self._caps = {}
            def set_capability(self, key, value): self._caps[key] = value
        error = RuntimeError('synthetic original startup failure')
        selenium = types.ModuleType('selenium')
        selenium.webdriver = SimpleNamespace(Remote=Mock(side_effect=error))
        modules = {'selenium':selenium, 'selenium.webdriver':selenium.webdriver,
            'selenium.webdriver.common':types.ModuleType('selenium.webdriver.common'),
            'selenium.webdriver.common.options':SimpleNamespace(BaseOptions=Options)}
        process = SimpleNamespace(poll=Mock(side_effect=RuntimeError('synthetic status failure')))
        with patch.dict(sys.modules, modules), patch.object(runner, 'free_port', return_value=32123), \
                patch.object(runner, 'wait_for'), patch.object(runner.subprocess, 'Popen', return_value=process), \
                patch.object(runner, 'shutdown_owned', side_effect=RuntimeError('synthetic cleanup failure')) as shutdown, \
                patch.object(runner, 'print', create=True):
            with self.assertRaises(RuntimeError) as raised:
                runner.start_typed_app(SimpleNamespace(tools=Path('/synthetic/tools'), binary=Path('/synthetic/app')), {})
            self.assertIs(raised.exception, error)
        shutdown.assert_called_once_with(None, process)
