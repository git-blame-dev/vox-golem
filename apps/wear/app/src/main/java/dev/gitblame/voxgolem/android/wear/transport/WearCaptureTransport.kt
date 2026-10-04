package dev.gitblame.voxgolem.android.wear.transport

import android.content.Context
import android.content.Intent
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.CapabilityClient
import com.google.android.gms.wearable.PutDataMapRequest
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.TransferState
import org.json.JSONObject
import java.io.BufferedOutputStream
import java.io.DataOutputStream
import java.io.File
import java.nio.charset.StandardCharsets
import java.time.Instant
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/** Wear Data Layer adapter. The PCM spool remains authoritative regardless of transport state. */
class WearCaptureTransport(private val context: Context) {
    private val repository = CaptureRepository(context)
    private val capability = Wearable.getCapabilityClient(context)
    private val messages = Wearable.getMessageClient(context)
    private val channels = Wearable.getChannelClient(context)
    private val data = Wearable.getDataClient(context)

    fun beginLive(record: CaptureRecord) {
        LivePipe(context.applicationContext, record, capability, messages, channels).also { it.start() }
            .also { activePipes[record.captureId] = it }
    }

    fun offerLive(frame: ByteArray) {
        activePipes.values.firstOrNull()?.offer(frame)
    }

    fun closeLive() {
        activePipes.values.toList().forEach { it.finish() }
        activePipes.clear()
    }

    fun publishPending(record: CaptureRecord) {
        val request = PutDataMapRequest.create("$PENDING_PATH/${record.captureId}").apply {
            dataMap.putString("protocol", PROTOCOL)
            dataMap.putString("captureId", record.captureId)
            dataMap.putString("status", record.transferState.name)
            dataMap.putString("finishDate", record.finishedLocalDate ?: "")
            dataMap.putString("timezone", record.timezoneId ?: "")
            dataMap.putLong("frameCount", record.durableFrames)
            dataMap.putLong("wavBytes", record.finalizedByteLength ?: 0L)
            dataMap.putString("sha256", record.sha256 ?: "")
            dataMap.putInt("sampleRate", record.format.sampleRate)
            dataMap.putInt("channels", record.format.channels)
            dataMap.putString("sourceKind", "watch")
        }.asPutDataRequest().setUrgent()
        runCatching { data.putDataItem(request) }
        CaptureTransferJob.schedule(context, immediate = true)
    }

    fun retryPending() {
        CaptureTransferJob.schedule(context, immediate = true)
    }

    internal fun transferPending(at: Instant = Instant.now()) {
        repository.pendingTransfers().filter {
            (it.captureState == CaptureState.SAVED || it.captureState == CaptureState.INTERRUPTED) &&
                it.finalizedByteLength != null && it.localAudioPath?.endsWith(".wav") == true
            }.filter { it.transferState != TransferState.PHONE_ARCHIVED }
            .forEach { record ->
                val retryAt = record.nextRetryAtUtc?.let { runCatching { Instant.parse(it) }.getOrNull() }
                if (retryAt != null && retryAt.isAfter(at)) {
                    return@forEach
                }
                if (record.transferState == TransferState.PHONE_STAGED) {
                    repository.updateTransfer(record.captureId, TransferState.PHONE_STAGED, at)
                    requestRemoteStatus(repository.get(record.captureId) ?: record)
                } else {
                    repository.beginTransferAttempt(record.captureId, at)?.let(::transferArchive)
                }
            }
    }

    private fun requestRemoteStatus(record: CaptureRecord) {
        val node = reachablePhones(capability).singleOrNull() ?: return
        val request = JSONObject().put("protocol", PROTOCOL).put("captureId", record.captureId)
            .put("requestId", java.util.UUID.randomUUID().toString())
            .put("knownStage", record.transferState.name).put("errorCode", record.lastErrorCode)
            .toString().toByteArray(StandardCharsets.UTF_8)
        if (CaptureProtocol.validatePayloadSize(request.size)) {
            runCatching { messages.sendMessage(node.id, CaptureProtocol.STATUS, request) }
        }
    }

