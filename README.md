# Vox Golem

[![Linux CI](https://github.com/git-blame-dev/vox-golem/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/git-blame-dev/vox-golem/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/git-blame-dev/vox-golem?label=release&logo=github&logoColor=white)](https://github.com/git-blame-dev/vox-golem/releases/latest)
[![Ubuntu 24.04 x86_64](https://img.shields.io/badge/Ubuntu%2024.04-x86__64-E95420?logo=ubuntu&logoColor=white)](#supported-linux-baseline)

Vox Golem is a Linux-first WSL2/WSLg Tauri desktop voice assistant for typed or spoken developer prompts. It captures wake-word turns, transcribes locally when configured, and displays the answer and runtime state in one UI.

## Capabilities

Each capability is optional and uses user-supplied local assets: local `llama.cpp`, the experimental Custom private Codex endpoint, OpenCode, wake-word/VAD/transcription, and local TTS. Six Instant choices are **Local Fast**, **Local Quality**, **Custom Sol High**, **Custom Luna Low**, **OpenCode Sol High**, and **OpenCode Luna Low**. Four Deep/Review choices are **Custom Sol High**, **Custom Luna Low**, **OpenCode Sol High**, and **OpenCode Luna Low**.

Deep and Review default to disabled. They are reasoning/review-only paths with no workspace or shell authority; OpenCode research is restricted to `websearch` and `webfetch`. Prefetch is disabled by default; enabling it can transmit predicted prompt text to the configured provider before submission.

## Supported Linux baseline

The supported release baseline is Ubuntu 24.04 LTS on x86_64, including Ubuntu 24.04 under WSL2/WSLg. CI builds on that baseline; older Ubuntu releases and other distributions are not promised because native library compatibility is not verified. CUDA and its system libraries are intentionally external and are never bundled.

## WSL2/WSLg setup

Install Bun 1.3.9, Rust stable, Tauri CLI 2.11.1, and the Ubuntu WebKitGTK/GTK, audio, CMake, Ninja, Clang/LLVM, and eSpeak development packages. WSLg supplies the Linux GUI and microphone integration; verify those manually on your machine.

```bash
sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libasound2-dev libssl-dev libxdo-dev libespeak-dev cmake ninja-build clang lld llvm pkg-config
cargo install tauri-cli --version 2.11.1 --locked
```

Copy [`config.example.toml`](config.example.toml) to `${XDG_CONFIG_HOME:-$HOME/.config}/voxgolem/config.toml` (or set `VOXGOLEM_CONFIG_PATH`). When using Local Fast or Local Quality on any platform, also create a non-empty `SOUL.md` beside that config; copying the example is therefore a config-only setup step, and the required soul text must be supplied separately. Missing local assets or `SOUL.md` disable only the local response capability. XDG defaults are `~/.config/voxgolem`, `~/.local/share/voxgolem`, `~/.local/state/voxgolem`, and `~/.cache/voxgolem`. Relative paths resolve beside the config. Keep model files external; there is no model downloader.

OpenCode uses its existing CLI login/auth state; this app does not provision credentials. Custom endpoints are experimental, especially private endpoints; inspect the endpoint and auth configuration before use. Never put secrets in this example or in source control.

## Commands

```bash
make app          # run the native Linux app
make test         # deterministic frontend and Rust checks
make app-smoke    # bounded Linux startup-ready smoke check
make dist         # build and stage native binary
```

`make app-smoke` proves only that the zero-asset Tauri shell reaches its startup marker (shell setup); it does not prove providers, models, voice, completion, or TTS, which require their own runtime checks.

The staged native binary is `dist/VoxGolem/vox-golem`. `make pc-dist` is an optional, non-gating Linux-hosted Windows cross-build and may require extra LLVM/cargo-xwin inputs; it is not part of Linux CI. No Windows runner or Windows release is promised here.

CI runs `make test` and a separate Ubuntu 24.04 native artifact job (`make dist`). The Linux package contains only the executable and the ONNX Runtime shared/CUDA provider libraries produced by the locked build; user models and CUDA system libraries remain external. `make verify-dist` checks package contents, executable/library modes, and fails clearly when TTS-capable ONNX assets are absent. CI cannot prove microphone, WebView/WSLg, GPU, model, TTS, or endpoint behavior; perform those hardware/runtime checks manually. Inference policy `auto` prefers CUDA when usable and otherwise falls back to CPU; `cuda` requires CUDA and `cpu` forces CPU.

Telemetry is enabled by default, configurable off, local, rotating, and metadata-only; it excludes prompt/audio contents, predictions, answers, URLs, credentials, and raw errors. Voice input and provider traffic remain user data; review the provider and prefetch settings before enabling transmission.
