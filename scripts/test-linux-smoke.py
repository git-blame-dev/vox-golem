#!/usr/bin/env python3
"""Synthetic process tests; never launch VoxGolem or a provider/audio service."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent.parent


class LinuxSmokeTests(unittest.TestCase):
    def run_case(self, mode, kind="native"):
        with tempfile.TemporaryDirectory(prefix="voxgolem-smoke-test-") as directory:
            directory = Path(directory)
            result_file = directory / "result.json"
            executable = directory / "synthetic-app"
            executable.write_text("#!/usr/bin/python3\n" + f"MODE={mode!r}\nRESULT={str(result_file)!r}\n" + '''
import json, os, pathlib, signal, subprocess, sys, time
config = pathlib.Path(os.environ["VOXGOLEM_CONFIG_PATH"])
keys = ["HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME",
        "XDG_CACHE_HOME", "XDG_RUNTIME_DIR", "TMPDIR"]
child = subprocess.Popen(["/usr/bin/python3", "-c",
    "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(120)"])
pathlib.Path(RESULT).write_text(json.dumps({
    "roots": {key: os.environ[key] for key in keys},
    "config": str(config), "contents": config.read_text(),
    "state": (config.parent / "state.toml").read_text(),
    "cwd": os.getcwd(), "child": child.pid,
    "net": os.readlink("/proc/self/ns/net"),
    "environment": sorted(os.environ),
    "diagnostics": os.environ.get("VOXGOLEM_STARTUP_DIAGNOSTICS"),
    "arguments": sys.argv[1:],
    "uid": os.getuid(),
    "gid": os.getgid(),
}))
print("VOXGOLEM_STARTUP_ENTER", flush=True)
if MODE == "exit":
    raise SystemExit(42)
if MODE == "ready-exit":
    print("VOXGOLEM_STARTUP_READY", flush=True)
    raise SystemExit(0)
if MODE != "timeout":
    print("VOXGOLEM_STARTUP_READY", flush=True)
if MODE == "warning":
    time.sleep(2)
    print("GStreamer element appsink not found", flush=True)
time.sleep(120)
''')
            executable.chmod(0o700)
            started = time.monotonic()
            process = subprocess.run(
                ["bash", str(ROOT / "scripts/linux-smoke.sh"), kind, str(executable)],
                text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                env={**os.environ, "SYNTHETIC_PROVIDER_SECRET": "must-not-inherit"},
                timeout=45,
            )
            elapsed = time.monotonic() - started
            self.assertTrue(result_file.exists(), process.stderr)
            result = json.loads(result_file.read_text())
            if os.environ.get("VOXGOLEM_SMOKE_NAMESPACE_MODE") == "privileged-network":
                self.assertEqual((result["uid"], result["gid"]), (os.getuid(), os.getgid()))
            self.assertEqual(result["arguments"], ["--appimage-extract-and-run"] if kind == "appimage" else [])
            smoke_dir = Path(result["config"]).parent.parent
            self.assertFalse(smoke_dir.exists(), "private directory leaked")
            # Killed descendants can briefly remain zombies under a slow host init.
            stat = Path(f'/proc/{result["child"]}/stat')
            if stat.exists():
                self.assertEqual(stat.read_text().split(") ", 1)[1][0], "Z",
                                 "owned descendant still executing")
            for value in result["roots"].values():
                self.assertTrue(Path(value).is_relative_to(smoke_dir))
            self.assertEqual(len(set(result["roots"].values())), 7)
            self.assertTrue(Path(result["cwd"]).is_relative_to(smoke_dir))
            self.assertEqual(result["contents"], "web_enabled = false\n")
            for flag in ("auto_update_download", "auto_update_install", "tts_enabled", "web_enabled"):
                self.assertIn(f"{flag} = false\n", result["state"])
            self.assertNotIn("SYNTHETIC_PROVIDER_SECRET", result["environment"])
            self.assertEqual(result["diagnostics"], "1")
            self.assertNotEqual(result["net"], os.readlink("/proc/self/ns/net"))
            return process, elapsed, str(smoke_dir)

    def test_ready_isolated_and_unique(self):
        first, _, first_root = self.run_case("ready")
        second, _, second_root = self.run_case("ready")
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertNotEqual(first_root, second_root)

    def test_early_exit_diagnostics_and_orphan_cleanup(self):
        result, _, _ = self.run_case("exit")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("VOXGOLEM_STARTUP_ENTER", result.stderr)
        self.assertIn("Linux smoke diagnostics:", result.stderr)

    def test_dead_process_marker_is_not_success(self):
        result, _, _ = self.run_case("ready-exit")
        self.assertNotEqual(result.returncode, 0)

    def test_appimage_remains_healthy_for_five_seconds(self):
        result, elapsed, _ = self.run_case("ready", "appimage")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertGreaterEqual(elapsed, 6)

    def test_appimage_delayed_warning_fails(self):
        result, _, _ = self.run_case("warning", "appimage")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("appsink is unavailable", result.stderr)

    def test_full_30_second_deadline_and_failure_diagnostics(self):
        result, elapsed, _ = self.run_case("timeout")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("timed out", result.stderr)
        self.assertIn("VOXGOLEM_STARTUP_ENTER", result.stderr)
        self.assertGreaterEqual(elapsed, 30)
        self.assertLess(elapsed, 42)


if __name__ == "__main__":
    unittest.main(verbosity=2)
