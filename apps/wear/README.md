# VoxGolem Wear: Note and Assistant

An independent native Wear OS Gradle root with four launcher entries:

- **Vox Note** is the Note recorder; it never sends Assistant prompts.
- **Vox Chat** is foreground Custom-provider chat with its own local
  credential, durable conversations and network requests. Once provisioned, it
  does not need the phone online to generate an answer.
- **Vox Ask** preserves the current conversation/draft, sends one authoritative finalized utterance once, optionally speaks the reply, then idles.
- **Vox Talk** uses that same pipeline and reopens recognition after reply/playback;
  it does not create a new chat. Stop cancels without sending.

The shell uses Kotlin/Compose, not Tauri or a WebView. It directly depends on the
ordinary [capture-android](../../crates/capture-android/README.md) and
[assistant-android](../../crates/assistant-android/README.md) and
[voice-android](../../crates/voice-android/README.md) libraries. It does not
compile the phone plugins or copy their source files. The shared Assistant library
packages `libwear_assistant_native.so`; this APK is `armeabi-v7a`/ARM32, using Rust
target `armv7-linux-androideabi`, not the phone's ARM64 target.

## Build and verification

Run these [root Makefile](../../Makefile) targets from the source checkout on Linux:

```bash
make wear-test
make wear-build
make wear-performance
```

Use JDK 21 (`JAVA_HOME` and its `bin` on `PATH`), SDK platforms 36 and 37.0 with
build tools 36 (`ANDROID_HOME`), NDK 28.2.13676358 (`NDK_HOME`) and the ARMv7 Rust target. Native
builds use cached Cargo dependencies with `--offline --locked`. `wear-test` runs
shared voice/Assistant/capture and shell unit tests and Android lint; common voice
tests do not build JNI. Assistant JNI unit tests load
the actual Linux host library, not ART on the watch.

The Wear UI uses Wear Material 3/Foundation 1.7.1 with Compose 1.10.0. Its separate
root uses AGP 9.1.1, Gradle 9.3.1 and Kotlin/Compose compiler 2.2.10; the app compiles
against stable API 37.0 while keeping target SDK 36 and the existing ABI/signing.
Wear-local legacy Kotlin/DSL compatibility options preserve the shared libraries'
phone AGP 8.11 path; these supported transitional options must be migrated before
AGP 10 rather than treated as permanent configuration.

Assistant and Note share one Material 3 theme/AppScaffold. Normal paragraphs use
native rectangular wrapping and ordinary lazy-list reading, not a custom circular
text engine. ScreenScaffold supplies responsive content padding and the bottom
ExtraSmall edge action. The shared compact control renders at80% of its original
footprint. Assistant keeps primary/Stop clearance while idle/active work needs it;
when reading older idle content, Latest floats transparently over text with one48dp
target rather than an opaque footer. The final line remains reachable. Note follows
new transcript content without rearming solely on viewport/padding changes.
Rotary, large-font and round-bezel painting remain target-device acceptance checks.

APKs are under `app/build/outputs/apk/debug/` and `app/build/outputs/apk/performance/`.
The performance variant uses release Rust, R8/resource shrinking, is
non-debuggable and uses the local debug signing key. It is an optimized local
trial, not a production release. Generated launcher resources derive from the
[canonical SVG](../desktop-tauri/src-tauri/icons/icon.svg).

Phone and watch use application ID `dev.gitblame.voxgolem.android`; actual signing
certificates must match for Wear Data Layer access. Merely having the same ID or
both calling a build “debug” is not certificate verification. Build commands do
not authorize deployment, clearing data or switching devices.

## Assistant interaction and shared conversations

