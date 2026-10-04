package dev.gitblame.voxgolem.android.wear.capture

import android.content.Context
import dev.gitblame.voxgolem.capture.ArchiveState
import dev.gitblame.voxgolem.capture.CaptureFormat
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.CaptureRepository as SharedCaptureRepository
import dev.gitblame.voxgolem.capture.PcmRecordingWriter
import dev.gitblame.voxgolem.capture.RecognitionState
import dev.gitblame.voxgolem.capture.TransferState
import dev.gitblame.voxgolem.capture.TranscriptReducer
import dev.gitblame.voxgolem.capture.TranscriptUpdate
import java.io.File
import java.io.FileOutputStream
import java.time.Instant
import java.time.ZoneId
import java.security.MessageDigest
import java.util.concurrent.ConcurrentHashMap

/** Watch adapter over the shared capture schema, SQLite repository, and PCM/WAV implementation. */
class CaptureRepository(context: Context) {
    private val appContext = context.applicationContext
    private val shared = SharedCaptureRepository(appContext)
    private val transcriptReducers = ConcurrentHashMap<String, TranscriptReducer>()

    fun create(sampleRate: Int, sessionId: String): CaptureRecord {
        val id = CaptureIdentity.newId()
        val pcm = shared.createPcmSpool(id)
        val now = Instant.now().toString()
        return CaptureRecord(
            captureId = id,
            sourceKind = "watch",
            createdAtUtc = now,
            startedAtUtc = now,
            captureState = CaptureState.PREPARING,
            format = CaptureFormat(sampleRate = sampleRate, channels = 1, bitsPerSample = 16),
            localAudioPath = pcm.absolutePath,
            recognitionState = RecognitionState.QUEUED,
            recognitionSessionId = sessionId,
            transferState = TransferState.PENDING,
        ).also(shared::save)
    }

    fun get(id: String): CaptureRecord? = shared.get(id)

    fun records(): List<CaptureRecord> = shared.list(limit = 500)

    fun pendingTransfers(): List<CaptureRecord> = recordPages().flatten().filter { record ->
        record.captureState in setOf(CaptureState.SAVED, CaptureState.INTERRUPTED) &&
            record.transferState != TransferState.PHONE_ARCHIVED && record.localAudioPath?.endsWith(".wav") == true
    }.toList()

    private fun recordPages(): Sequence<List<CaptureRecord>> = generateSequence(0) { it + 500 }
        .map { shared.list(it, 500) }.takeWhile { it.isNotEmpty() }

    fun active(): CaptureRecord? = records().firstOrNull {
        it.captureState == CaptureState.PREPARING || it.captureState == CaptureState.RECORDING || it.captureState == CaptureState.STOPPING
    }

    /** Lifecycle updates merge transactionally and cannot erase newer transcript or receipt fields. */
    fun save(record: CaptureRecord): CaptureRecord = shared.update(record.captureId) { current ->
        record.copy(
            captureId = current.captureId,
            transferState = current.transferState.maxWith(record.transferState),
            recognitionState = if (current.finalText != null) current.recognitionState else record.recognitionState,
            recognitionSessionId = current.recognitionSessionId ?: record.recognitionSessionId,
            lastRevision = maxOf(current.lastRevision, record.lastRevision),
            previewText = if (current.lastRevision > record.lastRevision) current.previewText else record.previewText,
            finalText = current.finalText ?: record.finalText,
            archivedAudioUri = current.archivedAudioUri ?: record.archivedAudioUri,
        )
    } ?: error("Capture disappeared before update: ${record.captureId}")

    fun pcm(id: String): File = shared.createPcmSpool(id)

    fun audio(id: String): File {
        val directory = File(File(appContext.filesDir, "captures"), id)
        return File(directory, "audio.wav")
    }

    fun updateTransfer(id: String, status: TransferState) {
        updateTransfer(id, status, Instant.now())
    }

    fun updateTransfer(id: String, status: TransferState, at: Instant) {
        shared.update(id) { current ->
            val merged = mergeTransferState(current.transferState, status)
            val stagedRetry = when {
                merged != TransferState.PHONE_STAGED -> current.retryCount
                current.transferState == TransferState.PHONE_STAGED -> (current.retryCount + 1).coerceAtMost(MAX_RETRY_COUNT)
                else -> 0
            }
            val retryAt = when (merged) {
                TransferState.PHONE_STAGED -> at.plusSeconds(transferRetryDelaySeconds(stagedRetry)).toString()
                TransferState.PHONE_ARCHIVED -> null
                TransferState.SENDING, TransferState.PENDING -> current.nextRetryAtUtc
                    ?.let { runCatching { Instant.parse(it) }.getOrNull() }
                    ?.takeIf { it.isAfter(at) }
                    ?.toString()
                    ?: at.plusSeconds(transferRetryDelaySeconds(current.retryCount)).toString()
                TransferState.LOCAL_ONLY -> current.nextRetryAtUtc
            }
            current.copy(
                transferState = merged,
                retryCount = if (merged == TransferState.PHONE_STAGED) stagedRetry else current.retryCount,
                nextRetryAtUtc = retryAt,
                archiveState = when (merged) {
                    TransferState.PHONE_STAGED -> ArchiveState.STAGED
                    TransferState.PHONE_ARCHIVED -> ArchiveState.VERIFIED
                    else -> current.archiveState
                },
            )
        }
    }

