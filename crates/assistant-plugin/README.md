# Phone Assistant bridge

Internal Tauri mobile plugin connected to the Android shell's top-level commands
and the shared [native Assistant library](../assistant-android/README.md). It owns
transport/projection and Activity callbacks, not another history reducer, database,
provider or Note capture pipeline. Desktop commands and IDs remain separate.

## Bound runtime and frontend

[AssistantPhoneBindings](android/src/main/java/dev/gitblame/voxgolem/assistant/phone/AssistantPhoneBackend.kt)
creates one process-scoped
[EnginePhoneBackend](android/src/main/java/dev/gitblame/voxgolem/assistant/phone/EnginePhoneBackend.kt),
which uses `AssistantRuntime.get(context)` and callback-settled engine commands.
There is no placeholder command factory or volatile-history fallback. The
[Android shell](../../apps/android-tauri/src-tauri/src/lib.rs) registers the plugin
and forwards commands; [App.tsx](../../frontend/app/src/App.tsx) mounts the Android
conversation and voice controls, without credential-management settings.

The backend projects selected ancestry through
[PhoneSnapshotEncoder](android/src/main/java/dev/gitblame/voxgolem/assistant/phone/PhoneSnapshotEncoder.kt),
never a flattened graph of sibling branches. Prompt context is bounded to 20 turns /
256 KiB. Ordinary initial hydration coalesces conversation creation and preserves
an existing fresh draft; it does not reset retained history.

- **New Chat** creates a new identity and retains old conversations/drafts.
- Conversation and continuation selection validate distributed IDs and settle on
  the engine actor before returning the canonical projection. Concurrent branches
  require a deliberate head choice before Send.
- `list_assistant_conversations` and `list_assistant_heads` provide revision-checked
  pages beyond the initial 128-item projections. The React UI has **More conversations**
  and **More continuations** buttons; the projection cap is not history deletion.
- Drafts and preferences persist natively before reply. Preference/model updates
   serialize and roll back on failure; native storage errors remain explicit.
  Preferences include Boolean `web_enabled` and `speech_network_fallback`, both
  default true. The engine's installation policy is authoritative in snapshots;
  Older preference payloads missing just these new keys receive true defaults;
  invalid Boolean types and missing legacy fields still fail validation.
  `speech_language`, `speech_tts_provider`, and `speech_tts_voice` are strings with
  empty defaults; older payloads missing just these fields receive empty defaults.
  Shape validation requires a valid locale when selected and bounded provider/voice
  strings; the actor validates new selections against observed native SDK choices.
  Snapshots read the engine's installation policy, not stale duplicated preferences.
  the phone preference transaction persists/rolls back the model and both policies
  together with the existing phone preferences. These settings are not replicated
  conversation records. Speech policy changes govern future admission, not an
  unsupported claim that a provider's offline hint forbids networking.

Native durable-request-to-client-token mappings correlate terminal replies with
later snapshots. The [frontend snapshot parser](../../frontend/app/src/lib/appSnapshot.ts)
and source encoders define the contract; do not duplicate wire schemas here.

## Delivery is not rendering or synchronization

[PhoneRequestDelivery](android/src/main/java/dev/gitblame/voxgolem/assistant/phone/PhoneRequestDelivery.kt)
adapts the shared acknowledged gate. The [Rust bridge](src/lib.rs) correlates
updates by client request, provider request and revision, checks the JavaScript
Channel's `send` result and sends native `ack_delivery`. Failed enqueue retires
the request before save; lost ACKs time out. The final candidate includes a status
barrier even with no text deltas. One prompt Channel and one input Channel are
registered for the bridge lifetime; per-request sinks are released at termination.

ACK means Rust-to-WebView enqueue, **not** rendered pixels or another SQLite
commit. Only the shared engine's terminal listener reports durable completion;
polling `busy` or a JavaScript success claim cannot commit a turn. Background loss,
Stop and dropped invocation retire work; interrupted drafts require deliberate
retry. Remote conversation ACKs are a separate exact-body, per-peer protocol after
receiver SQLite commit, not this UI delivery ACK.

