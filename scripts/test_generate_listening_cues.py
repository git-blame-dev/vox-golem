import hashlib
import importlib.util
import io
from pathlib import Path
import struct
import subprocess
import tempfile
import unittest
import wave

SPEC = importlib.util.spec_from_file_location("listening_cues", Path(__file__).with_name("generate-listening-cues.py"))
assert SPEC is not None and SPEC.loader is not None
CUES = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CUES)


class ListeningCueTests(unittest.TestCase):
    def test_committed_assets_match_synthesis_and_browser_matches_desktop(self):
        for relative, expected in CUES.canonical_assets().items():
            with self.subTest(path=relative):
                self.assertEqual(hashlib.sha256((CUES.ROOT / relative).read_bytes()).hexdigest(),
                                 hashlib.sha256(expected).hexdigest())

    def test_native_duration_headroom_pitch_and_click_free_boundaries(self):
        for kind, pitches, ceiling in [("start", (392, 523.25), 9000), ("stop", (523.25, 392), 8050)]:
            with self.subTest(kind=kind), wave.open(io.BytesIO(CUES.wav_bytes(kind))) as audio:
                self.assertEqual((audio.getframerate(), audio.getnchannels(), audio.getsampwidth(), audio.getnframes()),
                                 (48000, 1, 2, 15360))
                samples = struct.unpack("<15360h", audio.readframes(15360))
                self.assertEqual((samples[0], samples[-1]), (0, 0))
                self.assertLessEqual(max(map(abs, samples)), ceiling)
                self.assertGreater(max(map(abs, samples)), ceiling * 0.99)
                self.assertLess(max(abs(a - b) for a, b in zip(samples, samples[1:])), 700)
                for offset, pitch in zip((960, 8640), pitches):
                    window = samples[offset:offset + 4800]
                    crossings = sum(a <= 0 < b for a, b in zip(window, window[1:]))
                    self.assertAlmostEqual(crossings / 0.1, pitch, delta=12)

    def test_historical_format_and_duration_parameters(self):
        for rate, channels, frames in [(48000, 2, 54144), (48000, 2, 7680), (48000, 2, 50606), (44100, 2, 63930)]:
            with self.subTest(frames=frames), wave.open(io.BytesIO(CUES.wav_bytes("stop", rate, channels, frames))) as audio:
                self.assertEqual((audio.getframerate(), audio.getnchannels(), audio.getnframes()), (rate, channels, frames))
                samples = struct.unpack("<" + "h" * frames * channels, audio.readframes(frames))
                self.assertEqual(samples[::2], samples[1::2])

    def test_mobile_staging_consumes_generated_shared_source(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "source"
            source.mkdir()
            for kind in ("start", "stop"):
                (source / f"{kind}-listening.wav").write_bytes(CUES.wav_bytes(kind))
            subprocess.run(["python3", str(CUES.ROOT / "scripts/generate-mobile-cues.py"),
                            "--source-dir", str(source), "--output-dir", str(root / "mobile")], check=True)
            self.assertEqual((root / "mobile/raw/stop_listening.wav").read_bytes(), CUES.wav_bytes("stop"))
            with wave.open(str(root / "mobile/raw/start_listening.wav")) as audio:
                samples = struct.unpack("<15360h", audio.readframes(15360))
                self.assertEqual(audio.getnchannels(), 1)
                self.assertLessEqual(max(map(abs, samples)), 29205)
                self.assertGreater(max(map(abs, samples)), 22000)


if __name__ == "__main__":
    unittest.main()
