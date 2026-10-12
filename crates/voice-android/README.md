# Shared Android voice facilities

`voice-android` owns Android SDK speech contracts and resource lifetimes for phone
and Wear. It has no production dependency on Assistant runtime/JNI, GMS transport,
capture storage or Compose. Existing JVM packages/class identities are preserved;
the library resource namespace is `dev.gitblame.voxgolem.voice`.

## Ownership

- Speech selections, locale/PCM bounds and observed provider/route metadata.
- Recognition construction, dictation sessions, supplied-audio SDK ports and bounded
  pipe queues, with distinct admission/proof requirements for each input profile.
- One process-local recognition/playback lease and failed-retirement retry owner.
- Local TTS/audio focus, microphone cues and the sole generated cue-resource task.
- Lightweight installation preferences backed by the existing `assistant-policy`
  file and keys. Note reads an immutable Start snapshot; Assistant retains actor
  validation and publication. Shared reads/writes serialize with failed-commit
  rollback through one backing-file monitor.
- Bounded monotonic timing diagnostics and SDK callback shrink rules.

Assistant retains conversation/context ownership, Ask/Talk completion policies,
generation, canonical persistence and JNI. Capture retains recording metadata,
PCM/WAV storage, transport/archive/export and Note lifecycle. Shells explicitly
compose those owners; shared SDK code does not discover a global recording factory.

## Safety and performance boundaries

Recognition/TTS lifecycle operations follow the SDK's main-thread requirements.
PCM recording and bounded pipe delivery remain worker-owned. Moving code does not
authorize extra executors, eager warming, model downloads or simultaneous microphone
owners. Foreground readiness, language/privacy policy, cancellation and actual
resource retirement remain distinct checks.

Provider availability or an input-source extra does not prove supplied-PCM acceptance,
offline execution or microphone isolation. Recorded Assistant admission remains
default-disabled with an unproven provider gate; Note's existing live/replay attempt
profile is not evidence that this gate can be enabled. No canonical conversation
schema or existing user preference keys are migrated by this module extraction.

## Verification

From the project checkout with JDK21 and the configured Android SDK:

```sh
make android-voice-test
```

This runs SDK-layer unit tests and lint without compiling/loading Assistant JNI.
Phone/Wear aggregate checks also include this module. Tests use synthetic inputs and
simulated SDK boundaries; they do not establish device/provider, acoustic or optimized
physical performance acceptance. Host dependency isolation is not itself a measured
launch or microphone speedup.
