package dev.gitblame.voxgolem.android.wear.transport

import android.content.Context
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.PutDataMapRequest
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.TransferState
import dev.gitblame.voxgolem.capture.CapturePurpose
import org.json.JSONObject
import java.time.Instant
import java.util.concurrent.TimeUnit

/** The local spool/outbox remain authoritative; a channel write is never a receipt. */
class WearCaptureTransport internal constructor(private val context: Context, private val sdk: CaptureWearSdk) : AutoCloseable {
    constructor(context: Context) : this(context, GoogleCaptureWearSdk(context))
    private val repository = CaptureRepository(context)
    private val lock = Any()
    private var retired = false
    private var archive: ArchiveTransfer? = null

    internal fun beginLive(record: CaptureRecord): LiveCaptureHandle = LiveCapturePipe(record, sdk,
        onUnavailable = { code -> repository.reportLiveTransportFailure(record.captureId, record.recognitionSessionId, code) }).also { it.start() }

    fun publishPending(record: CaptureRecord) {
        if (record.purpose != CapturePurpose.NOTE) return
        val request = PutDataMapRequest.create("$PENDING_PATH/${record.captureId}").apply {
            dataMap.putString("protocol", PROTOCOL); dataMap.putString("captureId", record.captureId)
            dataMap.putString("status", record.transferState.name); dataMap.putString("finishDate", record.finishedLocalDate ?: "")
            dataMap.putString("timezone", record.timezoneId ?: ""); dataMap.putLong("frameCount", record.durableFrames)
            dataMap.putLong("wavBytes", record.finalizedByteLength ?: 0L); dataMap.putString("sha256", record.sha256 ?: "")
            dataMap.putInt("sampleRate", record.format.sampleRate); dataMap.putInt("channels", record.format.channels)
            dataMap.putString("sourceKind", "watch")
        }.asPutDataRequest().setUrgent()
        runCatching { Wearable.getDataClient(context).putDataItem(request) }
        CaptureTransferJob.schedule(context, immediate = true)
    }
    fun retryPending() {
        CaptureWearListenerService.retryReceiptRetirements()
        CaptureTransferJob.schedule(context, immediate = true)
    }
    private fun isRetired() = synchronized(lock) { retired } || Thread.currentThread().isInterrupted
    internal fun transferPending(at: Instant = Instant.now()) {
        CaptureWearListenerService.retryReceiptRetirements()
        for (record in repository.pendingTransfers().filter {
            it.purpose == CapturePurpose.NOTE && (it.captureState == CaptureState.SAVED || it.captureState == CaptureState.INTERRUPTED) &&
                it.finalizedByteLength != null && it.localAudioPath?.endsWith(".wav") == true
        }) {
            if (isRetired()) return
            val retryAt = record.nextRetryAtUtc?.let { runCatching { Instant.parse(it) }.getOrNull() }
            if (retryAt != null && retryAt.isAfter(at)) continue
            if (record.transferState >= TransferState.PHONE_STAGED) {
                repository.updateTransfer(record.captureId, record.transferState, at)
                requestRemoteStatus(repository.get(record.captureId) ?: record)
            } else repository.beginTransferAttempt(record.captureId, at)?.let(::transferArchive)
        }
    }
    private fun requestRemoteStatus(record: CaptureRecord) {
        val node = receiverFor(record) ?: return
        if (isRetired()) return
        val request = JSONObject().put("protocol", PROTOCOL).put("captureId", record.captureId)
            .put("requestId", java.util.UUID.randomUUID().toString()).put("knownStage", record.transferState.name)
            .put("errorCode", record.lastErrorCode?.let { dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(it) }).toString().toByteArray(Charsets.UTF_8)
        if (CaptureProtocol.validatePayloadSize(request.size)) runCatching { sdk.send(node.id, CaptureProtocol.STATUS, request) }
    }
    private fun transferArchive(record: CaptureRecord) {
        if (record.purpose != CapturePurpose.NOTE) return
        val node = receiverFor(record) ?: return
        val wav = repository.audio(record.captureId)
        if (!wav.isFile || wav.length() != record.finalizedByteLength || record.sha256 == null) return
        val attempt = synchronized(lock) {
            if (retired) return
            ArchiveTransfer(sdk).also { archive = it }
        }
        try { attempt.transfer(node.id, record, wav) }
        catch (_: Exception) { if (!isRetired()) repository.updateTransfer(record.captureId, TransferState.PENDING) }
        finally { attempt.close(); synchronized(lock) { if (archive === attempt) archive = null } }
    }
    fun isSelectedReceiver(nodeId: String): Boolean = reachablePhones().singleOrNull()?.id == nodeId
    private fun reachablePhones(): List<com.google.android.gms.wearable.Node> = runCatching {
        Tasks.await(sdk.phones(), 10, TimeUnit.SECONDS).nodes.toList()
    }.getOrDefault(emptyList())
    private fun receiverFor(record: CaptureRecord): com.google.android.gms.wearable.Node? {
        val nodes = reachablePhones()
        if (isRetired()) return null
        if (nodes.size == 1) return nodes.single()
        if (record.captureState == CaptureState.SAVED && !record.completionNotified) {
            val code = if (nodes.isEmpty()) "phone-unavailable" else "multiple-phone-receivers"
            if (repository.reportCompletionProblem(record.captureId, code)) CaptureSignals.attention(context, "Audio saved · transcription pending · $code", record.captureId)
        }
        return null
    }
    override fun close() {
        val owned = synchronized(lock) { retired = true; archive.also { archive = null } }
        owned?.close()
    }
    companion object {
        const val PROTOCOL = CaptureProtocol.PROTOCOL
        const val PHONE_CAPABILITY = CaptureProtocol.PHONE_CAPABILITY
        const val BASE_PATH = CaptureProtocol.BASE
        const val PENDING_PATH = CaptureProtocol.PENDING
        const val ARCHIVE_PATH = CaptureProtocol.ARCHIVE
        const val LIVE_PATH = CaptureProtocol.LIVE
        const val RECEIPT_PATH = CaptureProtocol.RECEIPT
    }
}