    fun beginTransferAttempt(id: String, at: Instant = Instant.now()): CaptureRecord? {
        var attempted: CaptureRecord? = null
        shared.update(id) { current ->
            if (current.transferState == TransferState.PHONE_STAGED || current.transferState == TransferState.PHONE_ARCHIVED) current
            else current.copy(
                transferState = TransferState.SENDING,
                retryCount = (current.retryCount + 1).coerceAtMost(MAX_RETRY_COUNT),
                nextRetryAtUtc = at.plusSeconds(transferRetryDelaySeconds(current.retryCount)).toString(),
            ).also { attempted = it }
        }
        return attempted
    }

    fun updateTranscript(id: String, sessionId: String, revision: Long, text: String, isFinal: Boolean) {
        if (!CaptureProtocol.validatePayloadSize(text.toByteArray(Charsets.UTF_8).size, channelResult = true)) return
        val before = shared.get(id) ?: return
        if (before.recognitionSessionId != sessionId || before.finalText != null || revision <= before.lastRevision) return
        val snapshot = transcriptReducers.getOrPut(id) { TranscriptReducer(id) }
            .apply(TranscriptUpdate(id, sessionId, revision, text, isFinal))
        if (snapshot.revision != revision) return
        shared.update(id) { current ->
            if (current.recognitionSessionId != sessionId || current.finalText != null || revision <= current.lastRevision) current
            else current.copy(
                recognitionState = if (isFinal) {
                    if (text.isBlank()) RecognitionState.NO_SPEECH else RecognitionState.FINAL
                } else RecognitionState.RUNNING,
                lastRevision = revision,
                previewText = snapshot.preview,
                finalText = snapshot.finalText,
            )
        }
    }

    fun recoverInterrupted() {
        val activeRecovered = shared.recoverInterrupted().map { it.captureId }.toSet()
        val interrupted = recordPages().flatten().filter { record ->
            record.captureState == CaptureState.INTERRUPTED &&
                (record.captureId in activeRecovered || record.localAudioPath?.endsWith(".pcm.part") == true ||
                    (record.finalizedByteLength == null && existingPcm(record.captureId).isFile))
        }
        interrupted.forEach { finalizeInterruptedSpool(it) }
    }

    /** Explicit repair retries only an already-interrupted capture; it never starts a microphone session. */
    fun retryInterruptedFinalization(captureId: String): Boolean {
        val record = shared.get(captureId) ?: return false
        if (record.captureState != CaptureState.INTERRUPTED) return false
        recoveryAttemptsFile(captureId).delete()
        val wav = audio(captureId)
        if (wav.isFile && manifestFromWav(record, wav) == null) {
            val quarantine = File(wav.parentFile, "audio-invalid-${java.util.UUID.randomUUID()}.wav")
            if (!wav.renameTo(quarantine)) {
                setRecoveryError(captureId, "cannot-preserve-invalid-wav")
                return false
            }
        }
        return finalizeInterruptedSpool(record)
    }

    fun deletePcm(id: String) { pcm(id).delete() }

    private fun finalizeInterruptedSpool(record: CaptureRecord): Boolean {
        if (record.captureState != CaptureState.INTERRUPTED) return false
        val spool = existingPcm(record.captureId)
        val wav = audio(record.captureId)
        if (!spool.isFile && !wav.isFile) return false

        val attempts = recoveryAttemptsFile(record.captureId)
        val previousAttempts = attempts.readTextOrZero()
        if (previousAttempts >= MAX_RECOVERY_FINALIZATION_ATTEMPTS) {
            setRecoveryError(record.captureId, "wav-finalization-retry-limit")
            return false
        }
        attempts.writeCount(previousAttempts + 1)

        val manifest = try {
            if (wav.exists()) {
                manifestFromWav(record, wav) ?: throw IllegalStateException("existing-wav-conflict")
            } else {
                PcmRecordingWriter(spool, record.format).use { it.finish(wav) }
            }
        } catch (failure: Exception) {
            setRecoveryError(record.captureId, failure.message ?: "wav-finalization-failed")
            return false
        }

        val committed = runCatching { shared.update(record.captureId) { current ->
            current.copy(
                captureState = CaptureState.INTERRUPTED,
                localAudioPath = wav.absolutePath,
                observedFrames = maxOf(current.observedFrames, manifest.frameCount),
                durableFrames = manifest.frameCount,
                finalizedByteLength = manifest.wavByteLength,
                sha256 = manifest.sha256,
                stopReason = current.stopReason ?: "process-recovery",
                finishedAtUtc = current.finishedAtUtc ?: Instant.now().toString(),
                finishedLocalDate = current.finishedLocalDate ?: Instant.now().atZone(ZoneId.systemDefault()).toLocalDate().toString(),
                timezoneId = current.timezoneId ?: ZoneId.systemDefault().id,
                lastErrorStage = if (current.lastErrorStage == "capture-recovery") null else current.lastErrorStage,
                lastErrorCode = if (current.lastErrorStage == "capture-recovery") null else current.lastErrorCode,
            )
        } }.getOrNull()
        if (committed == null) {
            setRecoveryError(record.captureId, "recovery-metadata-commit-failed")
            return false
        }
        spool.delete()
        attempts.delete()
        return true
    }

