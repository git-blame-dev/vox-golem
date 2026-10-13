# Shared Android/Wear Assistant

Ordinary Android library used by the phone's thin Tauri
[assistant-plugin](../assistant-plugin/README.md) and the native
[Wear shell](../../apps/wear/README.md). It owns native runtime, persistence,
read-only provider access, replication and conversation-bound speech adapters. SDK
speech facilities and the backing installation-preference store are exported from
[voice-android](../voice-android/README.md). It contains no WebView UI or Note
capture/export pipeline. The phone and watch use the same implementation, not
parallel history/provider reducers.

## Ownership and canonical sources

| Owner | Responsibility |
| --- | --- |
| [Rust durable domain](../core/src/durable_conversation.rs) | Immutable operations, snapshot restore/validation, revision-pinned mutation plans, ancestry, branches, tombstones and quarantine |
| [Headless JNI domain gateway](../wear-assistant-native/src/domain.rs) / [Android gateway](src/main/java/dev/gitblame/voxgolem/assistant/AssistantDomain.kt) | Bounded native calls into core; no credentials, provider execution or in-memory history authority |
| [AssistantStore](src/main/java/dev/gitblame/voxgolem/assistant/AssistantStore.kt) | App-private SQLite authority: identity/counter allocation, compare-and-commit, drafts, selection, outboxes and exact receipts |
| [AssistantRuntime](src/main/java/dev/gitblame/voxgolem/assistant/runtime/AssistantRuntime.kt) / [AssistantEngine](src/main/java/dev/gitblame/voxgolem/assistant/runtime/AssistantEngine.kt) | One process engine/IO actor, foreground request lifecycle, correlated delivery and committed projections |
| [Provider adapter](src/main/java/dev/gitblame/voxgolem/assistant/runtime/AssistantProviderNative.kt) | JNI worker calling the existing Rust Custom client; no duplicate provider implementation |
| [AssistantApplication](src/main/java/dev/gitblame/voxgolem/assistant/replication/AssistantApplication.kt) | Cold-start native association/replication setup; does not create an engine or initiate generation |
| [Common voice library](../voice-android/README.md) | SDK speech, cues, resource leases and lightweight preference backing; no Assistant/JNI dependency |

These source types define the contract; documentation is not a second schema.
Snapshots are trusted committed database state, not a peer import API. Native storage
commits core's plan only against the matching revision, including purge/outbox work,
before publishing success. Conflicts recompute the plan without repeating inference.
Pending ancestry and quarantined records cannot form prompt context. Concurrent
continuations remain branches and require an explicit head choice.

`assistant.db` and replication identity/state live under app-private
`noBackupFilesDir`; provider access is separately app-private and backup-excluded.
Installation UUIDs/counters are durable and backup-excluded,
not derived from wall time, GMS nodes or copied device identity. Local drafts,
selection and interrupted state are not shared conversation content.

## Requests and completion

The engine pins request identity, context revision, parent and ancestry before the
provider call. Streaming text is provisional. The correlated delivery gate must
settle successfully before a validated completed turn can be committed and published.
Phone delivery ACK means Rust-to-WebView enqueue, not proof of browser rendering or
another database commit. Stop, background loss and delivery failure retire active
requests; retained drafts require deliberate retry. Sync never starts generation.
Each device executes its own Custom request when foreground, provisioned and online.
Cancellation is ordered at Store transaction admission. Stop before admission prevents
the completion transaction; once admitted, the SQLite transaction wins (or rolls back
on its own failure), including its publication and consumption of that exact draft.
Stop cannot undo a committed answer. A successful `completeTurn` return is saved
immediately; later selection/projection failure reports `saved_local_state_failed`,
not `not_saved`. That warning pauses speech/automatic next-turn admission.
Desktop local models, wake/VAD/Parakeet, OpenCode subprocesses and Deep/Review are not
part of this native mobile runtime.

`NativeAssistantPolicyStorage` adapts the common installation-preference backing for
installation-local `web_enabled` and
`speech_network_fallback`, both defaulting to true. `AssistantClient.setWebEnabled`
and `setSpeechNetworkFallback` change that authoritative policy through the actor.
They are not conversation operations and are never replicated. A provider request
captures `webEnabled` once at admission. `externalSessionId` is a random UUID from
a process-local 256-entry LRU, stable while that conversation remains cached, never
the canonical conversation ID and never persisted. The JNI gateway owns web-off
tool/transport enforcement; Quick/Verify/Research are separate from permission,
and Verify/Research are explicit lookup requests rather than keyword detection.

