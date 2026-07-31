# Vox Golem

[![Linux CI](https://github.com/git-blame-dev/vox-golem/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/git-blame-dev/vox-golem/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/git-blame-dev/vox-golem?label=release&logo=github&logoColor=white)](https://github.com/git-blame-dev/vox-golem/releases/latest)
[![Ubuntu 24.04 x86_64](https://img.shields.io/badge/Ubuntu%2024.04-x86__64-E95420?logo=ubuntu&logoColor=white)](#supported-linux-baseline)

Vox Golem is a Linux-first, Linux/Windows Tauri desktop voice assistant for typed or spoken developer prompts. It captures wake-word turns, transcribes locally when configured, and displays the answer and runtime state in one UI.

## Capabilities

Each capability is optional and uses user-supplied local assets: local `llama.cpp`, the experimental Custom private Codex endpoint, OpenCode, wake-word/VAD/transcription, and local TTS. Six Instant choices are **Local Fast**, **Local Quality**, **Custom Sol High**, **Custom Luna Low**, **OpenCode Sol High**, and **OpenCode Luna Low**. Four Deep/Review choices are **Custom Sol High**, **Custom Luna Low**, **OpenCode Sol High**, and **OpenCode Luna Low**.

Deep and Review default to disabled. They are reasoning/review-only paths with no workspace or shell authority; OpenCode research is restricted to `websearch` and `webfetch`. Prefetch is disabled by default; enabling it can transmit predicted prompt text to the configured provider before submission.

## Supported Linux baseline

The supported release baseline is Ubuntu 24.04 LTS on x86_64, including Ubuntu 24.04 under WSL2/WSLg. CI builds on that baseline; older Ubuntu releases and other distributions are not promised because native library compatibility is not verified. CUDA and its system libraries are intentionally external and are never bundled.

## Windows target

Windows 11 x64 uses a per-user NSIS installer under `%LOCALAPPDATA%\Programs\VoxGolem`. The installer is built on Ubuntu, creates a Start Menu shortcut, and packages only the application, DirectML, ONNX Runtime provider bridges, and required app-local Microsoft VC runtime DLLs. NVIDIA drivers, CUDA, cuDNN, TensorRT, models, `llama-server`, `SOUL.md`, configuration, credentials, and user data remain external. Windows artifacts are not Authenticode-signed and may trigger SmartScreen. Installation, single-instance behavior, configured NVIDIA and forced-CPU modes, audio, data preservation, uninstall/reinstall, and a signed in-app update have been validated on Windows 11 x64 NVIDIA hardware; non-NVIDIA Windows hardware remains unvalidated.

Windows configuration defaults to `%APPDATA%\VoxGolem\config.toml`. Relative paths resolve beside that file, and missing assets disable only the affected optional capability.

### Native Windows with WSL providers

The native Windows app can use the default WSL distribution for OpenCode without adding OpenCode or WSL files to the installer. Install and log in to OpenCode inside that distribution first; the distribution must also provide `sh` and `setsid`, as Ubuntu does by default. The Custom provider reads the live WSL auth file but sends its HTTPS request directly from the Windows app; the OpenCode provider starts one authenticated WSL server at app startup and keeps it warm until app exit.

```toml
[custom_openai]
auth_source = "wsl"
endpoint = "https://chatgpt.com/backend-api/codex/responses"

[opencode]
runtime = "wsl"
```

The default locations are `$HOME/.local/share/opencode/auth.json` and `$HOME/.opencode/bin/opencode`, followed by WSL command lookup for the executable. Optional overrides must be absolute Linux paths. WSL selection is explicit per provider; omitted selectors retain native behavior, and Vox Golem never silently falls back to a different credential or executable source. Missing WSL, a default distribution, auth, or OpenCode disables only the affected capability.

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
make update-bundle                    # build and verify an unsigned, generated-version AppImage
make update-bundle-smoke              # run the built AppImage startup check
make pc-dist      # cross-build and stage the lightweight Windows payload on Linux
make pc-installer # build and verify the Windows NSIS installer on Linux
```

`make app-smoke` proves only that the zero-asset Tauri shell reaches its startup marker (shell setup); it does not prove providers, models, voice, completion, or TTS, which require their own runtime checks.

The staged native binary is `dist/VoxGolem/vox-golem`. Windows cross-builds require cargo-xwin 0.23.0, LLVM/Clang 19 or newer, NSIS, cabextract, and 7-Zip. They do not require a Windows runner.

CI runs `make test` plus separate Ubuntu 24.04 Linux and Windows artifact jobs. The Linux job runs `make dist` and `make update-bundle`; the Windows job runs `make pc-installer`. Exact package allowlists reject system NVIDIA runtimes, models, and user data. CI cannot prove microphone, WebView/WSLg, GPU, model, TTS, speaker, or real Windows behavior; perform those hardware/runtime checks manually. Inference policy `auto` prefers CUDA when usable and otherwise falls back to CPU; `cuda` requires CUDA and `cpu` forces CPU.

## Signed application updates

Eligible `main` pushes publish one atomic GitHub release containing the Linux ZIP, signed x86_64 AppImage, signed Windows 11 x64 NSIS installer, both detached updater signatures, a two-platform `latest.json`, and `SHA256SUMS`. Publication fails if either platform artifact is missing. Packaged builds check the latest release automatically and show the result under **Settings → Application updates**. Download and installation require an explicit button press. Linux keeps separate install and restart actions; Windows uses `Install and restart`, launches the passive installer, exits, replaces locked files, and relaunches.

The updater verifies every downloaded AppImage or NSIS installer with the public key embedded in `tauri.conf.json`; signature verification cannot be disabled. Downloads are size-bounded and bound to the exact repository, tag, version, platform, and filename. A failed check, interrupted download, invalid manifest, incompatible artifact, or bad signature leaves the running installation unchanged. Updates cover only application package files, never user models, configuration, credentials, or system GPU libraries.

Release versions use valid semantic versions derived from the source commit date and CI run number: `YYYY.M.D-N`. Linux and Windows artifacts and the release tag use the same version and source SHA.

Maintainers must store the matching private key outside the repository and configure it as the GitHub Actions secret `TAURI_SIGNING_PRIVATE_KEY`. If the key is password-protected, also configure `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. For example, an authorized maintainer can provision the key without printing it:

```bash
gh secret set TAURI_SIGNING_PRIVATE_KEY < ~/.tauri/vox-golem-updater.key
```

Back up the private key securely. Losing it prevents installed copies from accepting future updates. Do not rotate the embedded public key without a staged migration signed by the old key, and never commit or log the private key.

Telemetry is enabled by default, configurable off, local, rotating, and metadata-only; it excludes prompt/audio contents, predictions, answers, URLs, credentials, and raw errors. Voice input and provider traffic remain user data; review the provider and prefetch settings before enabling transmission.
