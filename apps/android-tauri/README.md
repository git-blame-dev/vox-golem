# Android Tauri Shell

This Android-only Tauri shell exposes canonical IPC commands and foreground lifecycle hooks through the thin `assistant-plugin` bridge. The shared `assistant-android` runtime owns durable conversations, provider access and phone/Watch replication; the reusable Custom transport remains in `voxgolem-platform`. Desktop audio/models/plugins stay outside this crate. The authored `tauri-plugin-capture` mobile plugin adds native watch dictation, persistent recording/history state, permission setup and SAF notes export independently of chat. A zero-asset install still starts and reports only the affected provider capability as unavailable.

`src-tauri/gen/android` is generator-owned apart from the explicit exceptions below. Regenerate it only with the root `make android-init` wrapper, which pins and verifies Tauri CLI 2.11.1 and runs `make icons` before and after scaffold generation. The canonical artwork is `apps/desktop-tauri/src-tauri/icons/icon.svg`; `make icons` derives desktop PNG/ICO files, the tracked Android `src-tauri/icons/icon.png`, and Android launcher resources from that one SVG. The emblem fills 90% of the visible icon width. Android uses a black background with foreground and monochrome layers sized for its 72dp masked viewport; legacy launcher PNGs are rendered directly without additional inset. Wear consumes the same generated launcher resources. Build the arm64 debug APK with `make android-build`.

The generated baseline is intentionally kept separate from authored behavior. Authored exceptions under `src-tauri/gen/android` are `app/src/main/AndroidManifest.xml`, `app/src/main/java/dev/gitblame/voxgolem/android/MainActivity.kt`, `app/src/main/res/xml/backup_rules.xml`, and `app/src/main/res/xml/data_extraction_rules.xml`, plus the single application of `capture.gradle.kts` in the app build script. Preserve the Activity's lifecycle and timing behavior when regenerating the scaffold. `scripts/prepare-android-capture.py` restores the Gradle hook idempotently during initialization/build. Authored shell capability resources live in `shell-res/`, and the Gradle overlay generates the voice-shortcut artwork from the canonical SVG into build output. Other launcher/theme resources remain generator-owned. Cross-platform files emitted beside `src-tauri/icons/icon.png`, machine-local SDK files, Gradle state and build products remain ignored.

Prerequisites are Bun, Rust/Cargo, Android SDK/NDK tooling supported by Tauri, and the pinned `tauri-cli 2.11.1`. Run `make android-init` from the repository root after those prerequisites are installed.

## Launcher entries

- **Vox Chat** (chat-bubble badge) opens the normal chat UI without starting a new microphone session.
- **Vox Ask** (bottom-right microphone badge) starts native recognition as soon as permission, foreground and provider admission permit. The recognizer's authoritative final utterance is sent once in the current conversation, then the voice session idles.
- **Vox Talk** (conversation-bubbles badge) follows the same pipeline, but listens again after the reply/playback until Stop or backgrounding. It does not change the persistent voice-mode preference.

Home-screen pinning is not offered; a shortcut pinned by an earlier build is disabled when the plugin loads. Phone/Wear purpose icons derive from the canonical SVG during Gradle builds; desktop and in-app emblems remain unbadged.

Ask/Talk are explicit exported launcher entry points, matching Wear's one-tap behavior. Another app can invoke them too: caller disclosure is deliberately not required. Microphone permission, visible-app admission, busy/context checks and Stop still apply. Incoming intent text or mode extras are not prompts; aliases select the policy. Saved-state/Recents replay does not mint a launch token, and repeated taps do not replace active work. Real mic startup time depends on Android/speech-provider readiness; no fixed latency is guaranteed.

## Provider access and conversation sharing

