# Capture domain and storage API (v1)

Package: `dev.gitblame.voxgolem.capture`. Tauri includes the phone Android library as
`:tauri-plugin-capture`; Wear packages the same maintained domain/storage sources
without the phone-only Tauri classes and services.
These signatures are the integration contract for the phone and Wear owners; keep
the contracts free of Activity/UI/ASR dependencies. All timestamps are UTC ISO-8601
strings unless named `finishedLocalDate` (ISO `yyyy-MM-dd`) or `timezoneId`.

## Phone transport integration

Phone-only native classes (no generated Rust/Tauri edits required):

```kotlin
class PhoneCaptureCoordinator(context: Context) {
    fun start(): EnableResult
    fun enableFromVisibleActivity(): EnableResult
    fun retryCapture(captureId: String)
    fun finalizeCapture(captureId: String)
}
class CaptureListenerService : WearableListenerService()
class PhoneCaptureRuntimeService : Service()
```

`CaptureListenerService` uses the current Wearable event-specific manifest filters
for `CHANNEL_EVENT`, `MESSAGE_RECEIVED`, and `DATA_CHANGED`, scoped to the capture
path prefix. This follows the [WearableListenerService manifest contract](https://developers.google.com/android/reference/com/google/android/gms/wearable/WearableListenerService);
an additional legacy `BIND_LISTENER` action is not required. `PhoneCaptureRuntimeService` handles supplied
watch audio as a connected-device foreground service; it never records from the
phone microphone. The host manifest needs `FOREGROUND_SERVICE`,
`FOREGROUND_SERVICE_CONNECTED_DEVICE`, and `BLUETOOTH_CONNECT` (runtime grant on
Android 12+), and the service declaration must use
`android:foregroundServiceType="connectedDevice"`. Android SpeechRecognizer
also requires the user-granted `RECORD_AUDIO` permission even though the adapter
only supplies the watch's PCM source. On Android 13+ supply a visible notification
channel. A background-start denial is not bypassed; onboarding must provide the
platform-supported companion association / foreground-start route or require the
user to open the app before live transcription.

`SuppliedAudioSpeechRecognizer` requires API 33 external PCM input and an
on-device SpeechRecognizer. It uses a pipe-backed
`RecognizerIntent.EXTRA_AUDIO_SOURCE`, PCM16 format extras, partial results, and
explicit source EOF. Android exposes no provider-independent capability bit for
accepting this external source; readiness distinguishes a pending attempt from
provider-ready and actual hypothesis delivery. No network/cloud
fallback or phone-microphone fallback is selected. The on-device provider's
actual external-audio support, language availability, and accuracy remain a
device acceptance gate.

Phone wire handling matches `WearCaptureTransport.kt`: `protocol` is
`voxgolem/capture/v1`; start/ready/partial/final/stop/receipt/status are JSON
messages under `/voxgolem/capture/v1/*`; live channels use an int32-length JSON
start header, then repeated int64 sequence + int32 PCM-byte-length + PCM frames,
and int64 `-1` EOF. Archive channels use int32-length JSON header, int64 WAV
length, then exact WAV bytes. Archive receipts echo `archiveHash` and use
`PHONE_STAGED` then, only after verified SAF read-back, `PHONE_ARCHIVED` with
optional `destinationId`. Phone transcript notifications currently fit the
Wear sender's 32 KiB message cap; a channel-backed large-transcript response is
not implemented by that Wear sender and must not be claimed as delivered.

```kotlin
enum class CaptureState { PREPARING, RECORDING, STOPPING, SAVED, INTERRUPTED, FAILED }
enum class RecognitionState { UNAVAILABLE, QUEUED, RUNNING, FINAL, FAILED, NO_SPEECH }
enum class TransferState { LOCAL_ONLY, PENDING, SENDING, PHONE_STAGED, PHONE_ARCHIVED }
enum class ArchiveState { NOT_STAGED, STAGED, WAITING_FOR_FOLDER, COPYING, VERIFIED, RETRYABLE_ERROR, CONFLICT }
enum class OrgState { WAITING_FOR_TRANSCRIPT, WAITING_FOR_FOLDER, READY, WRITING, EXPORTED, RETRYABLE_ERROR, NEEDS_REVIEW }

data class CaptureFormat(
    val sampleRate: Int = 16_000,
    val channels: Int = 1,
    val bitsPerSample: Int = 16,
)

data class CaptureRecord(
    val captureId: String,
    val schemaVersion: Int = 1,
    val sourceKind: String = "watch",
    val createdAtUtc: String,
    val startedAtUtc: String? = null,
    val finishedAtUtc: String? = null,
    val finishedLocalDate: String? = null,
    val timezoneId: String? = null,
    val captureState: CaptureState = CaptureState.PREPARING,
    val stopReason: String? = null,
    val observedFrames: Long = 0,
    val durableFrames: Long = 0,
    val format: CaptureFormat = CaptureFormat(),
    val localAudioPath: String? = null,
    val archivedAudioUri: String? = null,
    val finalizedByteLength: Long? = null,
    val sha256: String? = null,
    val recognitionState: RecognitionState = RecognitionState.QUEUED,
    val recognitionSessionId: String? = null,
    val lastRevision: Long = 0,
    val previewText: String? = null,
    val finalText: String? = null,
    val engineId: String? = null,
    val language: String? = null,
    val transcriptVersion: Long = 0,
    val exportTranscriptVersion: Long? = null,
    val transferState: TransferState = TransferState.LOCAL_ONLY,
    val archiveState: ArchiveState = ArchiveState.NOT_STAGED,
    val orgState: OrgState = OrgState.WAITING_FOR_TRANSCRIPT,
    val exportDestinationId: String? = null,
    val lastErrorStage: String? = null,
    val lastErrorCode: String? = null,
    val retryCount: Int = 0,
    val nextRetryAtUtc: String? = null,
)

data class CaptureManifest(
    val captureId: String,
    val format: CaptureFormat,
    val frameCount: Long,
    val pcmByteLength: Long,
    val wavByteLength: Long,
    val sha256: String,
)

object CaptureProtocol {
    const val VERSION: Int = 1
    const val MAX_MESSAGE_BYTES: Int = 32 * 1024
    const val MAX_CHANNEL_RESULT_BYTES: Int = 256 * 1024
    const val MAX_FRAME_MILLIS: Int = 100
    const val START: String = "/voxgolem/capture/v1/start"
    const val READY: String = "/voxgolem/capture/v1/ready"
    const val PARTIAL: String = "/voxgolem/capture/v1/partial"
    const val FINAL: String = "/voxgolem/capture/v1/final"
    const val STOP: String = "/voxgolem/capture/v1/stop"
    const val RECEIPT: String = "/voxgolem/capture/v1/receipt"
    const val STATUS: String = "/voxgolem/capture/v1/status"
    fun isValidUuid(value: String): Boolean
    fun validateFormat(format: CaptureFormat): Boolean
    fun validateFrame(format: CaptureFormat, sequence: Long, byteLength: Int, previousSequence: Long?): Boolean
    fun validatePayloadSize(byteLength: Int, channelResult: Boolean = false): Boolean
    fun validateManifest(manifest: CaptureManifest): Boolean
}

data class TranscriptUpdate(
    val captureId: String, val sessionId: String, val revision: Long,
    val fullText: String, val isFinal: Boolean,
)
data class TranscriptSnapshot(val sessionId: String?, val revision: Long, val preview: String?, val finalText: String?)
class TranscriptReducer(private val captureId: String) {
    fun apply(update: TranscriptUpdate): TranscriptSnapshot
    fun snapshot(): TranscriptSnapshot
}

class CaptureRepository(context: android.content.Context) {
    fun list(offset: Int = 0, limit: Int = 50): List<CaptureRecord>
    fun get(id: String): CaptureRecord?
    /** Insert-only; an identical duplicate is idempotent, a differing duplicate throws. Use update for changes. */
    fun save(record: CaptureRecord): CaptureRecord
    /** Atomic read-transform-write; returns null when id is absent. */
    fun update(id: String, transform: (CaptureRecord) -> CaptureRecord): CaptureRecord?
    /** Marks process-owned PREPARING/RECORDING/STOPPING records INTERRUPTED. */
    fun recoverInterrupted(): List<CaptureRecord>
    fun createPcmSpool(captureId: String): java.io.File
}

data class PcmCheckpoint(val pcmByteLength: Long, val frameCount: Long)
class PcmRecordingWriter(file: java.io.File, format: CaptureFormat) : java.io.Closeable {
    fun append(bytes: ByteArray, count: Int = bytes.size)
    fun checkpoint(): PcmCheckpoint
    /** Finalizes to WAV using streaming copy; file's parent directory must be captureId. */
    fun finish(wavFile: java.io.File): CaptureManifest
    override fun close()
}

object OrgExport {
    /** UTF-8 headline with headline-breaking whitespace collapsed and ends trimmed. */
    fun formatEntry(finalText: String, finishedLocalDate: String): ByteArray
    /** Prepends entry and preserves all existing bytes, including an initial UTF-8 BOM. */
    fun prepend(entry: ByteArray, existing: ByteArray): ByteArray
}

data class NotesDestination(val id: String, val treeUri: android.net.Uri)
interface NotesDestinationStore {
    fun selected(): NotesDestination?
    fun select(treeUri: android.net.Uri): NotesDestination
}
class SafNotesStore(context: android.content.Context) : NotesDestinationStore {
    override fun selected(): NotesDestination?
    override fun select(treeUri: android.net.Uri): NotesDestination
    fun archiveAudio(destination: NotesDestination, captureId: String, sourceWav: java.io.File,
                     expectedSha256: String, expectedByteLength: Long): android.net.Uri
}

data class OrgExportResult(val state: OrgState, val destinationId: String?, val reason: String? = null)
class OrgExportCoordinator(context: android.content.Context) {
    /** Export is permitted only for an intentional SAVED capture with nonempty FINAL text. */
    fun process(captureId: String): OrgExportResult
}
```

Protocol validation is strict (UUIDs, supported PCM dimensions, positive bounded
frame lengths and contiguous sequence starting at zero, payload caps, and manifest
frame/length/hash consistency). A rejected update is a no-op in the transcript
reducer. SAF operations use document URIs, never guessed filesystem paths. The Org
coordinator serializes and journals writes and reports ambiguous provider outcomes as
`NEEDS_REVIEW`; it does not claim atomic replacement or cross-process editor locking.
Capture metadata and Org export journals use platform SQLite; SAF destination selection
and the lightweight audio-copy recovery locator use app-private preferences.