Supported speech selections use installation-local strings `speech_language`,
`speech_tts_provider`, and `speech_tts_voice`; each defaults to the empty string
(system language / automatic engine / automatic installed voice). `AssistantState`
projects these as `speechLanguage`, `speechTtsProvider`, and `speechTtsVoice`.
`AssistantClient.setSpeechSelection(SpeechSelection)` validates observed choices
before persistence. Boolean setters preserve those selections. `publishSpeechOptions`
supplies the actor with immutable, bounded SDK inventories, not replicated records.
Recognition and playback each capture one `SpeechAdmission` at their admission
boundary. A settings change never substitutes selections midway through an utterance
or chunk chain. Removed/unsupported selections fail again at native admission.

`SpeechOptions` exposes at most eight installed TTS providers and eight cached
provider inventories of at most 128 installed voices each (1024 total). Voice options
carry their provider selection key, SDK voice name, language, and network requirement.
The empty provider key denotes the automatic/system engine; nonempty keys are SDK
package choices. Inventories for other providers become known after their selected
provider's preparation completes. Select a provider, wait for updated options, then
select one of its reported voices; a combined `SpeechSelection` can clear the old
voice explicitly when changing providers.

Recognition installed/supported languages are bounded to 128 normalized locales;
null means the SDK has not reported an inventory, not that every locale is supported.
Until support metadata arrives, only Automatic language is selectable. An explicit
language must also pass the current session's support check; provider fallback does
not turn unknown/unsupported explicit language into a successful admission. The
existing AU installed-language matching is retained, and network-off still requires
the dedicated on-device capability. No model download or new speech provider exists.
Hidden model identifiers are explicitly unknown and not selectable.

An explicit TTS provider uses only that installed package, and an explicit voice
uses only that voice name under the selected provider/language/network policy.
`ExplicitTtsProviderContext` refuses bindings to another package even if Android's
TTS constructor attempts an implicit engine fallback. Automatic engine/voice
selections retain the existing local-preferred fallback behavior. Engine preference
and selected voice metadata are still not a vendor-reported hidden model identity.

New turns record `typed-custom-search` instruction profile version `2` because the
integrated app-owned plain-response policy changes that instruction profile. Existing
immutable turns and their version `1` provenance/body hashes are not migrated or rewritten.

## Replication and credentials are separate boundaries

[Replication implementation](src/main/java/dev/gitblame/voxgolem/assistant/replication/AssistantReplication.kt)
and [transport](src/main/java/dev/gitblame/voxgolem/assistant/replication/GmsReplicationTransport.kt)
use finite, bounded GMS DataItem control slots and Assets for immutable records.
Conversation creates, completed turns and deletion evidence reconcile, not drafts,
provisional deltas, running requests, whole snapshots or retired ledgers. Receiver
validation/SQLite commit precedes exact ID/body ACK; the sender durably records each
peer receipt before confirmation. Inventory revisions, missing ancestry and retries
are explicit; an SDK Task completing is not proof of remote persistence.

[Peer setup](src/main/java/dev/gitblame/voxgolem/assistant/replication/AssistantPeerSetup.kt)
uses native Wear Data Layer pairing and matching package IDs/actual signing
certificates as the trust boundary for automatic conversation sharing.
The v3 Hello carries peer identity independent of credentials. One
opposite-form-factor counterpart is supported. `FILTER_ALL` considers known offline
peers as well as reachable ones; multiple candidates block setup rather than choosing a device by reachability
or list order. Missing metadata, incompatible versions, SDK discovery failure and
storage failure have distinct diagnostics.

Peer bindings and replica IDs are bookkeeping, not tokens or another authorization
ceremony. Native replication persistence owns these identities; credential storage
does not participate in enrollment or routing. Migration clears obsolete
setup metadata, not personal history, drafts, provider files, backups or
upstream access sources. Startup/resume, SDK events and bounded jobs drive setup
and conversation reconciliation. Fresh SDK identity and reachability are required
for routing, never persisted node hints. The
[integration guide](src/main/java/dev/gitblame/voxgolem/assistant/replication/README.md)
owns lifecycle and wire details. Target fields are routing metadata, not selective
encryption among installations sharing the same package/signature. GMS may carry
encrypted traffic through Google servers; this is not a cloud-free guarantee.

Provider execution consumes existing externally injected access read-only from an
app-private file. The app does not import, store updates, copy credentials, log in,
refresh or renew access. There is no credential status/settings panel or transfer
protocol. Missing/expired access, 401 rejection and account/model 403 denial remain
normal provider availability/error states; drafts survive for deliberate retry.
Access availability does not gate conversation reconciliation.

