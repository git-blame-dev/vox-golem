# Shared Android/Wear capture domain

Ordinary Android library consumed directly by the phone's
[capture plugin](../capture-plugin/API.md) and the native
[Wear shell](../../apps/wear/README.md). It owns portable capture records, SQLite
persistence and PCM/WAV utilities. It contains no Tauri dependency, Assistant
provider, foreground service, GMS listener or SAF/Org export implementation.

## One source of truth

- [CaptureContract.kt](src/main/java/dev/gitblame/voxgolem/capture/CaptureContract.kt):
  records, enums, manifests, protocol and transcript reducer.
- [CaptureRepository.kt](src/main/java/dev/gitblame/voxgolem/capture/CaptureRepository.kt):
  canonical SQLite repository and persistence JSON.
- [PcmRecordingWriter.kt](src/main/java/dev/gitblame/voxgolem/capture/PcmRecordingWriter.kt)
  and [WavPcmReader.kt](src/main/java/dev/gitblame/voxgolem/capture/WavPcmReader.kt):
  local recording finalization and validated supplied-audio reads.
- [RecognitionInputCompletion.kt](src/main/java/dev/gitblame/voxgolem/capture/RecognitionInputCompletion.kt),
  and [SegmentedTranscriptAssembler.kt](src/main/java/dev/gitblame/voxgolem/capture/SegmentedTranscriptAssembler.kt):
  capture recognition completion and transcript assembly. The shared Android SDK
  segmented callback bridge belongs to [voice-android](../voice-android/README.md).
- [CaptureTiming.kt](src/main/java/dev/gitblame/voxgolem/capture/CaptureTiming.kt):
  bounded startup-stage timing, not a speech accuracy or performance guarantee.

Phone and watch depend on this module through ordinary Gradle project dependencies;
neither generates a copied capture-domain source tree. Shell-specific adapters own
Android lifecycle, permissions, microphone/GMS IO and display. The phone owns its
supplied-audio SDK adapter and verified SAF/Org export; the watch owns local capture,
gesture policy and outbox transport. SDK recognition/playback primitives are shared
through [voice-android](../voice-android/README.md); conversation-bound speech uses
[Assistant](../assistant-android/README.md). Capture storage depends on neither module
and does not create another conversation authority.

The [capture API guide](../capture-plugin/API.md) links the canonical source types,
public projection and [frontend parser](../../frontend/app/src/lib/captures.ts).
Public history projections are not private persistence JSON. Prefer those declarations
over copied field lists, database schemas or obsolete protocol examples in prose.

## Durability boundary

Local `SAVED`, matching phone-archive receipt and completed Org export are separate
facts. Finalized audio remains durable when preview, recognition or connectivity
fails. Live-preview dropping never implies dropping the local WAV. Receipts require
matching identity/hash after verified archival; channel completion alone is not success.
Interrupted recordings and ambiguous export writes retain recovery state and need
explicit action rather than being silently completed. Recording cleanup is not automatic.
Note capture never enters Assistant completion or conversation replication.

## Verification

Run from the source checkout on Linux with JDK21 and SDK36; the phone targets also
require pinned Tauri CLI2.11.1, NDK28.2.13676358 and the ARM64 Rust target:

```bash
make android-capture-test
make wear-test
```

These cover shared-domain/storage/WAV tests and native adapter/shell tests plus lint.
Physical speech-provider support, permissions/FGS behavior, worn-device gestures and
offline transfer/export recovery remain device acceptance gates. Use synthetic data;
never add real recordings, transcripts, credentials or runtime databases to Git.
