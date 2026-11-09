# Listening cue provenance

`generate-listening-cues.py` is the authoritative source of the bundled listening
cue waveforms. They contain only mathematically synthesized sine waves: no
recordings, speech, downloaded samples, random input or runtime data.

Generate from the checkout with:

```sh
python3 scripts/generate-listening-cues.py
python3 scripts/generate-listening-cues.py --check
python3 -m unittest scripts/test_generate_listening_cues.py scripts/test_generate_mobile_cues.py
```

The canonical WAVs are PCM16 little endian, mono, 48 kHz, 15,360 frames (320 ms).
Start plays 392 then 523.25 Hz, with a peak ceiling of 9,000 PCM units; stop reverses
the notes with a ceiling of 8,050. Each note occupies half the cue; a linear 12 ms
attack and 60 ms release bring its endpoints to zero. Sample amplitudes use Python
`round`, and phase is `2*pi*frequency*sample_index/sample_rate`. There is no runtime
synthesis, extra decoding step, or increase in the desktop WAV duration/size.
The duration, pitches and amplitude ceilings approximate the prior short desktop
cues; audible equivalence has not been established.

The generator writes identical WAV bytes to desktop resources and browser public
resources. Android phone and Wear already consume those browser WAVs through
`crates/voice-android/build.gradle.kts` and `generate-mobile-cues.py`; that staging
step retains its existing bounded +8 dB start gain and unchanged stop copy. It is
an amplitude adapter, not a second waveform source. The legacy desktop
`stop-listening.mp3` remains present and is encoded from the canonical stop WAV.
The formerly stale browser stop WAV is now the same 320 ms stop cue as desktop.

WAV generation needs only Python's standard library. Legacy MP3 generation/checks
also use the existing ffmpeg with libmp3lame (128 kbit/s, source metadata removed,
bitexact flags, no ID3 tags). The checked-in MP3 was produced with ffmpeg
8.0.1-3ubuntu2. MP3 byte reproducibility requires the same encoder versions;
encoder upgrades may require regenerating that file. Python floating-point
sine results may vary at rounding boundaries on other math-library platforms;
`--check` detects byte drift rather than silently accepting it.

The earlier listening-cue binaries did not have an established generation source
in repository history. They were replaced rather than certified as recordings or
as synthetic media. Historical sanitization is a separate operation: replacements
can use `wav_bytes(kind, sample_rate, channels, frames)` and `mp3_bytes(wav)` from
this same generator, preserving historical format and duration without reusing
old audio samples. Do not substitute an unrelated recording as a fixture.
