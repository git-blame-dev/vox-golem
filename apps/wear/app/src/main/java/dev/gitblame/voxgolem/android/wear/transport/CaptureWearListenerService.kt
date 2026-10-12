package dev.gitblame.voxgolem.android.wear.transport

import android.content.Intent
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.android.wear.capture.CaptureIdentity
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.CaptureReceiptChannel
import dev.gitblame.voxgolem.capture.CaptureChannelLifetime
import dev.gitblame.voxgolem.capture.CaptureChannelRetirementOwner
import dev.gitblame.voxgolem.capture.CaptureErrorCodes
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureStartupTiming
import dev.gitblame.voxgolem.capture.RecognitionState
import dev.gitblame.voxgolem.capture.TransferState
import dev.gitblame.voxgolem.capture.CapturePurpose
import dev.gitblame.voxgolem.capture.LiveTranscriptionState
import org.json.JSONObject
import java.util.concurrent.TimeUnit

class CaptureWearListenerService : com.google.android.gms.wearable.WearableListenerService() {
    internal var selectedReceiver: (String) -> Boolean = { WearCaptureTransport(this).isSelectedReceiver(it) }
    internal var receiptStream: (ChannelClient.Channel) -> Task<java.io.InputStream> = { Wearable.getChannelClient(this).getInputStream(it) }
    internal var receiptOutput: (ChannelClient.Channel) -> Task<java.io.OutputStream> = { Wearable.getChannelClient(this).getOutputStream(it) }
    internal var closeReceiptChannel: (ChannelClient.Channel) -> Task<Void> = { Wearable.getChannelClient(this).close(it) }
    internal var receiptRetirements = processRetirements
    internal var observeReceiptClosures: () -> Unit = { observeClosures(applicationContext) }
    internal var receiptTimeoutMs: Long = 15_000
    override fun onPeerConnected(node: com.google.android.gms.wearable.Node) {
        val transport = WearCaptureTransport(this)
        if (transport.isSelectedReceiver(node.id)) transport.retryPending()
    }
    override fun onMessageReceived(event: com.google.android.gms.wearable.MessageEvent) {
        if (!CaptureProtocol.validatePayloadSize(event.data.size)) return
        val body = runCatching { JSONObject(String(event.data, Charsets.UTF_8)) }.getOrNull() ?: return
        if (body.optString("protocol") != WearCaptureTransport.PROTOCOL) return
        val id = body.optString("captureId")
        if (!CaptureIdentity.isValid(id)) return
        val textTiming = CaptureStartupTiming.forCapture(id)?.takeIf {
            event.path in setOf(CaptureProtocol.PARTIAL, CaptureProtocol.FINAL) && body.optString("fullText").isNotBlank()
        }
        textTiming?.mark("text-callback-entered")
        if (!selectedReceiver(event.sourceNodeId)) return
        textTiming?.mark("text-source-verified")
        val repository = CaptureRepository(this)
        val record = repository.get(id) ?: return
        if (record.purpose != CapturePurpose.NOTE) return
        if (event.path == CaptureProtocol.READY || event.path == CaptureProtocol.PARTIAL || event.path == CaptureProtocol.FINAL) {
            val sessionId = body.optString("sessionId")
            if (record.recognitionSessionId != sessionId) return
            if (event.path == CaptureProtocol.READY && body.optString("recognitionStatus") == "provider-ready") CaptureStartupTiming.forCapture(id)?.mark("phone-ready-received")
            if (event.path == CaptureProtocol.READY) {
                val status = LiveTranscriptionState.fromWire(body.optString("recognitionStatus")) ?: return
                if (repository.updateLiveStatus(id, sessionId, body.optLong("statusRevision", 0), status,
                        body.opt("canTranscribeLive") == true, body.optString("errorCode").takeIf(String::isNotBlank))) {
                    sendBroadcast(Intent(ACTION_STATE).setPackage(packageName))
                }
            }
            if (event.path != CaptureProtocol.READY) {
                val revision = body.optLong("revision", -1)
                val text = body.optString("fullText")
                if (revision > 0 && text.toByteArray(Charsets.UTF_8).size <= 256 * 1024) {
                    repository.updateTranscript(id, sessionId, revision, text, false)
                    if (text.isNotBlank()) CaptureStartupTiming.forCapture(id)?.mark("first-text-received")
                    sendBroadcast(Intent(ACTION_STATE).setPackage(packageName))
                }
            }
            return
        }
        if (event.path == CaptureProtocol.RECEIPT) handleReceipt(id, body, record)
    }
    override fun onChannelOpened(channel: ChannelClient.Channel) {
        if (receiptLifetimes.containsKey(channel)) return
        val closedBeforeAdoption = java.util.concurrent.atomic.AtomicBoolean(false)
        pendingNativeClosures[channel] = closedBeforeAdoption
        runCatching { observeReceiptClosures() }
        receiptRetirements.retryFailedRetirements()
        // Process work owns this channel after the transient listener is destroyed.
        // GMS dispatch is backpressured if even rejection cleanup is saturated; no handle is forgotten.
        lateinit var lifetime: CaptureChannelLifetime
        lifetime = receiptRetirements.awaitReservation(receiptTimeoutMs) { receiptLifetimes.remove(channel, lifetime) }
        val admitted = synchronized(receiptLifetimes) {
            if (receiptLifetimes.containsKey(channel)) {
                pendingNativeClosures.remove(channel, closedBeforeAdoption)
                lifetime.nativeAcquisitionFailed(); return
            }
            val accepted = receiptLifetimes.size < 10 && CaptureReceiptChannel.captureId(channel.path) != null
            receiptLifetimes[channel] = lifetime
            accepted
        }
        lifetime.acquiredNative { completed ->
            closeReceiptChannel(channel).addOnCompleteListener(DIRECT) { completed(it.isSuccessful) }
        }
        pendingNativeClosures.remove(channel)
        if (closedBeforeAdoption.get()) lifetime.confirmNativeRetired()
        if (!admitted || Thread.currentThread().isInterrupted) { lifetime.close(); return }
        try { receiptWorkers.execute {
            var waitingForSenderClose = false
            try {
                if (lifetime.isRetired || !selectedReceiver(channel.nodeId) || lifetime.isRetired) return@execute
                lifetime.beginStreamAcquisition()
                val task = try { receiptStream(channel) } catch (failure: Exception) {
                    lifetime.streamAcquisitionCompleted(null); throw failure
                }
                task.addOnCompleteListener(DIRECT) { lifetime.streamAcquisitionCompleted(if (it.isSuccessful) it.result else null) }
                val input = Tasks.await(task, 5, TimeUnit.SECONDS)
                if (!lifetime.isRetired && receiveChannelReceipt(channel.path, channel.nodeId, input, lifetime)) {
                    lifetime.beginStreamAcquisition()
                    val sending = try { receiptOutput(channel) } catch (failure: Exception) {
                        lifetime.streamAcquisitionCompleted(null); throw failure
                    }
                    sending.addOnCompleteListener(DIRECT) { lifetime.streamAcquisitionCompleted(if (it.isSuccessful) it.result else null) }
                    val output = Tasks.await(sending, 5, TimeUnit.SECONDS)
                    if (!lifetime.isRetired) {
                        output.write(CaptureReceiptChannel.CONSUMED); output.flush(); output.close()
                        // The sender consumes this acknowledgement before closing the native channel.
                        waitingForSenderClose = true
                    }
                }
            } catch (_: Exception) { /* Retry via the ordinary exact-node status path. */ }
            finally { if (!waitingForSenderClose) lifetime.close() }
        } } catch (_: java.util.concurrent.RejectedExecutionException) { lifetime.close() }
    }
    override fun onChannelClosed(channel: ChannelClient.Channel, closeReason: Int, appSpecificErrorCode: Int) {
        confirmedClosed(channel)
    }
    internal fun receiveChannelReceipt(path: String, sourceNodeId: String, input: java.io.InputStream,
                                       lifetime: CaptureChannelLifetime): Boolean {
        input.use {
            val id = CaptureReceiptChannel.captureId(path) ?: return false
            if (!selectedReceiver(sourceNodeId)) return false
            val bytes = runCatching { CaptureReceiptChannel.read(input) }.getOrNull() ?: return false
            val body = runCatching { JSONObject(String(bytes, Charsets.UTF_8)) }.getOrNull() ?: return false
            if (body.optString("protocol") != WearCaptureTransport.PROTOCOL || !body.optString("captureId").equals(id, ignoreCase = true)) return false
            // Discovery can involve SDK waits; never hold the deadline/commit gate through it.
            if (!selectedReceiver(sourceNodeId)) return false
            var consumed = false
            val admitted = lifetime.ifActive { CaptureRepository(this).get(id)?.let { consumed = handleReceipt(id, body, it) } }
            return admitted && consumed
        }
    }
    private fun handleReceipt(id: String, body: JSONObject, record: CaptureRecord): Boolean {
        if (body.optString("stage") == "AUTHORIZATION_FAILED") {
            if (body.optString("errorCode") != "capture-origin-not-authorized") return false
            val repository = CaptureRepository(this)
            if (repository.reportCompletionProblem(id, "capture-origin-not-authorized")) {
                CaptureSignals.attention(this, "Capture origin is not authorized", id)
            }
            sendBroadcast(Intent(ACTION_STATE).setPackage(packageName))
            return true
        }
        if (!body.optString("archiveHash").equals(record.sha256, ignoreCase = true)) return false
        val repository = CaptureRepository(this)
        val status = when (body.optString("stage")) {
            "PHONE_STAGED" -> TransferState.PHONE_STAGED
            "PHONE_ARCHIVED" -> TransferState.PHONE_ARCHIVED
            else -> return false
        }
        repository.updateTransfer(id, status)
        val recognition = runCatching { RecognitionState.valueOf(body.optString("recognitionState")) }.getOrNull()
        if (recognition == RecognitionState.FINAL && body.has("finalText")) {
            val finalText = body.opt("finalText") as? String ?: return false
            if (repository.acceptPhoneCompletion(id, body.getString("archiveHash"), finalText, body.optLong("transcriptVersion", -1)) && repository.claimCompletionFeedback(id)) CaptureSignals.completed(this, id)
        } else if (recognition in setOf(RecognitionState.FAILED, RecognitionState.UNAVAILABLE, RecognitionState.NO_SPEECH) || body.has("errorCode")) {
            val code = CaptureErrorCodes.safe(body.optString("errorCode"),
                if (recognition == RecognitionState.NO_SPEECH) "no-speech-detected" else "transcription-unavailable")
            if (repository.reportCompletionProblem(id, code, recognition)) CaptureSignals.attention(this, "Audio saved · transcription pending · $code", id)
        }
        CaptureTransferJob.schedule(this)
        sendBroadcast(Intent(ACTION_STATE).setPackage(packageName))
        return true
    }
    companion object {
        const val ACTION_STATE = "dev.gitblame.voxgolem.capture.STATE"
        private val DIRECT = java.util.concurrent.Executor { it.run() }
        private val processRetirements = CaptureChannelRetirementOwner(64)
        private val pendingNativeClosures = java.util.concurrent.ConcurrentHashMap<ChannelClient.Channel, java.util.concurrent.atomic.AtomicBoolean>()
        private val observerRequested = java.util.concurrent.atomic.AtomicBoolean(false)
        @Volatile private var observerClient: ChannelClient? = null
        private val closeObserver = object : ChannelClient.ChannelCallback() {
            override fun onChannelClosed(channel: ChannelClient.Channel, closeReason: Int, appSpecificErrorCode: Int) = confirmedClosed(channel)
        }
        private fun confirmedClosed(channel: ChannelClient.Channel) {
            pendingNativeClosures[channel]?.set(true)
            receiptLifetimes[channel]?.confirmNativeRetired()
        }
        /** SDK callback dispatch is independent of the backpressured WearableListenerService queue. */
        private fun observeClosures(context: android.content.Context) {
            if (observerClient == null) synchronized(processRetirements) {
                if (observerClient == null) observerClient = Wearable.getChannelClient(context.applicationContext)
            }
            requestObserver()
        }
        private fun requestObserver() {
            val client = observerClient ?: return
            if (!observerRequested.compareAndSet(false, true)) return
            try { client.registerChannelCallback(closeObserver).addOnCompleteListener(DIRECT) {
                if (!it.isSuccessful) observerRequested.set(false)
            } } catch (_: Exception) { observerRequested.set(false) }
        }
        internal fun retryReceiptRetirements() { requestObserver(); processRetirements.retryFailedRetirements() }
        private val receiptLifetimes = java.util.concurrent.ConcurrentHashMap<ChannelClient.Channel, CaptureChannelLifetime>()
        private val receiptWorkers = java.util.concurrent.ThreadPoolExecutor(2, 2, 0, TimeUnit.MILLISECONDS,
            java.util.concurrent.ArrayBlockingQueue(8), { runnable -> Thread(runnable, "capture-receipt").apply { isDaemon = true } })
    }
}
