# VoxGolem

VoxGolem is a local AI voice assistant that listens hands-free, stops automatically, and combines multiple frontier models for fast answers that improve in real time.

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
- Wake-word detection: `rustpotter`
- End-of-speech detection: Silero VAD ONNX via `ort`
- Local transcription: `transcribe-rs` (Parakeet v2)
- Configuration: `serde` + `toml`

## 🗺️ Roadmap Checklist

- ✅ Foundation complete (public repo, architecture direction, MVP plan).
- ⬜ Chat shell complete (top conversation view + bottom composer with keyboard send).
- ⬜ Voice pipeline complete (wake-word, silence stop, local Parakeet v2 STT).
- ⬜ Execution pipeline complete (`opencode` direct args + labeled output).
- ⬜ Progressive response mode complete (fast first answer, background refinement updates).