Authorized development uses external manual file injection on each selected app,
with complete owner-only publication and no secret/account output. Keep developer
tooling and runtime files outside Git; see the
[root access guidance](../../README.md#provider-access-external-development-setup-only).
Proper device authentication is deferred. Existing provider files and upstream
auth must be preserved, not deleted during code removal or metadata migration.

## Speech admission and current wiring

[SharedDictationOwner](../voice-android/src/main/java/dev/gitblame/voxgolem/assistant/recognition/SharedDictationOwner.kt)
owns one foreground utterance; ordinary dictation fills a draft and never sends it.
[AndroidLocalRecognizerFactory](../voice-android/src/main/java/dev/gitblame/voxgolem/assistant/recognition/AndroidLocalRecognizerFactory.kt)
prefers installed-language recognition (including the existing Google AU path),
with permitted provider-controlled fallback by default. An installed language or
`EXTRA_PREFER_OFFLINE` is not proof of the current utterance's locality. Disabling
network fallback admits **only** the dedicated Android on-device API; ordinary
Google/vendor services remain unavailable under this policy even if they report
installed languages. API31/32 dedicated services are supported without inventing
installed-language metadata. Missing capability/languages leave typing available.
No model download is attempted. Manifests declare speech/TTS service queries.

[LocalSpeechOutput](../voice-android/src/main/java/dev/gitblame/voxgolem/assistant/speech/LocalSpeechOutput.kt)
defaults off, prefers installed non-network voices, and respects audio focus and
foreground loss. Network voices are eligible only when the captured fallback policy
permits them; disabling fallback also prevents reuse of a cached network voice on
the next playback admission. Native TTS splits long answers into sequential chunks
strictly below the platform UTF-16 input limit without splitting surrogate pairs;
one playback epoch retains focus/resource admission until the final chunk or failure.
Stop, shutdown, replacement and late SDK callbacks cannot advance an old chunk chain.
The explicitly started
[ConversationLoopController](src/main/java/dev/gitblame/voxgolem/assistant/speech/ConversationLoopController.kt)
allows one generation per final utterance, optional TTS and then a next-turn timer.
Stop invalidates the epoch and retires recognition, generation, playback and timers;
foregrounding or SDK errors do not restart the loop.
Phone/Wear consume answer-only `speechText` when supplied, preserving sources in
the canonical answer. Normal no-match (`recognition_error_7`) remains a failed
recognition, not a successful final or automatic Send. `recognitionUiError` hides
only that warning; partial text, retirement and raw `diagnosticError` remain.
Route metadata distinguishes requested/selected/reported/installed facts, engine
preferences and unknown processing/model identities; no historical offline trial
is asserted as proof of today's utterance. Microphone cues play once at SDK readiness
and once at successful finish, with cancellation/background/close fencing.

[RecognitionResourceLease](../voice-android/src/main/java/dev/gitblame/voxgolem/assistant/recognition/RecognitionResourceLease.kt)
serializes Assistant speech/playback with Note/live/replay/probe owners, without
preemption. Failed SDK teardown retains admission until retirement succeeds. The
phone's [AssistantPlugin.load](../assistant-plugin/android/src/main/java/dev/gitblame/voxgolem/assistant/phone/AssistantPlugin.kt)
now enables `captureResourceAdmissionReady` after shared capture-lease integration.
Wear `AssistantActivity` also enables its process gate. This source-wiring blocker
is resolved; foreground, microphone permission, installed provider/language and
resource availability are still checked, and physical speech behavior is unverified.

The phone's single-turn voice shortcut is implemented in source; see the
[bridge guide](../assistant-plugin/README.md#new-voice-chat-shortcut-implemented-source).
Pinning requests launcher confirmation. Fresh phone Ask/Talk entries do not require
an additional caller-confirmation tap; another app can invoke those exported entries
too. Microphone permission and foreground/readiness/context ownership checks remain.
Ask uses the current conversation/draft, sends one authoritative final once,
optionally speaks, then idles. Talk alone repeats after reply/playback. Stop cancels
without sending; legacy Stop & send remains a compatibility command, not the normal
entry policy.

The distinct [AssistantInputRuntime](src/main/java/dev/gitblame/voxgolem/assistant/inputtransport/AssistantInputRuntime.kt)
now has production phone/watch constructors and creates `PhoneInputController` in
the phone role; it is no longer test-only code. That does not establish shell/UI
activation. Its supplied-audio provider/device gate still defaults **false** without
calibration backed by physical evidence. Installed-language support alone cannot
prove external-PCM acceptance, offline execution or no phone-mic fallback. The lane
remains foreground-only and unavailable without those checks; typing remains the
safe path. It is separate from the enabled local phone-speech admission gate.

## Verification

From the source checkout, with JDK 21, SDK36, NDK28.2.13676358, pinned Tauri CLI2.11.1
and the shell's Rust target installed:

```bash
make android-assistant-test
make android-capture-test
make wear-test
make android-build
make wear-build
```

Shared JNI packaging is defined in [build.gradle.kts](build.gradle.kts): ARM64 phone,
ARM32 Wear, Linux JVM library for unit tests, and shared JNI consumer shrink rules.
Linux unit tests/lint/builds do not prove ART, SDK/provider availability, physical
audio behavior, signing compatibility or disconnected/reconnect acceptance. Those
remain coordinated device gates, not implicit consequences of source integration.