[AssistantActivity](app/src/main/java/dev/gitblame/voxgolem/android/wear/assistant/AssistantActivity.kt)
uses the shared `AssistantRuntime`, engine, SQLite store and Rust domain. Keyboard
input fills a persisted draft; **Send** is deliberate. The native screen provides
model selection, New, Chats, explicit branch/head choices, provisional-versus-stored
output and Stop all. Settings retains model, speech and Note controls, not credential
management or identity labels. Multiple continuations remain
branches; provider context follows the selected ancestry rather than flattening them.
Leaving the foreground cancels active work; returning never replays a prompt.

Watch **Read replies aloud** and **Conversation mode** default to enabled.
Their native `assistant-runtime` settings (`read-replies-aloud` and
`conversation-mode`) persist explicit on/off choices across restarts and in-place
APK updates. Conversation mode changes the Mic action; the ordinary Assistant entry
does not start recording. Reply speech is applied after foreground admission, using the
existing installed-provider policy and capability handling.

The natively paired phone and Watch automatically share conversations
through matching-package/signature Wear Data Layer isolation. No app invitation,
peer selection or confirmation is needed. Compatible v3 role/replica
metadata establishes one opposite-form-factor counterpart. All known capability
peers, including offline devices, are considered; ambiguity blocks setup rather
than selecting whichever device is online. Peer identity is independent of provider
access; missing or expired access does not disable conversation synchronization.