## Read-only provider access and automatic conversation sharing

The bridge has no credential import, status, retry or transfer commands. Native
provider execution reads existing externally injected app-private access; token
bytes do not enter JavaScript. Normal chat availability and request errors still
distinguish missing/expired access, 401 rejection and model/account 403 denial.
Drafts remain available for deliberate retry, never automatic prompt replay.

Authorized development uses external manual file injection separately on phone and
Watch, not a Settings action or repository provisioning/export script. See the
[root access guidance](../../README.md#provider-access-external-development-setup-only).
The app has no login, refresh or renewal implementation. Proper device authentication
is deferred, not available now.

Native Wear Data Layer pairing and matching package/signature isolation authorize
automatic conversation sharing. The shared setup controller exchanges v3 peer
identity independent of provider access and binds one unambiguous phone/Watch
counterpart; there is no app invitation or peer selector. `FILTER_ALL` includes
known offline peers so a second device cannot be ignored merely because it is
unreachable. SDK identity/reachability checks reject stored-node fallback.

Startup, foreground resume, SDK events and bounded jobs drive setup and conversation
reconciliation, not credential copying. Credentials never enter conversation
DataItems/Assets or the frontend. Metadata migration preserves local history,
drafts and existing provider files. See the
[replication integration guide](../assistant-android/src/main/java/dev/gitblame/voxgolem/assistant/replication/README.md)
for lifecycle ownership. Data Layer may relay encrypted traffic through Google servers;
it is not cloud-free.

## Native phone speech admission

[PhoneSpeechSession](android/src/main/java/dev/gitblame/voxgolem/assistant/phone/PhoneSpeechSession.kt)
uses shared dictation, local TTS and conversation-loop owners. Ordinary dictation
fills the composer for manual Send; explicit conversation mode uses the normal
correlated submit/ACK/commit path. Stop/background invalidates epochs, cancels
pending permission admission and retires recognition, generation, playback and timers.
Foregrounding/status polling does not restart a microphone or generation.

The library/plugin baseline is minSdk24. SDK dedicated on-device recognition starts
at API31; installed-language metadata requires API33. Default dictation is
local-preferred with permitted controlled provider fallback. Disabling fallback
requires the dedicated local-only API, not merely an installed-language report or
`EXTRA_PREFER_OFFLINE`; unsupported services leave typing available. Native manifests
provide speech/TTS queries. No models are downloaded. TTS defaults off, prefers
installed non-network voices and admits network voices only under the captured policy.
Voice status includes `speechNetworkFallback`, `localOnlyRecognitionAvailable`,
`recognitionMetadata` and `ttsMetadata`. Hidden model identities/current provider
processing remain unknown. `diagnosticError` preserves raw no-match while the normal
`error` field uses the shared quiet-no-match mapping. A failed final never auto-sends.
Ordinary dictation writes only its admitted utterance/conversation's draft; idle
teardown and late final callbacks cannot resurrect a sent draft or overwrite a new one.

Voice status adds the configured `speechLanguage`, `speechTtsProvider`, and
`speechTtsVoice` strings and a `speechOptions` object:

- `ttsProviders`: at most eight installed package strings; the empty preference
  means Automatic and need not be an array entry.
- `ttsVoices`: at most 1024 installed options, each `{provider, name, language,
  networkRequired}`. Provider `""` means the observed automatic/system engine.
  Other providers' voice inventories appear after preparing that selected provider.
- `recognitionInstalledLanguages` / `recognitionSupportedLanguages`: null when
  unreported, otherwise at most 128 normalized locale strings. Do not fill these
  from Java's generic locale list or TTS languages. Automatic remains selectable
  without support metadata; explicit unsupported/unknown recognition languages fail.
- `localOnlyRecognitionAvailable`: dedicated SDK capability, not an offline hint.
- `modelSelectionAvailable`: false; `modelIdentifiers`: null. No hidden ASR/TTS
  model identifier, downloader or new provider is exposed.

Use the normal preference transaction to submit the three strings. When changing
provider, clear the previous explicit voice deliberately or choose a voice already
reported for that provider; no other explicit provider/voice is silently substituted.
Selections and fallback policy are captured together for future native admission.
Wear uses the same installation policy and `SpeechOptions` through its thin voice
controller; it does not need a duplicate selection store in Wear presentation preferences.

`AssistantPlugin.load` now calls `AssistantRuntime.configureCaptureResourceAdmission(true)`
after shared capture-lease integration. The mounted speech controls are no longer
blocked by missing admission wiring. This does not guarantee an installed recognizer,
language, voice, permission or physical callback behavior. Note/live/replay/probe
owners share admission and must not be preempted; foreground/resource checks and
failed-retirement exclusion remain in force. SDK interruption and audio routing
still require device validation.

### New voice chat shortcut: implemented source

Settings mounts [NewVoiceShortcutSettings](../../frontend/app/src/components/NewVoiceShortcutSettings.tsx)
with **Add Ask to Home screen**. On API26+ and a supporting launcher,
[NewVoiceShortcutLauncher](android/src/main/java/dev/gitblame/voxgolem/assistant/phone/NewVoiceShortcutLauncher.kt)
requests a pin with generated artwork. The launcher asks for confirmation; accepted
request is not confirmed pinning, and installing the APK does not automatically pin it.

A fresh legacy shortcut dispatch selects Ask; the phone launcher also exposes
Chat/Ask/Talk. Voice alias identity selects Ask or Talk without resetting the
conversation or draft, accepting no incoming prompt or mode extras. These exported
entries issue a one-time process-local launch token without caller confirmation:
another app can invoke them too. Microphone permission, canonical hydration,
foreground readiness, pending draft writes and current conversation/head ownership
still gate admission. Saved-state/Recents restoration does not replay mic entry.

Ask sends one authoritative finalized utterance once through the existing
Channel-ACK/SQLite path, optionally speaks the reply, then idles. Talk is the separate
repeat-after-reply policy. **Stop cancels recognition, response and playback without
submitting**; failed/partial speech is not promoted to a final. Ordinary manual
dictation remains draft-only. Stable IDs and the legacy
[StopSendNewPolicy](android/src/main/java/dev/gitblame/voxgolem/assistant/phone/StopSendNewPolicy.kt)
remain compatibility interfaces, not the normal Ask entry. Missing provider/language
support leaves typing available; busy/permission failure does not cancel unrelated
work. Unavailable provider access retains input for deliberate retry, never automatic
replay.

Source availability does not establish physical acceptance of launcher pinning,
provider callbacks or the complete flow. Deployment needs matching source/build
identity and APK SHA-256, not an older installed/archived APK's package name.
Bundled watch-local transcription remains future backlog, not supplied here.

The separate shared `AssistantInputRuntime` provides production constructors for
watch-to-phone supplied-audio input. Its provider/device gate remains default-false
without physical calibration; this is not the local speech-admission switch above,
and constructor availability is not proof of shell/UI activation.

## Integrated build and verification

The [Cargo workspace](../../Cargo.toml) includes this plugin, and the
[phone Cargo manifest](../../apps/android-tauri/src-tauri/Cargo.toml) depends on it.
[Authored Gradle settings](../../apps/android-tauri/capture.settings.gradle) include
`:tauri-plugin-assistant`, `:assistant-android` and the shared `:voice-android` SDK
library and select the phone's ARM64 ABI.
The shared manifest supplies `AssistantApplication`, native replication
listeners, retry jobs and service visibility. JNI packaging/consumer shrink rules
belong to the shared library, not this bridge.

Run from the source checkout on Linux with JDK21, SDK36, NDK28.2.13676358,
Tauri CLI2.11.1 and `aarch64-linux-android` installed:

```bash
make android-assistant-test
make android-build
```

The test target runs shared native-library and phone bridge unit tests plus lint.
Shared tests automatically build/load Linux host JNI; the shell sets
`assistantNativeAbis` so the library is not configured with an absent ABI selection.
Host tests, Android lint and compilation do not prove ART/JNI, native launcher,
physical microphone/TTS or two-device association behavior. Keep credentials,
transcripts and runtime artifacts outside Git; use synthetic fixtures only.
