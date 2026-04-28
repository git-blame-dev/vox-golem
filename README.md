# VoxGolem

VoxGolem is a local AI voice assistant that listens hands-free, stops automatically, and runs local model profiles so you can trade speed for reasoning depth.

The Windows-first baseline MVP voice path is now in place: live mic capture feeds wake-word detection, backend-owned VAD, local Parakeet transcription, and the existing `opencode` execution path.

## 🚀 What It Does

- Captures voice with wake word plus automatic stop after silence.
- Supports typed prompts in the same chat-style interface.
- Shows transcript, verification state, and labeled command output.
- Runs local Gemma-class profiles with a fast mode and a smarter slower mode.

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
- Local model inference: `llama.cpp` (`llama-server`) + GGUF models
- Local model families: Gemma 3 and Gemma 4 profile variants

## 🗺️ Roadmap Checklist

- ✅ Foundation complete (public repo, architecture direction, MVP plan).
- ✅ Chat shell complete (top conversation view + bottom composer with keyboard send).
- ✅ Voice pipeline complete (wake-word, silence stop, local Parakeet v2 STT).
- ✅ Execution pipeline complete (`opencode` direct args + labeled output).
- ✅ Local AI profile toggle complete (fast profile vs quality profile with local models).
- ⬜ Progressive response mode complete (fast first answer, background refinement updates).
- ⬜ Dual-profile simultaneous answering (run fast and quality models together, then update the response live as each model returns).
- ⬜ Realistic voice output toggle (short, succinct spoken answers while preserving longer detailed text responses in chat).
- ⬜ Android version.

## ⚙️ Required Local Assets

The Windows runtime expects these local assets in `%APPDATA%\VoxGolem`:

- `SOUL.md`: assistant identity and response-style prompt file
- `models/hey_livekit.onnx`: wake-word classifier
- `models/parakeet-v2`: local Parakeet v2 model directory
- `models/silero-vad.onnx`: local Silero VAD ONNX file
- `llama/bin/llama-server.exe`: local llama.cpp runtime binary
- `models/llama/*`: GGUF model files for fast and quality profile choices
- `start-listening.wav` and `stop-listening.wav` (optional cue audio)
- `state.toml` (app-managed selected profile state)

For a current local Gemma desktop setup, common model choices include:

- `models/llama/gemma-3-1b-it-Q4_K_M.gguf`
- `models/llama/gemma-3-1b-it-Q8_0.gguf`
- `models/llama/google_gemma-4-E2B-it-Q2_K.gguf`
- `models/llama/google_gemma-4-E2B-it-Q3_K_S.gguf`
- `models/llama/google_gemma-4-E2B-it-Q8_0.gguf`
- `models/llama/google_gemma-4-E4B-it-Q8_0.gguf`
