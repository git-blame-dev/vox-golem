package dev.gitblame.voxgolem.capture

import android.content.Context
import java.io.File
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale
import java.util.TimeZone
import java.util.UUID

data class RecordedAudioArtifact(val captureId: String, val path: String, val manifest: CaptureManifest)

/** Local audio persistence only. Opening or finishing an entry never schedules export or transfer. */
class RecordedAudioStore(context: Context, private val sourceKind: String,
    private val clock: () -> Long = System::currentTimeMillis,
    private val timeZone: () -> TimeZone = TimeZone::getDefault,
) {
    internal val appContext = context.applicationContext
    private val repository = CaptureRepository(appContext)

    fun open(purpose: CapturePurpose, language: String, networkFallback: Boolean): Recording {
        require(purpose != CapturePurpose.NOTE) { "Notes retain their existing capture owner" }
        // Caller holds entry/file admission, so only abandoned voice spools can be recovered here.
        recoverInterrupted()
        val id = UUID.randomUUID().toString()
        val now = formatUtc(Date(clock()))
        val spool = repository.createPcmSpool(id)
        val record = CaptureRecord(captureId = id, sourceKind = sourceKind, createdAtUtc = now,
            startedAtUtc = now, purpose = purpose, requestedLanguage = language,
            speechNetworkFallback = networkFallback, format = CaptureFormat(),
            captureState = CaptureState.PREPARING, localAudioPath = spool.absolutePath,
            recognitionState = RecognitionState.RUNNING, transferState = TransferState.LOCAL_ONLY,
            orgState = OrgState.NONE)
        repository.save(record)
        try {
            return Recording(record, spool, PcmRecordingWriter(spool, record.format))
        } catch (failure: Exception) {
            repository.update(id) { it.copy(captureState = CaptureState.FAILED,
                lastErrorStage = "recording-open", lastErrorCode = "spool-create-failed") }
            throw failure
        }
    }

    /** Requires exclusive entry/file admission. Does not repair, finalize or delete any Note audio. */
    fun recoverInterrupted(): List<CaptureRecord> = CapturePurpose.entries.filter { it != CapturePurpose.NOTE }
        .flatMap { repository.recoverInterrupted(it) }

    inner class Recording internal constructor(
        private val initial: CaptureRecord,
        private val spool: File,
        private val writer: PcmRecordingWriter,
    ) {
        val captureId: String get() = initial.captureId
        private var completed: RecordedAudioArtifact? = null
        private var ended = false
        fun started() {
            checkNotNull(repository.update(captureId) { it.copy(captureState = CaptureState.RECORDING) })
        }
        fun append(pcm: ByteArray, count: Int = pcm.size) = writer.append(pcm, count)
        fun checkpoint() {
            val checkpoint = writer.checkpoint()
            checkNotNull(repository.update(captureId) { it.copy(observedFrames = checkpoint.frameCount,
                durableFrames = checkpoint.frameCount) })
        }
        @Synchronized fun finish(reason: String): RecordedAudioArtifact {
            completed?.let { return it }
            check(!ended)
            ended = true
            val finish = Date(clock())
            val zone = timeZone().clone() as TimeZone
            val finishedAtUtc = formatUtc(finish)
            val finishedLocalDate = SimpleDateFormat("yyyy-MM-dd", Locale.ROOT).apply { this.timeZone = zone }.format(finish)
            try {
                val wav = File(spool.parentFile, "audio.wav")
                val manifest = writer.finish(wav, captureId)
                checkNotNull(repository.update(captureId) { it.copy(captureState = CaptureState.SAVED,
                    finishedAtUtc = finishedAtUtc, finishedLocalDate = finishedLocalDate,
                    timezoneId = zone.id, stopReason = reason, observedFrames = manifest.frameCount,
                    durableFrames = manifest.frameCount, localAudioPath = wav.absolutePath,
                    finalizedByteLength = manifest.wavByteLength, sha256 = manifest.sha256,
                    transferState = TransferState.LOCAL_ONLY) })
                return RecordedAudioArtifact(captureId, wav.absolutePath, manifest).also { completed = it }
            } catch (failure: Exception) {
                runCatching { writer.close() }
                runCatching { repository.update(captureId) { it.copy(captureState = CaptureState.INTERRUPTED,
                    finishedAtUtc = finishedAtUtc, stopReason = reason,
                    lastErrorStage = "recording-save", lastErrorCode = "audio-save-failed") } }
                throw failure
            }
        }
        fun recognition(text: String?, error: String? = null) {
            checkNotNull(repository.update(captureId) { it.copy(
                recognitionState = if (error != null) RecognitionState.FAILED else if (text.isNullOrBlank())
                    RecognitionState.NO_SPEECH else RecognitionState.FINAL,
                finalText = text?.takeIf(String::isNotBlank), previewText = null,
                transcriptVersion = if (text.isNullOrBlank()) it.transcriptVersion else it.transcriptVersion + 1,
                lastErrorStage = if (error != null) "recognition" else it.lastErrorStage,
                lastErrorCode = error ?: it.lastErrorCode) })
        }
    }

    private fun formatUtc(date: Date): String = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'", Locale.ROOT)
        .apply { this.timeZone = TimeZone.getTimeZone("UTC") }.format(date)
}
