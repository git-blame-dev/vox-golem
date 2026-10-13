# Android Tauri Shell

This Android-only Tauri shell exposes canonical IPC commands and foreground lifecycle hooks through the thin `assistant-plugin` bridge. The shared `assistant-android` runtime owns durable conversations, read-only provider access and phone/Watch replication; the reusable Custom transport remains in `voxgolem-platform`. Desktop audio/models/plugins stay outside this crate. The authored `tauri-plugin-capture` mobile plugin adds native watch dictation, persistent recording/history state, permission setup and SAF notes export independently of chat. A zero-asset install still starts and reports only the affected provider capability as unavailable.

`src-tauri/gen/android` is generator-owned apart from the explicit exceptions below. Regenerate it only with the root `make android-init` wrapper, which pins and verifies Tauri CLI 2.11.1 and runs `make icons` before and after scaffold generation. The canonical artwork is `apps/desktop-tauri/src-tauri/icons/icon.svg`; `make icons` derives desktop PNG/ICO files, the tracked Android `src-tauri/icons/icon.png`, and Android launcher resources from that one SVG. The emblem fills 90% of the visible icon width. Android uses a black background with foreground and monochrome layers sized for its 72dp masked viewport; legacy launcher PNGs are rendered directly without additional inset. Wear consumes the same generated launcher resources. Build the arm64 debug APK with `make android-build`.

The generated baseline is intentionally kept separate from authored behavior. Authored exceptions under `src-tauri/gen/android` are `app/src/main/AndroidManifest.xml`, `app/src/main/res/xml/backup_rules.xml`, and `app/src/main/res/xml/data_extraction_rules.xml`, plus the single application of `capture.gradle.kts` in the app build script. `scripts/prepare-android-capture.py` restores that hook idempotently during initialization/build. Authored shell capability resources live in `shell-res/`, and the Gradle overlay generates the voice-shortcut artwork from the canonical SVG into build output. Other launcher/theme resources remain generator-owned. Cross-platform files emitted beside `src-tauri/icons/icon.png`, machine-local SDK files, Gradle state and build products remain ignored.

Prerequisites are Bun, Rust/Cargo, Android SDK/NDK tooling supported by Tauri, and the pinned `tauri-cli 2.11.1`. Run `make android-init` from the repository root after those prerequisites are installed.

## Launcher entries

- **Vox Chat** (chat-bubble badge) opens the normal chat UI without starting a new microphone session.
- **Vox Ask** (bottom-right microphone badge) starts native recognition as soon as permission, foreground and provider admission permit. The recognizer's authoritative final utterance is sent once in the current conversation, then the voice session idles.
- **Vox Talk** (conversation-bubbles badge) follows the same pipeline, but listens again after the reply/playback until Stop or backgrounding. It does not change the persistent voice-mode preference.

The existing pinned **Ask** shortcut keeps its component, action and ID and uses the same microphone artwork and single-turn policy. Its stored icon is refreshed when shortcut Settings checks its status. Phone/Wear purpose icons derive from the canonical SVG during Gradle builds; desktop and in-app emblems remain unbadged.

Ask/Talk are explicit exported launcher entry points, matching Wear's one-tap behavior. Another app can invoke them too: caller disclosure is deliberately not required. Microphone permission, visible-app admission, busy/context checks and Stop still apply. Incoming intent text or mode extras are not prompts; aliases select the policy. Saved-state/Recents replay does not mint a launch token, and repeated taps do not replace active work. Real mic startup time depends on Android/speech-provider readiness; no fixed latency is guaranteed.

## Provider access and conversation sharing

The app reads externally injected access from its app-private provider file. It
does not import documents, manage credentials, copy them to the Watch, log in or
refresh them. Missing, expired and rejected access remain normal chat availability/
error states; drafts are retained and prompts are never replayed automatically.

Authorized development setup is an external manual file-injection step on each
selected app, not a repository helper or Settings action. Publish only compatible
existing access, owner-only and complete, without printing keys or account data.
Keep developer tooling, source/staging files and runtime evidence outside Git and
preserve upstream auth and app data. The [root access guidance](../../README.md#provider-access-external-development-setup-only)
describes the boundary; proper device authentication is deferred.

Phone/Watch conversation sharing remains automatic over native Wear Data Layer
pairing. Both apps require matching package IDs and actual signing certificates,
with compatible v3 role/replica identity metadata independent of credentials. One
unambiguous opposite-form-factor counterpart is selected; unavailable access does
not prevent reconciliation. Startup, foreground resume, SDK events and bounded
jobs drive setup and conversation retries, never credential transfer or generation.
See the [shared library guide](../../crates/assistant-android/README.md) for native
ownership. Linux checks do not establish physical phone/Watch readiness.

## Local optimized capture timing

Set `ORG_GRADLE_PROJECT_voxgolemCaptureTiming=true` for an optimized Android build
to enable shell profiling and content-free capture-stage timing in that artifact.
Ordinary release builds leave this opt-in disabled. It records stage names and
elapsed timings, never audio, transcripts, capture IDs, peer addresses or credentials.
