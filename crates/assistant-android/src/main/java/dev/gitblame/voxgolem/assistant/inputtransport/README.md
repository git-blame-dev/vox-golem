# Assistant phone-assisted on-device input

This package is a separate ephemeral Assistant input lane. It does not import the
capture plugin, write recordings/WAV/Org, replicate raw audio, touch conversations,
or send chat messages. Linux/Windows desktop is not an applicable native adapter;
the shared Android library hosts the phone processor and watch source.

## NativeRuntime integration contract

All controller methods and terminal/draft callbacks run on one Android **main
dispatcher**. The runtime must call `tick()` at least every 250 ms while a session
exists. No method waits synchronously for recognition results.

- `CallerForegroundSignal.isForeground(): Boolean` must reflect a resumed native
  Assistant window. It must not equate an existing service, explicit loop preference,
  or Note foreground-service state with foreground eligibility. This implementation
  declares **foreground-only** phone availability; it adds no new service policy.
- `SourcePeerBinding.accepts(node, source, target): Boolean` binds the transport's
  actual remote node to the selected, enrolled, signed app installation (`source`)
  and this local installation (`target`). It must fail for an unselected/replaced/
  revoked peer. UUID frame fields alone, proximity, and reachability are not trust.
  This is an adapter seam for the native peer owner, not an alternate registry.
- Both owners pass `RecognitionResourceLease.process` (the default) or the same
  injected lease used by local recognition/Note. Both use `Kind.CHAT`; neither
  preempts or transfers a Note token. Failed teardown holds admission until the
  runtime explicitly calls `retryFailedRetirement()` on the main dispatcher.

### Phone

Construct `PhoneInputController(AndroidSuppliedAudioFactory(context, deviceGate),
foreground, peers, lease, reply, now, onOutcome)` with named arguments where useful.
`reply(node, frame)` delegates to the transport's `control` method. Its SDK listener,
support query, creation/start/cancel/destroy calls stay on main; only bounded PCM
pipe writes run on a worker. API <33, no on-device provider, missing SDK permission,
uninstalled exact language, support errors and an unapproved device are unavailable.

Wire `GmsAssistantInputTransport` callbacks:

| Callback | Phone target |
| --- | --- |
| `onControl(node, frame)` | `phone.receive(node, frame)` |
| `authorizeAudio(node, session)` | `phone.acceptsAudio(node, session)` |
| `onAudio(node, session, pcm)` | `phone.audio(node, session, pcm)` |
| `onSourceEnded(node, session)` | `phone.sourceEnded(node, session)` |
| `onFailure(node, session)` | `phone.transportFailure(node, session)` |

`stop(session): CompletableFuture<InputOutcome>` completes with an authoritative
SDK final or a non-authoritative retained draft/error. A remote STOP Message only
arms an end timeout: the ordered Channel end marker must arrive before the supplied
pipe drains/closes. Closing a broken Channel is an error, never a fabricated final.

### Watch

Construct `WatchInputController(transport, AndroidWatchMicrophoneFactory(context),
foreground, peers, nativeAvailable, lease, onDraft, now, onOutcome)`.
`nativeAvailable(language)` must describe native on-device availability for the
exact requested language. Native availability wins; the fallback never starts then.
`start(selectedPhoneNode, localInstallation, phoneInstallation, language)` returns
a session UUID or null (use typing). No microphone is opened until both phone READY
and successful Channel opening. The AudioRecord factory rejects non-watch devices.

Wire `onControl` to `watch.receive` and `onFailure` to `watch.transportFailure`.
The watch does not accept incoming audio: use `authorizeAudio = { _, _ -> false }`,
`onAudio = { _, _, _ -> false }`, and a no-op `onSourceEnded` callback.
`onDraft` and `onOutcome` are projection/status notifications **not send actions**.
`watch.stop(session)` returns a future and waits for FINAL; only the user's send
action or an explicitly authorized active loop may send its authoritative text.
Empty/early finals, partials, errors and cancellation never authorize sending.

Home, dismissal, `HomeNativeWindow` exit, interruption and disconnect must call the
relevant controller's `interrupt()`/`transportFailure()` promptly. On window exit,
interrupt the controller **before** closing its transport and clearing runtime
bindings. Never auto-resume input when a window returns. Keep callbacks associated
with the original conversation in the runtime; this package owns no conversation
selection or durable draft storage.

## Provider/device gate (required, default false)

`SuppliedAudioDeviceGate.permits(language)` must be a fresh, fail-closed answer for
the actual selected on-device provider/package version, OS build and exact intent
(language, mono PCM16LE 16 kHz, supplied pipe, segmented end). It is rechecked at
support/start, PCM admission and SDK event delivery. No model download is requested.

Android's public support response lists installed languages, but **does not attest
external-audio acceptance, segmentation, offline execution or absence of phone-mic
use**. The public SDK also does not expose a reliable identity for the actual
on-device provider selected internally. A support response or API level cannot
establish this approval. Runtime owners must supply a provider-bound capability
check backed by physical-device evidence and invalidate it on provider/OS changes;
if they cannot, leave the gate false and present typing.