Startup/resume, SDK events and bounded retry jobs drive conversation sharing. Each
app reads its own externally injected app-private access file; there is no phone
import/copy, credential status/settings panel, login or refresh workflow. Normal
chat reports unavailable, expired or rejected access while retaining drafts for
deliberate retry. Authorized development uses external manual file injection on
each selected app, with private tooling/staging outside Git and no token output;
see the [root access guidance](../../README.md#provider-access-external-development-setup-only).
Proper device authentication is deferred, not currently available. Migration must
preserve local histories, provider files and drafts.

The same conversation IDs and completed turns reconcile through finite GMS
DataItem/Asset exchanges after automatic setup. Drafts, selected heads, running requests,
provisional text, credentials and database snapshots are not conversation payloads.
Exact per-peer ACKs follow local SQLite commit. No sync event starts remote generation.
The [shared library guide](../../crates/assistant-android/README.md) owns those rules.
Data Layer can route through encrypted Google servers; neither transport nor remote
Custom chat should be described as cloud-free.

### Native speech, without a bundled model

Ask, Talk and manual draft dictation prefer an installed matching language using native
recognition services. Controlled provider fallback is permitted by default; a normal
vendor service can use network processing, and an installed-language report or offline
preference is not proof of the current route. Turning off **Speech network fallback**
requires the dedicated on-device API; devices without that capability retain keyboard
input rather than silently using an offline hint. No voice model is downloaded by the
app. The keyboard's own microphone belongs to the installed IME and is not guaranteed
offline.

**Speak replies** retains the Wear installation's choice and defaults on for new
settings. Installed non-network voices are preferred; network voices are eligible
only when the captured fallback policy permits them. Settings offers only reported
language, engine and voice choices, with Automatic recovery when a saved choice is
unavailable. Configured choices, selected/reported identities and unknown model or
processing information are shown separately. Talk uses the shared foreground-only
dictation → one committed generation → optional TTS → next-utterance loop; Ask stops
after the reply. Manual dictation remains draft-only. Stop
invalidates its epoch, retires recognition/generation/playback and cancels next-turn
timers. It does not resume automatically after foreground loss or an SDK error.

**Vox Ask** opens the existing Assistant and requests a single finalized voice turn
in its current conversation/draft after foreground, permission and ownership checks.
Microphone start/end
feedback uses the same audio attributes as reply speech; user volume and mute settings
are not changed. Vendor/keyboard sounds are outside the app-owned cue player.

**Vox Talk** is a distinct launcher alias for the same singleTop
Assistant activity. One fresh launch requests the existing conversation loop
only once the screen is visible/resumed and the native current chat/draft is admitted
to the foreground. Busy generation, active voice, a pending editor or permission
request prevent a duplicate start; microphone and native capability checks still
apply. Neither alias changes the persisted **Conversation mode** choice, creates a
new chat, retries a blocked launch later, nor replays on recreation or ordinary resume.
Watch launcher caller verification was explicitly removed to permit direct automatic
start: another app can invoke these exported entries when microphone permission is
already granted. This is not silent background capture; the visible foreground and
ownership checks remain. Phone unverified shortcuts still require an in-app tap.

While the Assistant is resumed, its native window requests **keep screen on for up to
two minutes of inactivity**. Real touch or rotary input resets the window; changed
dictation text/phases, generation progress and reply/loop transitions reset the
window. Equal snapshots, link polling, sync/history changes and capability
metadata do not. Idle expiry releases the window flag without finishing the activity,
leaving normal Wear OS timeout/ambient behavior in control. A fresh touch/rotary
interaction, newly observed meaningful progress or resume starts a new window;
progress queued before expiry or foreground loss cannot revive it. Pause, stop and
destroy release immediately. This uses no permanent wake lock or global device setting
and does not guarantee overriding vendor
wrist-down, palm-cover or power gestures; those behaviors require watch acceptance.

Note recording, Assistant dictation and Assistant playback share process-local audio
admission. Assistant does not preempt Note; busy resources are an explicit error.
Note likewise cannot acquire an Assistant-owned resource. Actual SDK interruption,
audio focus/routing and teardown behavior still require watch testing.

## Note recording and phone export

Select **Vox Note** as Samsung knock-knock's launcher target. A deliberate
launch starts one capture after microphone/setup checks; ordinary resume does not.
The capture service writes local PCM/WAV independently of phone connectivity. Manual
Stop and calibrated arm-down stopping are supported, with a ten-minute maximum.
Ambient events are diagnostic-only; no wake-word or silence stop is used.

The listening screen has a simple Stop control. Only a verified local `SAVED` record
shows the existing four-second **SAVED** feedback, then the exit animation returns to
the watch face. This does not prove phone archival or Org export. Leaving the Note
UI stops/finalizes the recording; non-normal stop reasons retain `INTERRUPTED` audio,
not a false `SAVED` success. Repair remains explicit.

Note captures the shared installation language/fallback policy at Start. Live phone
transcription and saved-WAV replay use that captured policy, not later phone settings.
Disabled fallback requires the dedicated on-device route; permitted ordinary providers
may use networking. Provider acceptance of supplied Watch PCM and microphone isolation
are not proven by availability or READY. The UI distinguishes recording, connection,
recognizer readiness, returned words and retained audio with pending/unavailable text.

Live preview may be dropped without dropping local recording. Completed WAV transfer
requires exact length/hash validation and a matching receipt; channel completion
alone is not archival. Outbox retries use remote status and bounded backoff; the watch
never automatically deletes its recordings. Phone permission, recognition and verified
SAF/Org export behavior are documented in the [capture API guide](../../crates/capture-plugin/API.md),
with canonical contracts in the shared library rather than copied wire schemas here.

## Acceptance status

Deployment must match source/build identity and APK SHA-256, not just an archived or
installed package name. Automatic setup, conversation reconciliation and usable
provider access require matching signed artifacts and runtime acceptance; this
guide does not claim those checks have succeeded. Credential transfer acknowledgements
are not part of the current app.
Linux tests/builds alone do not establish ARM32 ART/JNI execution, installed speech
support, gesture accuracy, foreground interruption or offline/reconnect sync.

The phone's [voice chat shortcut](../../crates/assistant-plugin/README.md#new-voice-chat-shortcut-implemented-source)
is implemented in source and requests launcher confirmation, but is not physically
accepted. It is not a Wear shortcut or a bundled watch recognizer. The distinct
watch-to-phone supplied-audio Assistant lane has production runtime constructors;
its provider/device calibration gate still defaults false, so constructor availability
does not establish an activated UI feature. Future bundled watch-local ASR remains
outside the current implementation; task coordination stays outside this Git checkout.
