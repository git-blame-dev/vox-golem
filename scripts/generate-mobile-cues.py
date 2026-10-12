"""Stage canonical mobile microphone cues with bounded start-cue gain."""
import argparse
import math
from pathlib import Path
import shutil
import struct
import wave


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-dir", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    with wave.open(str(args.source_dir / "start-listening.wav"), "rb") as source:
        params = source.getparams()
        if source.getsampwidth() != 2 or source.getcomptype() != "NONE" or not 0 < source.getnframes() <= source.getframerate() * 8:
            raise ValueError("Expected bounded PCM16 listening cue")
        pcm = source.readframes(source.getnframes())
    samples = struct.unpack("<" + "h" * (len(pcm) // 2), pcm)
    peak = max(abs(sample) for sample in samples)
    if peak == 0:
        raise ValueError("Listening cue is silent")
    gain = min(math.pow(10, 8 / 20), (32767 * math.pow(10, -1 / 20)) / peak)
    amplified = struct.pack("<" + "h" * len(samples), *(round(sample * gain) for sample in samples))
    raw = args.output_dir / "raw"
    raw.mkdir(parents=True, exist_ok=True)
    with wave.open(str(raw / "start_listening.wav"), "wb") as target:
        target.setparams(params)
        target.writeframes(amplified)
    shutil.copyfile(args.source_dir / "stop-listening.wav", raw / "stop_listening.wav")


if __name__ == "__main__":
    main()