Settings provides ChatGPT device-code sign-in through the shared native owner.
The phone refreshes its own credentials and shares access-only snapshots with its
verified paired watch through a separate transient channel. Tokens never enter
JavaScript. Copied or imported credential files are not read. See the
[root sign-in guidance](../../README.md#chatgpt-device-code-sign-in) for setup,
sign-out and offline-watch limitations. Drafts survive access failures; prompts
are never replayed automatically.

Phone/Watch conversation sharing remains automatic over native Wear Data Layer
pairing. Both apps require matching package IDs and actual signing certificates,
with compatible v3 role/replica identity metadata independent of credentials. One
unambiguous opposite-form-factor counterpart is selected; unavailable access does
not prevent reconciliation. Startup, foreground resume, SDK events and bounded
jobs drive setup and conversation retries, never generation. Credential sharing
has a separate bounded protocol and does not enter conversation payloads.
See the [shared library guide](../../crates/assistant-android/README.md) for native
ownership. Linux checks do not establish physical phone/Watch readiness.

## Local optimized capture timing

Set `ORG_GRADLE_PROJECT_voxgolemCaptureTiming=true` for an optimized Android build
to enable shell profiling and content-free capture-stage timing in that artifact.
Ordinary release builds leave this opt-in disabled. It records stage names and
elapsed timings, never audio, transcripts, capture IDs, peer addresses or credentials.

## Shared typed-app benchmark

Use the shared suite controller rather than manually invoking one smoke turn per
question:

```sh
make benchmark PLATFORM=android DEVICE=a17 BENCHMARK_FLAGS="--help"
make benchmark PLATFORM=android DEVICE=a17 BENCHMARK_FLAGS="--list"
```

See `tools/benchmark/README.md` for the complete authorized local/SSH device-run
command, prerequisites, cleanup and report semantics. The shared controller owns
scenario sequencing and validation; `scripts/android-e2e-smoke.py` remains the
reused single-turn CDP observer. The computer may control the run, but the measured
application executes on the explicitly selected physical phone. Help/listing must
not access devices. No automatic installation, pairing, dependency download,
microphone test or app-data reset is part of this command.

The shared desktop adapter is not evidence of phone performance, and an Android
run does not certify desktop, Windows or Wear. Unsupported/unobserved checks must
remain explicit in the report. Use the build and clock conditions below when
interpreting results; a debuggable WebView run does not establish optimized-release
performance.

## Debug startup and response timing

Build an inspectable ARM64 debug APK with the existing JDK/SDK/NDK environment:

```sh
VITE_VOXGOLEM_TIMING=1 make android-build
```

The frontend flag opts into bounded, fixed-vocabulary timing diagnostics; it does
not change model routing or persistence. Native `VoxGolemAssistantTiming` events
are enabled in debug builds (or by explicit Assistant timing manifest metadata).
Use an explicitly verified physical transport and retain logs outside Git.

- Phone `ACTIVITY` events use Android's process-start elapsed-realtime origin.
  They separate Activity/WebView creation, plugin load, runtime creation and
  queued hydration, the initial snapshot, and frontend acknowledgements. A
  recreated Activity in an existing process is not a cold-process launch.
- `GENERATION` events use the native submission's elapsed-realtime origin and
  separate actor queueing, draft persistence, credential checking, projection,
  context preparation, provider worker/dispatch, nonblank answer receipt,
  delivery enqueue, durable commit and settlement.
- Frontend startup observations use WebView `performance.now()` from navigation.
  Request observations use one frontend admission origin. The Android first-text
  badge and completion duration must share that origin, not provider durations.
- `FRONTEND_READY_ACK` and `FIRST_RESPONSE_DOM_ACK` are **native receipt of a
  frontend observation**, including IPC delay. DOM commit, Channel enqueue and
  animation-frame callbacks are not proof of painted pixels.
- The transport first-text callback starts at HTTP dispatch for Custom, or at
  the pipe-write attempt for Agy (including lock acquisition/write/flush and
  engine processing). Android's terminal compatibility timing pair can mix
  native-admission completion with transport first text, so it is **not** emitted
  as a provider-duration diagnostic. Use the named native dispatch/text/outcome
  stages instead. Neither describes isolated network latency or verified engine
  readiness. An absent first-text timing is not an earlier streaming event.

Compare phases only within their declared clock/origin. Android events in the
same process/boot share the native elapsed-realtime observer; Rust `Instant`,
logcat timestamps and WebView performance timestamps must not be subtracted from
one another. Separate process-cold, warm resume, first provider request and reused
request trials; keep build, model, effort, synthetic history and instrumentation
conditions comparable. `scripts/android-e2e-smoke.py` requires explicit local ADB
routing and a DevTools target. It observes a newly UI-admitted request, correlated
nonblank text, authoritative successful terminal metadata, and a sleeping shell;
disabled Send, error/refusal/cancellation, or missing metadata fail closed. Its
metric is **first new answer observed**, not provider TTFB or painted pixels.
Live CDP execution requires the existing Python `websockets` prerequisite. The
synthetic observer tests do not establish actual WebView or provider acceptance.