    private fun setRecoveryError(id: String, code: String) {
        shared.update(id) { current ->
            if (current.captureState != CaptureState.INTERRUPTED) current
            else current.copy(captureState = CaptureState.INTERRUPTED, lastErrorStage = "capture-recovery", lastErrorCode = code)
        }
    }

    private fun existingPcm(id: String) = File(File(File(appContext.filesDir, "captures"), id), "audio.pcm.part")

    private fun recoveryAttemptsFile(id: String) = File(File(File(appContext.filesDir, "captures"), id), "wav-recovery-attempts")

    private fun manifestFromWav(record: CaptureRecord, wav: File): dev.gitblame.voxgolem.capture.CaptureManifest? {
        val frameBytes = record.format.channels * record.format.bitsPerSample / 8
        if (wav.length() < 44 || (wav.length() - 44) % frameBytes != 0L) return null
        val dataBytes = wav.length() - 44
        val header = ByteArray(44)
        wav.inputStream().use { input ->
            var read = 0
            while (read < header.size) {
                val count = input.read(header, read, header.size - read)
                if (count < 0) return null
                read += count
            }
        }
        if (String(header, 0, 4, Charsets.US_ASCII) != "RIFF" ||
            String(header, 8, 4, Charsets.US_ASCII) != "WAVE" ||
            String(header, 12, 4, Charsets.US_ASCII) != "fmt " ||
            String(header, 36, 4, Charsets.US_ASCII) != "data" ||
            le32(header, 4) != wav.length() - 8 || le16(header, 20) != 1 ||
            le16(header, 22) != record.format.channels || le32(header, 24) != record.format.sampleRate.toLong() ||
            le16(header, 34) != record.format.bitsPerSample || le32(header, 40) != dataBytes) return null
        return dev.gitblame.voxgolem.capture.CaptureManifest(
            captureId = record.captureId,
            format = record.format,
            frameCount = (wav.length() - 44) / frameBytes,
            pcmByteLength = wav.length() - 44,
            wavByteLength = wav.length(),
            sha256 = sha256(wav),
        ).takeIf(CaptureProtocol::validateManifest)
    }

    private fun le16(bytes: ByteArray, offset: Int): Int =
        (bytes[offset].toInt() and 0xff) or ((bytes[offset + 1].toInt() and 0xff) shl 8)

    private fun le32(bytes: ByteArray, offset: Int): Long =
        (bytes[offset].toLong() and 0xff) or ((bytes[offset + 1].toLong() and 0xff) shl 8) or
            ((bytes[offset + 2].toLong() and 0xff) shl 16) or ((bytes[offset + 3].toLong() and 0xff) shl 24)

    private fun File.readTextOrZero(): Int =
        if (!exists()) 0 else runCatching { readText().trim().toInt().coerceIn(0, MAX_RECOVERY_FINALIZATION_ATTEMPTS) }.getOrDefault(0)

    private fun File.writeCount(count: Int) {
        val temp = File(parentFile, "$name.tmp")
        FileOutputStream(temp).use { output ->
            output.write(count.toString().toByteArray(Charsets.UTF_8))
            output.fd.sync()
        }
        check(temp.renameTo(this)) { "Could not persist WAV recovery attempt" }
    }

    private fun sha256(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().buffered().use { input ->
            val buffer = ByteArray(16 * 1024)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                digest.update(buffer, 0, count)
            }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun TransferState.maxWith(other: TransferState): TransferState =
        if (ordinal >= other.ordinal) this else other

    private companion object {
        const val MAX_RETRY_COUNT = 10
        const val MAX_RECOVERY_FINALIZATION_ATTEMPTS = 3
    }
}

internal fun transferRetryDelaySeconds(retryCount: Int): Long =
    (30L * (1L shl retryCount.coerceIn(0, 10))).coerceAtMost(300L)

internal fun mergeTransferState(current: TransferState, update: TransferState): TransferState = when (current) {
    TransferState.PHONE_ARCHIVED -> current
    TransferState.PHONE_STAGED -> if (update == TransferState.PHONE_ARCHIVED) update else current
    else -> update
}