    private fun transferArchive(record: CaptureRecord) {
        val node = reachablePhones(capability).singleOrNull() ?: return
        val wav = repository.audio(record.captureId)
        if (!wav.isFile || wav.length() != record.finalizedByteLength || record.sha256 == null) return
        val attemptId = java.util.UUID.randomUUID().toString()
        val path = "$ARCHIVE_PATH/${record.captureId}/$attemptId"
        val channel = runCatching { Tasks.await(channels.openChannel(node.id, path)) }.getOrNull() ?: return
        try {
            val output = Tasks.await(channels.getOutputStream(channel))
            DataOutputStream(BufferedOutputStream(output)).use { stream ->
                val header = JSONObject().apply {
                    put("protocol", PROTOCOL); put("captureId", record.captureId)
                    put("attemptId", attemptId); put("sampleRate", record.format.sampleRate)
                    put("channels", record.format.channels); put("bitsPerSample", record.format.bitsPerSample)
                    put("frameCount", record.durableFrames)
                    put("wavBytes", record.finalizedByteLength); put("sha256", record.sha256)
                    put("finishDate", record.finishedLocalDate); put("timezone", record.timezoneId)
                    put("captureState", record.captureState.name); put("stopReason", record.stopReason)
                    put("createdAtUtc", record.createdAtUtc); put("startedAtUtc", record.startedAtUtc)
                    put("finishedAtUtc", record.finishedAtUtc)
                }.toString().toByteArray(StandardCharsets.UTF_8)
                require(header.size <= MAX_HEADER_BYTES)
                stream.writeInt(header.size); stream.write(header); stream.writeLong(wav.length())
                wav.inputStream().buffered().use { input -> input.copyTo(stream, 16 * 1024) }
                stream.flush()
            }
            // A completed write is not a receipt. Preserve audio/outbox until a phone receipt arrives.
        } catch (_: Exception) {
            repository.updateTransfer(record.captureId, TransferState.PENDING)
        } finally {
            runCatching { Tasks.await(channels.close(channel)) }
        }
    }

    fun isSelectedReceiver(nodeId: String): Boolean = reachablePhones(capability).singleOrNull()?.id == nodeId

    companion object {
        const val PROTOCOL = CaptureProtocol.PROTOCOL
        const val PHONE_CAPABILITY = CaptureProtocol.PHONE_CAPABILITY
        const val BASE_PATH = CaptureProtocol.BASE
        const val PENDING_PATH = CaptureProtocol.PENDING
        const val ARCHIVE_PATH = CaptureProtocol.ARCHIVE
        const val LIVE_PATH = CaptureProtocol.LIVE
        const val RECEIPT_PATH = CaptureProtocol.RECEIPT
        private const val MAX_HEADER_BYTES = CaptureProtocol.MAX_HEADER_BYTES
        private val activePipes = java.util.concurrent.ConcurrentHashMap<String, LivePipe>()
    }
}

private class LivePipe(
    private val context: Context,
    private val record: CaptureRecord,
    private val capability: com.google.android.gms.wearable.CapabilityClient,
    private val messages: com.google.android.gms.wearable.MessageClient,
    private val channels: ChannelClient,
) {
    private val queue = ArrayBlockingQueue<ByteArray>(20)
    private val executor = Executors.newSingleThreadExecutor()
    @Volatile private var stopped = false

    fun start() {
        executor.execute {
            val node = reachablePhones(capability).singleOrNull() ?: return@execute
            val sessionId = record.recognitionSessionId ?: return@execute
            val path = "${WearCaptureTransport.LIVE_PATH}/${record.captureId}/$sessionId"
            val channel = runCatching { Tasks.await(channels.openChannel(node.id, path)) }.getOrNull() ?: return@execute
            try {
                val output = Tasks.await(channels.getOutputStream(channel))
                DataOutputStream(BufferedOutputStream(output)).use { stream ->
                    val start = JSONObject().apply {
                        put("protocol", WearCaptureTransport.PROTOCOL); put("captureId", record.captureId)
                        put("sessionId", sessionId); put("sampleRate", record.format.sampleRate)
                        put("channels", record.format.channels); put("bitsPerSample", record.format.bitsPerSample)
                        put("startedAt", record.startedAtUtc)
                    }.toString().toByteArray(StandardCharsets.UTF_8)
                    stream.writeInt(start.size); stream.write(start)
                    Tasks.await(messages.sendMessage(node.id, CaptureProtocol.START, start))
                    var sequence = 0L
                    while (!stopped || queue.isNotEmpty()) {
                        val bytes = queue.poll(200, TimeUnit.MILLISECONDS) ?: continue
                        stream.writeLong(sequence++)
                        stream.writeInt(bytes.size)
                        stream.write(bytes)
                    }
                    stream.writeLong(-1L); stream.flush()
                    val latest = runCatching { CaptureRepository(context).get(record.captureId) }.getOrNull() ?: record
                    val stop = JSONObject().apply {
                        put("protocol", WearCaptureTransport.PROTOCOL); put("captureId", record.captureId)
                        put("sessionId", sessionId); put("frameCount", latest.durableFrames)
                        put("finishDate", latest.finishedLocalDate ?: ""); put("timezone", latest.timezoneId ?: "")
                    }.toString().toByteArray(StandardCharsets.UTF_8)
                    runCatching { Tasks.await(messages.sendMessage(node.id, CaptureProtocol.STOP, stop)) }
                }
            } catch (_: Exception) {
                // Live preview is expendable; finite archive and local recording remain intact.
            } finally {
                runCatching { Tasks.await(channels.close(channel)) }
            }
        }
    }

    fun offer(frame: ByteArray) {
        if (!stopped && frame.isNotEmpty() && frame.size <= 3_200 && !queue.offer(frame)) {
            stopped = true
            queue.clear()
        }
    }

    fun finish() {
        stopped = true
        executor.shutdown()
    }
}

