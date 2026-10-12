package dev.gitblame.voxgolem.capture

enum class CaptureState { PREPARING, RECORDING, STOPPING, SAVED, INTERRUPTED, FAILED }
enum class RecognitionState { UNAVAILABLE, QUEUED, RUNNING, FINAL, FAILED, NO_SPEECH }
enum class TransferState { LOCAL_ONLY, PENDING, SENDING, PHONE_STAGED, PHONE_ARCHIVED }
enum class ArchiveState { NOT_STAGED, STAGED, WAITING_FOR_FOLDER, COPYING, VERIFIED, RETRYABLE_ERROR, CONFLICT }
enum class OrgState { WAITING_FOR_TRANSCRIPT, WAITING_FOR_FOLDER, READY, WRITING, EXPORTED, RETRYABLE_ERROR, NEEDS_REVIEW, NONE }
enum class CapturePurpose { NOTE, ASK, TALK, DRAFT }
enum class LiveTranscriptionState {
    NOT_REQUESTED, CONNECTING, PENDING, ATTEMPTING, PROVIDER_READY, TRANSCRIBING, UNAVAILABLE, COMPLETED;

    companion object {
        fun fromWire(value: String): LiveTranscriptionState? = when (value) {
            "pending" -> PENDING
            "attempting" -> ATTEMPTING
            "provider-ready" -> PROVIDER_READY
            "accepted" -> TRANSCRIBING
            "unavailable", "live-input-ownership-rejected", "transcript-too-large" -> UNAVAILABLE
            else -> null
        }
    }
}

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
    val completionNotified: Boolean = false,
    val attentionNotifiedCode: String? = null,
    val purpose: CapturePurpose = CapturePurpose.NOTE,
    val requestedLanguage: String? = null,
    val speechNetworkFallback: Boolean = false,
    val liveTranscriptionState: LiveTranscriptionState = LiveTranscriptionState.NOT_REQUESTED,
    val liveStatusRevision: Long = 0,
    val canTranscribeLive: Boolean = false,
    val liveErrorCode: String? = null,
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
    const val VERSION = 1
    const val PROTOCOL = "voxgolem/capture/v1"
    const val BASE = "/$PROTOCOL"
    const val LIVE = "$BASE/live"
    const val ARCHIVE = "$BASE/archive"
    const val PENDING = "$BASE/pending"
    const val PHONE_CAPABILITY = "voxgolem_capture_phone_v1"
    const val MAX_HEADER_BYTES = 4 * 1024
    const val MAX_MESSAGE_BYTES = 32 * 1024
    const val MAX_CHANNEL_RESULT_BYTES = 256 * 1024
    const val MAX_FRAME_MILLIS = 100
    const val START = "$BASE/start"
    const val READY = "$BASE/ready"
    const val PARTIAL = "$BASE/partial"
    const val FINAL = "$BASE/final"
    const val STOP = "$BASE/stop"
    const val RECEIPT = "$BASE/receipt"
    const val STATUS = "$BASE/status"

    private val uuidPattern = Regex("(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")

    fun isValidUuid(value: String): Boolean = uuidPattern.matches(value)

    fun validateFormat(format: CaptureFormat): Boolean =
        format.sampleRate in 8_000..48_000 && format.channels in 1..2 && format.bitsPerSample == 16

    fun validateRequestedLanguage(value: String?): Boolean = value == null ||
        (value.length in 1..64 && value.matches(Regex("[A-Za-z]{2,8}(-[A-Za-z0-9]{1,8})*")))

    fun validateFrame(format: CaptureFormat, sequence: Long, byteLength: Int, previousSequence: Long?): Boolean {
        if (!validateFormat(format) || sequence < 0 || byteLength <= 0) return false
        val alignment = format.channels * format.bitsPerSample / 8
        val maximumBytes = format.sampleRate.toLong() * alignment * MAX_FRAME_MILLIS / 1_000
        return byteLength % alignment == 0 && byteLength <= maximumBytes &&
            (if (previousSequence == null) sequence == 0L else previousSequence >= 0 && sequence == previousSequence + 1)
    }

    fun validatePayloadSize(byteLength: Int, channelResult: Boolean = false): Boolean =
        byteLength >= 0 && byteLength <= (if (channelResult) MAX_CHANNEL_RESULT_BYTES else MAX_MESSAGE_BYTES)

    fun validateManifest(manifest: CaptureManifest): Boolean {
        val alignment = manifest.format.channels * manifest.format.bitsPerSample / 8
        return isValidUuid(manifest.captureId) && validateFormat(manifest.format) && manifest.frameCount >= 0 &&
            manifest.frameCount <= Long.MAX_VALUE / alignment && manifest.pcmByteLength <= Long.MAX_VALUE - 44 &&
            manifest.pcmByteLength == manifest.frameCount * alignment &&
            manifest.wavByteLength == manifest.pcmByteLength + 44 && manifest.sha256.matches(Regex("(?i)^[0-9a-f]{64}$"))
    }
}

data class TranscriptUpdate(
    val captureId: String,
    val sessionId: String,
    val revision: Long,
    val fullText: String,
    val isFinal: Boolean,
)

data class TranscriptSnapshot(
    val sessionId: String?,
    val revision: Long,
    val preview: String?,
    val finalText: String?,
)

/** Reduces full-hypothesis events for one active recognition attempt. */
class TranscriptReducer(private val captureId: String) {
    private var state = TranscriptSnapshot(null, 0, null, null)

    /** Selects the current recognition attempt; late events from prior attempts are then rejected. */
    @Synchronized
    fun beginSession(sessionId: String): TranscriptSnapshot {
        require(sessionId.isNotBlank()) { "Recognition session cannot be blank" }
        if (state.sessionId != sessionId) state = TranscriptSnapshot(sessionId, 0, null, null)
        return state
    }

    @Synchronized
    fun apply(update: TranscriptUpdate): TranscriptSnapshot {
        if (update.captureId != captureId || update.sessionId.isBlank() || update.revision <= 0) return state
        if (state.sessionId != null && update.sessionId != state.sessionId) return state
        if (state.finalText != null || update.revision <= state.revision) return state
        val text = update.fullText
        state = TranscriptSnapshot(
            sessionId = update.sessionId,
            revision = update.revision,
            preview = text.takeIf { it.isNotBlank() } ?: state.preview,
            finalText = if (update.isFinal) text else null,
        )
        return state
    }

    @Synchronized
    fun snapshot(): TranscriptSnapshot = state
}