`RecognizerIntent.EXTRA_AUDIO_SOURCE` explicitly documents that an unsupported
provider may open its microphone instead. There is no generic `createSpeechRecognizer`
fallback here. Calling `createOnDeviceSpeechRecognizer` plus `EXTRA_PREFER_OFFLINE`
is not advertised as a universal supplied-audio/no-phone-mic guarantee.
Physical acceptance must prove injected synthetic audio is recognized, speech near
the phone without injected audio is not captured, phone-mic access does not occur,
network-disabled behavior, exact installed language, partial/segment/final ordering,
source EOF, interruption, backpressure and bounded cleanup. No such device test was
performed as part of this package implementation.

References:
- [Android RecognizerIntent API](https://developer.android.com/reference/android/speech/RecognizerIntent#EXTRA_AUDIO_SOURCE)
- [Android 13 RecognizerIntent source](https://android.googlesource.com/platform/frameworks/base/+/refs/tags/android-13.0.0_r1/core/java/android/speech/RecognizerIntent.java)
- [SpeechRecognizer API](https://developer.android.com/reference/android/speech/SpeechRecognizer)

## Build-owner requirements (not changed here)

- Reuse included `play-services-wearable:19.0.0`. Phone and watch Data Layer apps
  require matching application IDs and actual signing certificates.
- Advertise distinct `voxgolem_assistant_input_v1` via each shell's
  `android_wear_capabilities` string array in its Wear capability resources. START
  and Channel opening require the exact selected node to advertise this capability.
- Watch: declare/request `android.permission.RECORD_AUDIO` for the explicit
  foreground source. Phone: declare/request the same permission because Android's
  SpeechRecognizer SDK requires it even with supplied audio; this is not permission
  to capture the phone mic. Without provider/device proof above, refuse processing.
- Add/retain manifest `<queries><intent><action
  android:name="android.speech.RecognitionService" /></intent></queries>` for SDK
  provider visibility. No capture-plugin import or private factory reuse is needed.
- **No new SDK service or manifest service declaration is required** for this
  foreground-only implementation: it registers dynamic MessageClient and ChannelClient
  listeners. There is no WearableListenerService, foreground-service permission,
  Note FGS reuse or phone microphone service in this package. If background phone
  eligibility later requires a service, the assistant runtime/build owners must
  assess the approved connectedDevice policy separately; do not set foreground true
  merely to bypass this boundary.

## Wire and bounds

Controls use `/voxgolem/assistant/input/v1/control`: versioned binary magic `VGI1`,
kind, canonical session/source/target UUIDs, canonical language, monotonic per-side
sequence, text/error. Kinds are START, READY, PARTIAL, FINAL, STOP, CANCEL, ERROR.
Frames are strict UTF-8, at most 9000 bytes; text at most 8192 bytes. Recent terminal
session IDs fence duplicate/delayed STARTs in bounded process memory only.

PCM uses Channel path `/voxgolem/assistant/input/v1/pcm/<session UUID>`, bound to
the active session's actual node. Unsigned big-endian 16-bit frame length precedes
each 2..640-byte aligned PCM16LE chunk; zero length is the explicit end marker.
Raw Channel EOF without that marker is failure. Audio stays transient: two bounded
25-chunk queues, nonblocking SDK pipe writes with a 250 ms write budget, 1 s Channel
operation timeout, 2 s GMS Task timeout, 5 s admission/audio-idle timeout, 3 s phone
final/source-end timeout and 60 s session ceiling. Main handoff holds only one PCM
frame and waits at most 250 ms. Late successful Tasks close their stream/Channel.
The global Channel callback ignores foreign namespaces, including Note/archive.
Normal end closes only the stream direction and retains the accepted Channel handle
until an accepted peer terminal invokes cancellation, local FINAL/ERROR delivery
succeeds, or a 5 s terminal deadline expires. Failure/cancellation aborts the whole
owned Channel; stream close itself has the same 1 s operation deadline.
An SDK final racing a supplied-pipe close waits for atomic successful EOF publication
(at most 250 ms). Failed/stalled close cannot promote it; genuinely early finals fail.
Workers are not replaced while an earlier Channel worker is still alive. No claim
is made that arbitrary vendor SDK/native calls can be forcibly terminated; failed
microphone/recognizer retirement retains the shared lease and disables new input.

## Verification

Package-local JUnit and Robolectric tests use only synthetic text/PCM and SDK fakes.
They exercise strict codecs/framing, bounded queue cloning/overflow/cancellation,
source/end ordering, defaultStop final/error semantics, remote SDK error 21,
late support/events, exact installed language and descriptor intent, main dispatch,
native-first selection, Note lease exclusion and failed-retirement retry.
Real adapter tests inject private client hooks with actual GMS Tasks, delayed readers
and controlled pipe-close callbacks to cover namespace ownership, normal half-close,
terminal deadlines, late stream cleanup and successful/failed/racing EOF publication.
Cached standalone Kotlin 1.9.25/Java compilation against Android 36 and GMS 19
avoids Gradle/JNI/shared build outputs. These checks do not establish live GMS,
phone background eligibility or physical-device provider/microphone behavior.