private fun reachablePhones(client: CapabilityClient): List<com.google.android.gms.wearable.Node> =
    runCatching {
        Tasks.await(client.getCapability(WearCaptureTransport.PHONE_CAPABILITY, CapabilityClient.FILTER_REACHABLE)).nodes.toList()
    }.getOrDefault(emptyList())

class CaptureWearListenerService : com.google.android.gms.wearable.WearableListenerService() {
    override fun onPeerConnected(node: com.google.android.gms.wearable.Node) {
        val transport = WearCaptureTransport(this)
        if (transport.isSelectedReceiver(node.id)) transport.retryPending()
    }

    override fun onMessageReceived(event: com.google.android.gms.wearable.MessageEvent) {
        if (!CaptureProtocol.validatePayloadSize(event.data.size)) return
        val body = runCatching { JSONObject(String(event.data, StandardCharsets.UTF_8)) }.getOrNull() ?: return
        if (body.optString("protocol") != WearCaptureTransport.PROTOCOL) return
        val id = body.optString("captureId")
        if (!dev.gitblame.voxgolem.android.wear.capture.CaptureIdentity.isValid(id)) return
        val transport = WearCaptureTransport(this)
        if (!transport.isSelectedReceiver(event.sourceNodeId)) return
        val repository = CaptureRepository(this)
        val record = repository.get(id) ?: return
        if (event.path == CaptureProtocol.READY || event.path == CaptureProtocol.PARTIAL || event.path == CaptureProtocol.FINAL) {
            val sessionId = body.optString("sessionId")
            if (record.recognitionSessionId != sessionId) return
            if (event.path != CaptureProtocol.READY) {
                val isFinal = event.path == CaptureProtocol.FINAL
                val revision = body.optLong("revision", -1)
                val text = body.optString("fullText")
                if (revision > 0 && text.toByteArray(StandardCharsets.UTF_8).size <= 256 * 1024) {
                    repository.updateTranscript(id, sessionId, revision, text, isFinal)
                    sendBroadcast(Intent(ACTION_STATE).setPackage(packageName))
                }
            }
            return
        }
        if (event.path != CaptureProtocol.RECEIPT || !body.optString("archiveHash").equals(record.sha256, ignoreCase = true)) return
        val status = when (body.optString("stage")) {
            "PHONE_STAGED" -> TransferState.PHONE_STAGED
            "PHONE_ARCHIVED" -> TransferState.PHONE_ARCHIVED
            else -> return
        }
        repository.updateTransfer(id, status)
        CaptureTransferJob.schedule(this)
        sendBroadcast(Intent(ACTION_STATE).setPackage(packageName))
    }

    companion object { const val ACTION_STATE = "dev.gitblame.voxgolem.capture.STATE" }
}
