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
import dev.gitblame.voxgolem.capture.CapturePurpose
import dev.gitblame.voxgolem.capture.LiveTranscriptionState
import dev.gitblame.voxgolem.voice.InstallationPreferences
import dev.gitblame.voxgolem.assistant.recognition.SpeechLocales
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
        val policy = InstallationPreferences(appContext).read()
        val requestedLanguage = SpeechLocales.parse(policy.speechLanguage.ifBlank { java.util.Locale.getDefault().toLanguageTag() })
            ?.toLanguageTag() ?: throw IllegalArgumentException("capture-language-unavailable")
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
            requestedLanguage = requestedLanguage,
            speechNetworkFallback = policy.speechNetworkFallback,
            liveTranscriptionState = LiveTranscriptionState.CONNECTING,
        ).also(shared::save)
    }

    fun get(id: String): CaptureRecord? = shared.get(id)

    fun records(limit: Int = 500): List<CaptureRecord> {
        require(limit in 1..500)
        return shared.recent(CapturePurpose.NOTE, limit)
    }

    fun pendingTransfers(): List<CaptureRecord> = recordPages().flatten().filter { record ->
        record.captureState in setOf(CaptureState.SAVED, CaptureState.INTERRUPTED) &&
            record.purpose == CapturePurpose.NOTE &&
            (record.transferState != TransferState.PHONE_ARCHIVED ||
                (record.captureState == CaptureState.SAVED && !record.completionNotified && record.recognitionState != RecognitionState.NO_SPEECH)) &&
            record.localAudioPath?.endsWith(".wav") == true
    }.toList()

    private fun recordPages(): Sequence<List<CaptureRecord>> = generateSequence(0) { it + 500 }
        .map { shared.list(it, 500) }.takeWhile { it.isNotEmpty() }

    fun active(): CaptureRecord? = shared.active(CapturePurpose.NOTE)

    /** Lifecycle updates merge transactionally and cannot erase newer transcript or receipt fields. */
    fun save(record: CaptureRecord): CaptureRecord = shared.update(record.captureId) { current ->
        record.copy(
            captureId = current.captureId,
            transferState = current.transferState.maxWith(record.transferState),
            recognitionState = if (current.finalText != null) current.recognitionState else record.recognitionState,
            recognitionSessionId = current.recognitionSessionId ?: record.recognitionSessionId,
            lastRevision = maxOf(current.lastRevision, record.lastRevision),
            previewText = if (current.lastRevision > record.lastRevision || record.previewText.isNullOrBlank()) current.previewText else record.previewText,
            finalText = current.finalText ?: record.finalText,
            transcriptVersion = maxOf(current.transcriptVersion, record.transcriptVersion),
            exportTranscriptVersion = listOfNotNull(current.exportTranscriptVersion, record.exportTranscriptVersion).maxOrNull(),
            archivedAudioUri = current.archivedAudioUri ?: record.archivedAudioUri,
            completionNotified = current.completionNotified,
            attentionNotifiedCode = current.attentionNotifiedCode,
            purpose = current.purpose, requestedLanguage = current.requestedLanguage,
            speechNetworkFallback = current.speechNetworkFallback,
            liveTranscriptionState = current.liveTranscriptionState,
            liveStatusRevision = current.liveStatusRevision, canTranscribeLive = current.canTranscribeLive,
            liveErrorCode = current.liveErrorCode,
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
            if (current.purpose != CapturePurpose.NOTE) return@update current
            val merged = mergeTransferState(current.transferState, status)
            val stagedRetry = when {
                merged < TransferState.PHONE_STAGED -> current.retryCount
                current.transferState >= TransferState.PHONE_STAGED -> (current.retryCount + 1).coerceAtMost(MAX_RETRY_COUNT)
                else -> 0
            }
            val retryAt = when (merged) {
                TransferState.PHONE_STAGED, TransferState.PHONE_ARCHIVED -> at.plusSeconds(transferRetryDelaySeconds(stagedRetry)).toString()
                TransferState.SENDING, TransferState.PENDING -> current.nextRetryAtUtc
                    ?.let { runCatching { Instant.parse(it) }.getOrNull() }
                    ?.takeIf { it.isAfter(at) }
                    ?.toString()
                    ?: at.plusSeconds(transferRetryDelaySeconds(current.retryCount)).toString()
                TransferState.LOCAL_ONLY -> current.nextRetryAtUtc
            }
            current.copy(
                transferState = merged,
                retryCount = if (merged >= TransferState.PHONE_STAGED) stagedRetry else current.retryCount,
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
            if (current.purpose != CapturePurpose.NOTE || current.transferState == TransferState.PHONE_STAGED || current.transferState == TransferState.PHONE_ARCHIVED) current
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
        if (before.purpose != CapturePurpose.NOTE || before.recognitionSessionId != sessionId || before.finalText != null || revision <= before.lastRevision) return
        val snapshot = transcriptReducers.getOrPut(id) { TranscriptReducer(id) }
            .apply(TranscriptUpdate(id, sessionId, revision, text, isFinal && text.isNotBlank()))
        if (snapshot.revision != revision) return
        shared.update(id) { current ->
            if (current.recognitionSessionId != sessionId || current.finalText != null || revision <= current.lastRevision) current
            else current.copy(
                recognitionState = if (isFinal) {
                    if (text.isBlank()) RecognitionState.NO_SPEECH else RecognitionState.FINAL
                } else RecognitionState.RUNNING,
                lastRevision = revision,
                previewText = snapshot.preview?.takeIf { it.isNotBlank() } ?: current.previewText,
                finalText = snapshot.finalText,
                liveTranscriptionState = if (text.isNotBlank() && current.liveTranscriptionState != LiveTranscriptionState.UNAVAILABLE) LiveTranscriptionState.TRANSCRIBING else current.liveTranscriptionState,
                canTranscribeLive = current.liveTranscriptionState != LiveTranscriptionState.UNAVAILABLE && (current.canTranscribeLive || text.isNotBlank()),
                liveErrorCode = if (text.isNotBlank() && current.liveTranscriptionState != LiveTranscriptionState.UNAVAILABLE) null else current.liveErrorCode,
            )
        }
    }

    fun updateLiveStatus(id: String, sessionId: String, revision: Long, state: LiveTranscriptionState,
                         canTranscribe: Boolean, errorCode: String? = null): Boolean {
        if (revision < 0) return false
        var accepted = false
        shared.update(id) { current ->
            if (current.purpose != CapturePurpose.NOTE || current.recognitionSessionId != sessionId || current.finalText != null ||
                 (current.liveTranscriptionState == LiveTranscriptionState.UNAVAILABLE && state !in setOf(LiveTranscriptionState.PENDING, LiveTranscriptionState.UNAVAILABLE)) ||
                 (current.liveTranscriptionState == LiveTranscriptionState.TRANSCRIBING && errorCode == null &&
                     state in setOf(LiveTranscriptionState.PENDING, LiveTranscriptionState.ATTEMPTING, LiveTranscriptionState.PROVIDER_READY)) ||
                (revision > 0 && revision <= current.liveStatusRevision) || (revision == 0L && current.liveStatusRevision > 0)) current
            else current.copy(liveTranscriptionState = if (current.liveTranscriptionState == LiveTranscriptionState.UNAVAILABLE) LiveTranscriptionState.UNAVAILABLE else state,
                liveStatusRevision = revision,
                canTranscribeLive = current.liveTranscriptionState != LiveTranscriptionState.UNAVAILABLE && canTranscribe,
                liveErrorCode = errorCode?.let { dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(it) }
                    ?: current.liveErrorCode.takeIf { current.liveTranscriptionState == LiveTranscriptionState.UNAVAILABLE }).also { accepted = true }
        }
        return accepted
    }

    fun reportLiveTransportFailure(id: String, sessionId: String?, code: String) {
        shared.update(id) { current ->
            if (current.purpose != CapturePurpose.NOTE || current.recognitionSessionId != sessionId || current.finalText != null) current
            else current.copy(liveTranscriptionState = LiveTranscriptionState.UNAVAILABLE, canTranscribeLive = false,
                liveErrorCode = current.liveErrorCode?.takeUnless { it in transportFailures }
                    ?: dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(code))
        }
    }

    fun acceptPhoneCompletion(id: String, hash: String, text: String, version: Long): Boolean {
        if (text.isBlank() || version < 0 || !CaptureProtocol.validatePayloadSize(text.toByteArray(Charsets.UTF_8).size, channelResult = true)) return false
        var accepted = false
        shared.update(id) { current ->
            if (current.purpose != CapturePurpose.NOTE || current.captureState != CaptureState.SAVED || !hash.equals(current.sha256, ignoreCase = true) ||
                current.transferState < TransferState.PHONE_STAGED || version < current.transcriptVersion || current.completionNotified) current
            else current.copy(
                recognitionState = RecognitionState.FINAL, finalText = text, previewText = text,
                transcriptVersion = version, lastErrorStage = null, lastErrorCode = null,
                liveTranscriptionState = LiveTranscriptionState.COMPLETED, liveErrorCode = null,
                canTranscribeLive = false,
            ).also { accepted = true }
        }
        return accepted
    }

    fun claimCompletionFeedback(id: String): Boolean {
        var claimed = false
        shared.update(id) { current ->
            val wav = audio(id)
            if (current.purpose != CapturePurpose.NOTE || current.completionNotified || current.captureState != CaptureState.SAVED ||
                current.recognitionState != RecognitionState.FINAL || current.finalText.isNullOrBlank() ||
                current.transferState < TransferState.PHONE_STAGED || current.finalizedByteLength == null ||
                !wav.isFile || wav.length() != current.finalizedByteLength) current
            else current.copy(completionNotified = true).also { claimed = true }
        }
        return claimed
    }

    fun reportCompletionProblem(id: String, code: String, state: RecognitionState? = null): Boolean {
        var notify = false
        val latestCaptureId = records(limit = 1).firstOrNull()?.captureId
        shared.update(id) { current ->
            if (current.purpose != CapturePurpose.NOTE || current.completionNotified) current
            else current.copy(
                recognitionState = state ?: current.recognitionState,
                liveTranscriptionState = LiveTranscriptionState.UNAVAILABLE,
                canTranscribeLive = false, liveErrorCode = dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(code),
                lastErrorStage = "completion", lastErrorCode = dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(code),
                attentionNotifiedCode = if (current.captureId == latestCaptureId) dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(code) else current.attentionNotifiedCode,
            ).also { notify = current.captureId == latestCaptureId && current.attentionNotifiedCode != it.attentionNotifiedCode }
        }
        return notify
    }

    fun recoverInterrupted() {
        recoverForAdmission()
        repairInterruptedAudio()
    }

    /** Retire abandoned active metadata and align its owned spool before admitting a new recorder. */
    fun recoverForAdmission() {
        val owner = checkNotNull(CaptureLifetime.tryAcquire()) { "Capture worker has not retired" }
        try { recoverForAdmission(owner) } finally { owner.close() }
    }

    internal var beforeAdmissionRecovery: () -> Unit = {}

    internal fun recoverForAdmission(owner: CaptureLifetime.Owner) {
        check(owner.permitsRecovery()) { "Recovery requires exclusive abandoned-capture ownership" }
        beforeAdmissionRecovery()
        check(owner.permitsRecovery()) { "Capture recovery owner changed" }
        shared.recoverInterrupted(CapturePurpose.NOTE)
    }

    /** Historical WAV/hash work is not required for microphone admission. */
    fun repairInterruptedAudio(checkActive: () -> Unit = {}) {
        recordPages().flatten().forEach { record ->
            checkActive()
            if (needsAudioRepair(record)) synchronized(audioRepairMonitor) {
                checkActive()
                if (!CaptureLifetime.ownsCapture(record.captureId)) {
                    shared.get(record.captureId)?.takeIf(::needsAudioRepair)?.let(::finalizeInterruptedSpool)
                }
            }
        }
    }

    private fun needsAudioRepair(record: CaptureRecord): Boolean =
        record.purpose == CapturePurpose.NOTE && record.captureState == CaptureState.INTERRUPTED &&
            (record.localAudioPath?.endsWith(".pcm.part") == true ||
                (record.finalizedByteLength == null && (existingPcm(record.captureId).isFile || audio(record.captureId).isFile)))

    /** Explicit repair retries only an already-interrupted capture; it never starts a microphone session. */
    fun retryInterruptedFinalization(captureId: String): Boolean = synchronized(audioRepairMonitor) {
        if (CaptureLifetime.ownsCapture(captureId)) return@synchronized false
        val record = shared.get(captureId) ?: return@synchronized false
        if (record.purpose != CapturePurpose.NOTE || record.captureState != CaptureState.INTERRUPTED) return@synchronized false
        recoveryAttemptsFile(captureId).delete()
        val wav = audio(captureId)
        if (wav.isFile && manifestFromWav(record, wav) == null) {
            val quarantine = File(wav.parentFile, "audio-invalid-${java.util.UUID.randomUUID()}.wav")
            if (!wav.renameTo(quarantine)) {
                setRecoveryError(captureId, "cannot-preserve-invalid-wav")
                return@synchronized false
            }
        }
        finalizeInterruptedSpool(record)
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
        } catch (_: Exception) {
            setRecoveryError(record.captureId, "wav-finalization-failed")
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
        val transportFailures = setOf("phone-unavailable", "multiple-phone-receivers", "live-stream-failed", "live-queue-overflow", "live-finish-timeout")
        val audioRepairMonitor = Any()
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
