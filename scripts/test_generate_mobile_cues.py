import importlib.util
import math
from pathlib import Path
import struct
import sys
import tempfile
import unittest
from unittest.mock import patch
import wave

SPEC = importlib.util.spec_from_file_location("mobile_cues", Path(__file__).with_name("generate-mobile-cues.py"))
assert SPEC is not None and SPEC.loader is not None
GENERATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GENERATOR)


class MobileCueGenerationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.source = Path(self.temp.name) / "source"
        self.output = Path(self.temp.name) / "generated"
        self.source.mkdir()
        self.write("stop-listening.wav", [1200, -1200, 0])

    def write(self, name, samples, width=2):
        with wave.open(str(self.source / name), "wb") as audio:
            audio.setnchannels(1)
            audio.setsampwidth(width)
            audio.setframerate(48000)
            audio.writeframes(struct.pack("<" + "h" * len(samples), *samples) if width == 2 else bytes(samples))

    def generate(self):
        with patch.object(sys, "argv", ["generate-mobile-cues.py", "--source-dir", str(self.source), "--output-dir", str(self.output)]):
            GENERATOR.main()

    def samples(self):
        with wave.open(str(self.output / "raw/start_listening.wav"), "rb") as audio:
            self.assertEqual((audio.getnchannels(), audio.getsampwidth(), audio.getframerate()), (1, 2, 48000))
            frames = audio.readframes(audio.getnframes())
            return struct.unpack("<" + "h" * (len(frames) // 2), frames)

    def test_actual_cli_generation_boosts_start_eight_db_and_preserves_end_and_source(self):
        self.write("start-listening.wav", [-9000, 0, 9000])
        original = (self.source / "start-listening.wav").read_bytes()
        ending = (self.source / "stop-listening.wav").read_bytes()
        self.generate()
        expected = round(9000 * math.pow(10, 8 / 20))
        self.assertEqual(self.samples(), (-expected, 0, expected))
        self.assertEqual((self.source / "start-listening.wav").read_bytes(), original)
        self.assertEqual((self.output / "raw/stop_listening.wav").read_bytes(), ending)

    def test_loud_input_uses_uniform_gain_with_one_db_headroom_not_clipping(self):
        self.write("start-listening.wav", [-32000, -16000, 16000, 32000])
        self.generate()
        samples = self.samples()
        ceiling = math.ceil(32767 * math.pow(10, -1 / 20))
        self.assertLessEqual(max(abs(value) for value in samples), ceiling)
        self.assertAlmostEqual(samples[0] / samples[1], 2, places=3)
        self.assertEqual(samples[0], -samples[3])

    def test_silent_and_non_pcm16_input_are_rejected_before_staging(self):
        for samples, width in [([0, 0], 2), ([1, 2, 3], 1)]:
            with self.subTest(width=width):
                self.write("start-listening.wav", samples, width)
                with self.assertRaises(ValueError):
                    self.generate()
                self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
