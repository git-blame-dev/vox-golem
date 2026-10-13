# Vox Golem

[![Linux CI](https://github.com/git-blame-dev/vox-golem/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/git-blame-dev/vox-golem/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/git-blame-dev/vox-golem?label=release&logo=github&logoColor=white)](https://github.com/git-blame-dev/vox-golem/releases/latest)
[![Ubuntu 24.04 x86_64](https://img.shields.io/badge/Ubuntu%2024.04-x86__64-E95420?logo=ubuntu&logoColor=white)](#supported-linux-baseline)

Vox Golem is a Linux-first assistant with Linux/Windows Tauri desktop, Android phone, and native Wear OS shells. Desktop supports typed or spoken developer prompts, including configured wake-word capture and local transcription. Phone and watch can each run foreground Custom-provider chat; neither requires the other to execute its provider request once credentials and network access are available.

Desktop and phone share the responsive React UI. Wear uses native Kotlin/Compose and an ARM32 Rust JNI library, not a WebView. Browser preview uses the same React tree with a labelled deterministic simulated runtime.

## Platform boundaries

| Capability | Linux/Windows desktop | Android phone | Wear OS |
| --- | --- | --- | --- |
| Chat providers | Local, Custom, OpenCode | Custom; foreground only | Custom; foreground only |
| Conversation storage | Desktop lifecycle; no phone/Wear replication wiring | App-private SQLite, local drafts, conversation/head selection | Same native store/domain; local drafts and conversation/head selection |
| Phone/Wear conversation sync | Not wired | Automatic native-paired sharing | Automatic native-paired sharing |
| Assistant speech | Configured desktop wake/VAD/transcription/TTS | Native admission enabled; on-device dictation requires installed language support; optional local TTS | On-device SDK dictation when installed language support is proven; optional local TTS |
| Voice notes | No native watch capture runtime | Watch-audio recognition, history and SAF/Org export | Durable local recording with phone preview/archive transfer |
| Desktop-only features | Completion, Deep/Review, prefetch, signed updates | Capability-gated off | Not included |

Linux builds and tests do not establish Windows, Android or Wear runtime readiness. Real microphone, speech provider, gestures, lifecycle, GMS transport and optimized JNI paths require device acceptance.

## Android and Wear builds

Run from this source checkout on Linux. Use JDK 21, Android SDK platform/build tools 36, NDK 28.2.13676358 and Tauri CLI 2.11.1. Set `JAVA_HOME`, `ANDROID_HOME`, and `NDK_HOME`; put the JDK's `bin` on `PATH`. Rust targets are `aarch64-linux-android` for the phone and `armv7-linux-androideabi` for the current Wear APK. JNI builds use cached Cargo dependencies (`--offline --locked`).

```bash
make android-build           # ARM64 phone debug APK, including both native plugins
make android-assistant-test  # shared Assistant + phone bridge unit tests and lint
make android-capture-test    # shared capture + phone plugin unit tests and lint
make wear-test               # shared libraries + Wear unit tests and lint
make wear-build              # ARM32 Wear debug APK
make wear-performance        # optimized, non-debuggable, debug-signed local trial
```

Phone output: `apps/android-tauri/src-tauri/gen/android/app/build/outputs/apk/universal/debug/`. Wear outputs: `apps/wear/app/build/outputs/apk/{debug,performance}/`. An optimized local trial is not a production-signed release. These commands build/check source; they do not install anything.

`make android-init` is the only phone scaffold-regeneration command. Keep authored configuration, native libraries and UI sources outside generated output; launcher assets derive from the canonical [SVG](apps/desktop-tauri/src-tauri/icons/icon.svg). The [phone shell guide](apps/android-tauri/README.md) covers native setup, and the [Wear guide](apps/wear/README.md) covers launchers and device gates.

## Shared phone/Wear Assistant

Phone and watch retain the **same conversation identities and completed turns** after native enrollment and reconciliation, while drafts, selected heads, running requests and provisional text remain local. Concurrent continuations remain branches: choose a head before sending rather than silently merging sibling answers. New Chat retains existing conversations.

Rust core owns immutable operations, snapshot validation, merge/deletion rules and bounded context selection. The shared headless JNI bridge applies those rules; [AssistantRuntime](crates/assistant-android/src/main/java/dev/gitblame/voxgolem/assistant/runtime/AssistantRuntime.kt) owns one engine per process. [AssistantStore](crates/assistant-android/src/main/java/dev/gitblame/voxgolem/assistant/AssistantStore.kt) owns app-private SQLite transactions, including revision checks, outboxes and exact per-peer receipts. Installation replica IDs and credentials are excluded from backup. Provisional output is not history: successful delivery gating and a local commit are required before a completed turn is published.

Conversation replication uses bounded, finite GMS DataItem/Asset exchanges of immutable records, never an imported database snapshot or a remote-generation command. Receiver commit precedes exact-body ACK; SDK submission alone is not proof of synchronization. Wear Data Layer may route through encrypted Google servers when direct connectivity is unavailable, so this is **not a cloud-free transport**. Custom prompts and answers also go to the configured remote provider. See the [shared native Assistant guide](crates/assistant-android/README.md) for ownership and canonical source links.

### Provider access: external development setup only

Phone and Watch consume existing access from their own app-private provider files, read-only. There is no credential settings panel, document-picker import, phone-to-Watch credential copy, login, renewal or refresh workflow. Normal chat still reports unavailable, expired or rejected provider access and retains drafts for deliberate retry.

For authorized development, manually inject an existing compatible access-only file into each selected app's private provider location using external developer tooling. Keep that tooling and all source/staging files outside this repository, publish a complete owner-only file (0600), verify the target and publication without printing contents, then restart the app. Preserve upstream auth, histories and other app data. Do not put runtime keys in prompts, logs, examples or project artifacts. There is no maintained provisioning/export helper in this repository; non-debuggable installations cannot be assumed to permit `run-as`.

Conversation sharing remains automatic over native-paired, matching-package/signature Wear Data Layer apps. Version-3 peer identity is independent of provider access; sync neither transfers credentials nor requires usable access. Each device needs its own valid access and internet route to generate an answer. A proper device-auth flow is deferred, not available in the current app.

### Phone voice chat shortcut

Android Settings exposes **Add Ask to Home screen**. On API26+ with a supporting launcher, pinning requires launcher confirmation; installation alone never pins it. Ask preserves the current conversation and existing draft, submits one authoritative finalized utterance once, optionally speaks the reply, then idles. Talk uses the same current-context pipeline and reopens recognition after the reply/playback completes. **Stop cancels without sending**; ordinary draft dictation still requires a separate Send. Unverified phone shortcut origins require a visible in-app Ask tap rather than automatic microphone consent. Missing provider/language support leaves typing available. Stable legacy shortcut/command IDs remain compatibility interfaces, not the current new-chat workflow. See the [phone bridge guide](crates/assistant-plugin/README.md#new-voice-chat-shortcut-implemented-source).

The phone plugin now enables shared speech-resource admission on load. This removes the previous source-wiring blocker, not the need for microphone permission, installed provider/language checks or physical acceptance. The separate watch-to-phone supplied-audio Assistant lane has production runtime constructors, but its provider/device gate remains default-false without physical calibration evidence; it is not advertised as an activated UI feature.

Automated builds and checks establish software compatibility, not launcher, speech-provider or physical runtime acceptance. During a refactor, previously built or installed APKs are not evidence for the changing source. Deployment must verify the matching build, application ID, ABI, signing certificate and APK SHA-256, and preserve user data.

## Voice notes: separate from Assistant

On the phone, open **Voice notes**, enable watch dictation through visible permission setup, and select the destination with Android's folder picker. On the watch, use **Vox Note**, including as Samsung knock-knock's target. A deliberate launch starts one recording after setup; ordinary resume does not start another. Recording remains local when the phone is absent. Arm-down stopping needs calibration and worn-device verification; manual Stop remains available.

Vox Note records on the Watch and attempts live phone transcription from supplied PCM (API33+); text previews return while recording, with saved-WAV replay as recovery after Stop. New Notes capture the shared installation speech language/fallback preference at Start; an Automatic language resolves to the Watch locale. With fallback disabled, admission requires the dedicated on-device route. With fallback allowed, an ordinary provider may use network processing; offline preference is not an offline guarantee. Legacy records without captured policy require dedicated on-device recognition. Supplied-input/segmented support and microphone-source isolation require actual provider acceptance, not API availability or a READY callback. No recognition model is bundled or downloaded. Recording readiness, transcription readiness, retained audio, archival and export are distinct statuses. Note capture never submits an Assistant prompt.

Verified WAV copies go to `recordings/<capture-id>.wav` in the selected folder. The app creates `ai-inbox.org` if absent and prepends a final transcript as a `* TODO` with a capture-day, date-only `SCHEDULED` line, without a summary or property drawer. Only the matching `ai-inbox.org.txt` candidate may be normalized. Existing `inbox.org` files and old exports stay untouched; old-target pending journals require review. Corrections to an already exported transcript do not create another task. Ambiguous SAF writes retain recovery data rather than claiming success; permanent recordings are not automatically pruned.

Shared records, SQLite and WAV code live in [capture-android](crates/capture-android/README.md); the [capture API guide](crates/capture-plugin/API.md) links canonical types, phone adapters and the frontend parser without duplicating schemas. Watch **SAVED**, phone archival and Org export are distinct acknowledgements.

## Capabilities

Each capability is optional and uses user-supplied local assets: local `llama.cpp`, the experimental Custom private Codex endpoint, OpenCode, wake-word/VAD/transcription, and local TTS. Eight Instant choices are **Local Fast**, **Local Quality**, **Custom Sol High**, **Custom Sol No Reasoning**, **Custom GPT-6 Luna No Reasoning**, **Custom GPT-5.6 Luna Low**, **OpenCode Sol High**, and **OpenCode Luna Low**. Both Custom Sol choices and Custom GPT-6 Luna No Reasoning use VoxGolem's direct Rust connection to the configured Custom endpoint; OpenCode choices use the separate OpenCode client. Four Deep/Review choices are **Custom Sol High**, **Custom Luna Low**, **OpenCode Sol High**, and **OpenCode Luna Low**.

Deep and Review default to disabled. They are reasoning/review-only paths with no workspace or shell authority; OpenCode research is restricted to `websearch` and `webfetch`. Prefetch is disabled by default; enabling it can transmit predicted prompt text to the configured provider before submission.

The stable Custom/OpenCode Sol aliases now request `gpt-6.1-sol`; alias mapping is not
evidence that a live provider accepts that model. Chat remains on the requested backend,
independent of local-first speech processing.

### Web permission and response presentation

Web access defaults on and is persisted per installation through Settings, separately
from Quick/Verify/Research effort. Verify and Research are explicit lookup actions, not
prompt-keyword heuristics. Disabling web access keeps ordinary chat available, advertises
no web tools and prevents native lookup/provider-fallback calls; it does not make remote
chat or provider-controlled speech offline. Native lookup sends model-generated queries,
focus text and URLs to Parallel, with Exa as the disclosed fallback. OpenCode research is
delegated to its configured runtime, whose external provider identity may be unknown.
External correlation is process-scoped rather than the canonical local conversation ID.

One portable app-owned policy requests plain text and the shortest complete answer,
without a duplicate lead summary. Tool calls, required structured stage JSON, refusals,
literal code/symbols and essential sources retain their contracts. Existing conversation
records and external `SOUL.md` content are not rewritten. Mobile speech consumes the
answer-only projection, not the canonical source footer. Model compliance and physical
speech behavior remain separate acceptance checks.

## Supported Linux baseline

The supported release baseline is Ubuntu 24.04 LTS on x86_64, including Ubuntu 24.04 under WSL2/WSLg. CI builds on that baseline; older Ubuntu releases and other distributions are not promised because native library compatibility is not verified. CUDA and its system libraries are intentionally external and are never bundled.

## Windows target

Windows 11 x64 uses a per-user NSIS installer under `%LOCALAPPDATA%\Programs\VoxGolem`. The installer is built on Ubuntu, creates a Start Menu shortcut, and packages only the application, DirectML, ONNX Runtime provider bridges, and required app-local Microsoft VC runtime DLLs. NVIDIA drivers, CUDA, cuDNN, TensorRT, models, `llama-server`, `SOUL.md`, configuration, credentials, and user data remain external. Windows artifacts are not Authenticode-signed and may trigger SmartScreen. Installation, single-instance behavior, configured NVIDIA and forced-CPU modes, audio, data preservation, uninstall/reinstall, and the previously shipped updater were validated on Windows 11 x64 NVIDIA hardware. The maintained-updater installer was also installed successfully over the existing application while preserving configuration; a signed in-app update through the new updater still requires human Windows acceptance. Non-NVIDIA Windows hardware remains unvalidated.

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

The default locations are `$HOME/.local/share/opencode/auth.json` and `$HOME/.opencode/bin/opencode`, followed by WSL command lookup for the executable. On Windows, `wsl.exe` uses the default WSL distribution, and `$HOME` is that distribution's user home. Optional overrides must be absolute Linux paths. WSL selection is explicit per provider; omitted selectors retain native behavior, and Vox Golem never silently falls back to a different credential or executable source. Missing WSL, a default distribution, auth, or OpenCode disables only the affected capability. A failed Custom WSL auth lookup gets one bounded automatic startup retry; if it still fails, the app keeps the reason visible and offers **Retry Custom** without a restart.

## WSL2/WSLg setup

Install Bun 1.3.9, Rust 1.95.0, Tauri CLI 2.11.1, and the Ubuntu WebKitGTK/GTK, audio, CMake, Ninja, Clang/LLVM, and eSpeak development packages. WSLg supplies the Linux GUI and microphone integration; verify those manually on your machine.

```bash
sudo apt-get install libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libasound2-dev libssl-dev libxdo-dev libespeak-dev cmake ninja-build clang lld llvm pkg-config
cargo install tauri-cli --version 2.11.1 --locked
```

Copy [`config.example.toml`](config.example.toml) to `${XDG_CONFIG_HOME:-$HOME/.config}/voxgolem/config.toml` (or set `VOXGOLEM_CONFIG_PATH`). When using Local Fast or Local Quality on any platform, also create a non-empty `SOUL.md` beside that config; copying the example is therefore a config-only setup step, and the required soul text must be supplied separately. Missing local assets or `SOUL.md` disable only the local response capability. XDG defaults are `~/.config/voxgolem`, `~/.local/share/voxgolem`, `~/.local/state/voxgolem`, and `~/.cache/voxgolem`. Relative paths resolve beside the config. Keep model files external; there is no model downloader.

Desktop OpenCode uses its existing CLI login/auth state; the desktop app does not perform login or renewal. Mobile external development access setup is described above. Custom endpoints are experimental, especially private endpoints; inspect the endpoint and auth configuration before use. Never put secrets in examples or source control.

### Standalone local llama.cpp runtime

Local chat and completion use an external `llama-server` bundle, not another application project. `scripts/build-local-llama.sh` builds pinned llama.cpp `b10054` (`ac2557cb24def295888ef47f1a35b401d978c510`) with CUDA and stages the executable, colocated libraries, licence and checksum manifest under `${XDG_DATA_HOME:-$HOME/.local/share}/voxgolem/runtimes/llama.cpp/b10054/`.

```bash
LLAMA_SOURCE=/absolute/path/to/pinned/llama.cpp \
CUDAToolkit_ROOT=/absolute/path/to/cuda \
LLAMA_CUDA_ARCHITECTURES=86 \
scripts/build-local-llama.sh
```

The source must be clean and pinned; the script does not download source or models, install drivers, or overwrite an existing runtime. It requires Git, CMake, Ninja and patchelf; `PATCHELF` can select an explicit executable. Staged ELF library paths are normalized to exactly `$ORIGIN`, without build-cache or current-directory entries. Architecture 86 targets RTX 30-series GPUs; select the actual hardware architecture for another machine. `LLAMA_BUILD_JOBS`, `LLAMA_BUILD_DIR` and `LLAMA_RUNTIME_DIR` override bounded build concurrency and external output paths. Keep all runtime library files and symlinks together. Point both `[llama_cpp].server_path` and `[completion].server_path` at the staged `bin/llama-server`, and configure models separately. Existing `auto`, `cuda` and `cpu` inference policies remain unchanged.

Run `python3 scripts/test_build_local_llama.py` for synthetic build-boundary, no-clobber publication and bundle-preservation checks; these do not compile CUDA or run models.

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

For repeatable synthetic wake-word accuracy checks, run `make wakeword-bench WAKE_WORD_MODEL=/path/to/hey_livekit.onnx`. The [benchmark guide](tools/wakeword-bench/README.md) describes corpus generation, calibration/holdout evaluation, artifact verification, and the limits of artificial speech tests.

When the microphone is active, VoxGolem saves up to 0.8 seconds of pre-trigger wake audio for each new detection as a local PCM16 WAV; older 0.5-, 0.55-, 0.7-, 0.75-, 1.5- and three-second clips remain playable. This tight window can truncate a slowly spoken wake phrase, so review recordings before using them for training. Clips are grouped by the detecting model filename and a local content-derived revision ID. The revision ID distinguishes replaced model bytes with the same filename; it is not a cryptographic checksum and may change with Rust toolchain versions. In Settings → Wake clips, you can pause saving, choose an absolute local save directory, browse clips in pages, play them, label them true/false wakes, or delete them. Clips start unreviewed: the detector's model name is a prediction, not proof of what was said. The default folder is `wake-clips` beside your local VoxGolem configuration (`%APPDATA%\VoxGolem\wake-clips` on Windows); the selected directory is stored locally in `wake-clips.json`. No storage quota or automatic deletion is applied. Save failures appear as a notice without stopping the microphone. Recordings, models, and runtime settings are local user data and stay out of Git.

After a wake, `initial_silence_timeout_ms` allows 2,500 ms by default for the first VAD-detected speech. If none arrives, VoxGolem returns to wake-word waiting without transcribing or playing the processing cue. Once VAD detects speech, `silence_timeout_ms` ends the utterance after 750 ms by default without another speech detection. Both values are configurable in `config.toml`; VAD detects speech activity, not speaker identity, so confirm the timing with your microphone and speaker setup.

The staged native binary is `dist/VoxGolem/vox-golem`. Windows cross-builds require cargo-xwin 0.23.0, LLVM/Clang 19 or newer, NSIS, cabextract, and 7-Zip. They do not require a Windows runner.

CI runs `make test` plus separate Ubuntu 24.04 Linux and Windows artifact jobs on pull requests and non-main branches. Main pushes use the self-contained release workflow instead of rebuilding those unsigned CI packages: tests run first, then the pinned Tauri action builds Linux and Windows sequentially before the draft is validated and published. Keeping validation and release workflows separate avoids a privileged cross-workflow handoff while preventing duplicate main package builds. The Linux job retains the small AppDir post-processing step needed to include GStreamer appsink and build-produced ONNX provider libraries without recompiling the executable; the Linux ZIP reuses that same executable. The Windows bundle uses Tauri's native resource mapping and pre-bundle staging. Exact package allowlists reject system NVIDIA runtimes, models, and user data. CI cannot prove microphone, WebView/WSLg, GPU, model, TTS, speaker, real Windows behavior, or a signed in-app update; perform those runtime checks manually. Inference policy `auto` prefers CUDA when usable and otherwise falls back to CPU; `cuda` requires CUDA and `cpu` forces CPU.

## Signed application updates

Pushes to protected `main` publish one complete GitHub release containing the Linux ZIP, signed x86_64 AppImage, signed Windows 11 x64 NSIS installer, both detached updater signatures, a two-platform `latest.json`, and `SHA256SUMS`. Publication fails if either platform artifact is missing; once a draft exists, a failed run leaves it available for a rerun to replace and complete, while an already-published release is verified and left unchanged. Packaged AppImage and NSIS builds check and download in the background by default. Verified downloads also install automatically by default, but only while VoxGolem is fully idle; installation never interrupts recording, prompts, responses, TTS, model work, startup, or application shutdown. A busy app retains the verified download and offers **Restart and update** and **Later**. Later frees the in-memory download and defers background updates for the current session. Automatic download and automatic installation are independent persisted settings. Manual **Check** reports availability and leaves downloading to **Download**. **Retry** repeats the failed check, download, or installation while preserving any retained update.

The maintained Tauri updater verifies every downloaded AppImage or NSIS installer with the public key embedded in `tauri.conf.json`; signature verification cannot be disabled. Verified downloads remain only in memory until installation or **Later** discards them. On Linux, installation replaces the AppImage and relaunches automatically. On Windows, VoxGolem first excludes normal activity and shuts down model, audio, completion, and OpenCode services before launching NSIS. If the installer cannot be launched, the app honestly requires a restart and keeps normal runtime operations blocked rather than reconstructing services. Upstream installation errors do not imply an atomic rollback; use the surfaced error and reinstall the signed package manually if necessary. Updates cover only application package files, never user models, configuration, credentials, or system GPU libraries.

Release versions use `YYYY.M.D-N`, where `N` advances beyond the highest canonical draft or published release ordinal reserved for that UTC date. A rerun reuses the sole canonical release already targeting the same source commit, including after the date changes. Linux and Windows artifacts and the release tag use the same version and source SHA.

Maintainers must store the matching private key outside the repository and configure it as the GitHub Actions secret `TAURI_SIGNING_PRIVATE_KEY`. If the key is password-protected, also configure `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`. For example, an authorized maintainer can provision the key without printing it:

```bash
gh secret set TAURI_SIGNING_PRIVATE_KEY < ~/.tauri/vox-golem-updater.key
```

Back up the private key securely. Losing it prevents installed copies from accepting future updates. Do not rotate the embedded public key without a staged migration signed by the old key, and never commit or log the private key.

Telemetry is enabled by default, configurable off, local, rotating, and metadata-only; it excludes prompt/audio contents, predictions, answers, URLs, credentials, and raw errors. Voice input and provider traffic remain user data; review the provider and prefetch settings before enabling transmission.
