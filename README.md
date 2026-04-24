# VoxGolem

VoxGolem is a local AI voice assistant that listens hands-free, stops automatically, and combines multiple frontier models for fast answers that improve in real time.

The Windows-first baseline MVP voice path is now in place: live mic capture feeds wake-word detection, backend-owned VAD, local Parakeet transcription, and the existing `opencode` execution path.

## 🚀 What It Does

- Captures voice with wake word plus automatic stop after silence.
- Supports typed prompts in the same chat-style interface.
- Shows transcript, verification state, and labeled command output.

## 🧠 Architecture Decisions

- Keep the real-time audio callback non-blocking and allocation-light.
- Bound rolling and utterance audio buffers by explicit limits.
- Use local Parakeet v2 STT behind a swappable transcription boundary.
- Execute `opencode` with direct argument passing (no shell parsing).
- Keep runtime behavior explicit with a typed state machine.

## 🛠️ Tech Stack

- Desktop shell: Tauri (Windows-first)
- Frontend: React, strict TypeScript, Vite, Bun tooling
- Runtime/core: Rust + `tokio`
- Audio capture/handoff: `cpal` + `ringbuf`
- Wake-word detection: `livekit-wakeword`
- End-of-speech detection: Silero VAD ONNX via `ort`
- Local transcription: `transcribe-rs` (Parakeet v2)
- Configuration: `serde` + `toml`

## 🗺️ Roadmap Checklist

- ✅ Foundation complete (public repo, architecture direction, MVP plan).
- ✅ Chat shell complete (top conversation view + bottom composer with keyboard send).
- ✅ Voice pipeline complete (wake-word, silence stop, local Parakeet v2 STT).
- ✅ Execution pipeline complete (`opencode` direct args + labeled output).
- ⬜ Progressive response mode complete (fast first answer, background refinement updates).

## ⚙️ Required Local Assets

The Windows runtime expects these configured local assets in `%APPDATA%\VoxGolem\config.toml`:

- `wake_word_model_path`: LiveKit-compatible wake-word classifier `.onnx` file
- `parakeet_model_dir`: local Parakeet v2 model directory
- `silero_vad_model`: local Silero VAD ONNX file
- `start_listening_cue` and `stop_listening_cue`: cue audio files
- `opencode_path`: local `opencode` executable
