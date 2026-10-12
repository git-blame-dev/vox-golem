package dev.gitblame.voxgolem.capture

import android.content.Context
import org.json.JSONObject
import java.nio.charset.StandardCharsets
import java.util.concurrent.Executors

/** Publishes persisted capture results; live notifications never queue behind archive receipts. */
internal class PhoneCaptureDelivery(
    private val repository: CaptureRepository,
    private val sendMessage: (String, String, ByteArray) -> Unit,
    private val sendOversizedReceipt: (String, String, ByteArray) -> Unit,
) {
    private val delivery = java.util.concurrent.ThreadPoolExecutor(1, 1, 0, java.util.concurrent.TimeUnit.MILLISECONDS,
        java.util.concurrent.ArrayBlockingQueue<Runnable>(8))
    private val liveDelivery = Executors.newSingleThreadExecutor()

    class Transcript {
        @Volatile var deliverableRevision: Long = 0
    }

    /** A denial is deliberately independent of private capture metadata and contains no transcript/hash. */
    fun sendAuthorizationFailure(nodeId: String, captureId: String) {
        if (nodeId.isBlank() || !CaptureProtocol.isValidUuid(captureId)) return
        val bytes = JSONObject().put("protocol", CaptureWire.PROTOCOL).put("captureId", captureId)
            .put("stage", "AUTHORIZATION_FAILED").put("errorCode", "capture-origin-not-authorized")
            .toString().toByteArray(StandardCharsets.UTF_8)
        enqueueMessage(nodeId, CaptureWire.RECEIPT, bytes)
    }

    fun sendReady(nodeId: String, captureId: String, sessionId: String, state: String, canTranscribe: Boolean,
                  errorCode: String? = null) {
        if (nodeId.isBlank() || !CaptureProtocol.isValidUuid(captureId) || !CaptureProtocol.isValidUuid(sessionId)) return
        if (!repository.isPhoneOrigin(captureId, nodeId)) return
        val status = LiveTranscriptionState.fromWire(state) ?: return
        var revision = -1L
        val updated = repository.update(captureId) { current ->
            val terminalAttempt = current.recognitionState in setOf(
                RecognitionState.FAILED, RecognitionState.UNAVAILABLE, RecognitionState.FINAL, RecognitionState.NO_SPEECH)
            if (current.purpose != CapturePurpose.NOTE || current.recognitionSessionId != sessionId || current.finalText != null ||
                (terminalAttempt && status != LiveTranscriptionState.UNAVAILABLE) ||
                (current.liveTranscriptionState == LiveTranscriptionState.TRANSCRIBING && status in setOf(
                    LiveTranscriptionState.PENDING, LiveTranscriptionState.ATTEMPTING, LiveTranscriptionState.PROVIDER_READY))) current
            else current.copy(liveTranscriptionState = status, liveStatusRevision = current.liveStatusRevision + 1,
                canTranscribeLive = !terminalAttempt && canTranscribe,
                liveErrorCode = (errorCode ?: if (terminalAttempt) current.liveErrorCode ?: current.lastErrorCode else null)
                    ?.let { CaptureErrorCodes.safe(it) })
                .also { revision = it.liveStatusRevision }
        } ?: return
        if (revision < 0) return
        val bytes = JSONObject()
            .put("protocol", CaptureWire.PROTOCOL)
            .put("captureId", captureId)
            .put("sessionId", sessionId)
            .put("canTranscribeLive", updated.canTranscribeLive)
            .put("recognitionStatus", state)
            .put("statusRevision", revision)
            .apply { updated.liveErrorCode?.let { put("errorCode", it) } }
            .toString().toByteArray(StandardCharsets.UTF_8)
        enqueueMessage(nodeId, CaptureWire.READY, bytes) {
            repository.isPhoneOrigin(captureId, nodeId) && repository.get(captureId)?.let {
                it.purpose == CapturePurpose.NOTE && it.recognitionSessionId == sessionId && it.liveStatusRevision == revision
            } == true
        }
    }

    fun sendTranscript(captureId: String, sessionId: String, nodeId: String, transcript: Transcript,
                       timing: CaptureTiming, text: String, final: Boolean, revision: Long) {
        if (!repository.isPhoneOrigin(captureId, nodeId) || repository.get(captureId)?.purpose != CapturePurpose.NOTE) return
        val path = if (final) CaptureWire.FINAL else CaptureWire.PARTIAL
        val body = JSONObject()
            .put("protocol", CaptureWire.PROTOCOL)
            .put("captureId", captureId)
            .put("sessionId", sessionId)
            .put("revision", revision)
            .put("transcriptVersion", repository.get(captureId)?.transcriptVersion ?: 0)
            .put("fullText", text)
            .toString()
        val bytes = body.toByteArray(StandardCharsets.UTF_8)
        if (CaptureProtocol.validatePayloadSize(bytes.size)) {
            if (text.isNotBlank()) transcript.deliverableRevision = revision
            enqueueMessage(nodeId, path, bytes, timing.takeIf { text.isNotBlank() }) {
                repository.isPhoneOrigin(captureId, nodeId) && (final || revision >= transcript.deliverableRevision)
            }
        } else {
            repository.update(captureId) { current ->
                if (current.recognitionSessionId != sessionId) current
                else current.copy(lastErrorStage = "delivery", lastErrorCode = "transcript-not-sent-exceeds-message-limit")
            }
            sendReady(nodeId, captureId, sessionId, "transcript-too-large", true)
        }
    }

    fun sendReceipt(nodeId: String, record: CaptureRecord, stage: String, destinationId: String? = null) {
        if (record.purpose != CapturePurpose.NOTE || nodeId.isBlank() || record.sha256.isNullOrBlank() || !repository.isPhoneOrigin(record.captureId, nodeId)) return
        val bytes = JSONObject().put("protocol", CaptureWire.PROTOCOL).put("captureId", record.captureId)
            .put("archiveHash", record.sha256).put("stage", stage)
            .put("recognitionState", record.recognitionState.name)
            .put("transcriptVersion", record.transcriptVersion)
            .apply {
                if (record.captureState == CaptureState.SAVED && record.recognitionState == RecognitionState.FINAL && !record.finalText.isNullOrBlank()) {
                    put("finalText", record.finalText)
                }
                if (record.lastErrorStage in setOf("recognition", "execution", "delivery")) put("errorCode", record.lastErrorCode?.let { CaptureErrorCodes.safe(it) })
            }
            .apply { if (destinationId != null) put("destinationId", destinationId) }
            .toString().toByteArray(StandardCharsets.UTF_8)
        if (CaptureProtocol.validatePayloadSize(bytes.size)) enqueueMessage(nodeId, CaptureWire.RECEIPT, bytes) {
            repository.isPhoneOrigin(record.captureId, nodeId)
        }
        else if (CaptureProtocol.validatePayloadSize(bytes.size, channelResult = true)) {
            runCatching { delivery.execute {
                if (repository.isPhoneOrigin(record.captureId, nodeId)) runCatching { sendOversizedReceipt(nodeId, record.captureId, bytes) }
            } }
        } else {
            val error = JSONObject().put("protocol", CaptureWire.PROTOCOL).put("captureId", record.captureId)
                .put("archiveHash", record.sha256).put("stage", stage).put("recognitionState", "FAILED")
                .put("errorCode", "final-transcript-exceeds-result-limit").toString().toByteArray(StandardCharsets.UTF_8)
            enqueueMessage(nodeId, CaptureWire.RECEIPT, error)
        }
    }

    private fun enqueueMessage(nodeId: String, path: String, bytes: ByteArray, timing: CaptureTiming? = null,
                               isCurrent: () -> Boolean = { true }) {
        if (bytes.size > CaptureProtocol.MAX_MESSAGE_BYTES) return
        timing?.mark("first-transcript-enqueued")
        val sender = if (path in setOf(CaptureWire.READY, CaptureWire.PARTIAL, CaptureWire.FINAL)) liveDelivery else delivery
        runCatching { sender.execute {
            if (!isCurrent()) return@execute
            timing?.mark("first-transcript-send-started")
            runCatching { sendMessage(nodeId, path, bytes) }
                .onSuccess { timing?.mark("first-transcript-send-completed") }
                .onFailure { timing?.mark("transcript-send-failed") }
        } }
    }

    fun barrier(done: () -> Unit) { liveDelivery.execute { delivery.execute(done) } }

    fun shutdown() {
        delivery.shutdownNow()
        liveDelivery.shutdownNow()
    }

    companion object {
        fun wearReceiptSender(context: Context): (String, String, ByteArray) -> Unit =
            PhoneReceiptChannelSender.google(context)::send
    }
}
