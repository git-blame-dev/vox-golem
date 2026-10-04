package dev.gitblame.voxgolem.capture

import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.DataEvent
import com.google.android.gms.wearable.DataEventBuffer
import com.google.android.gms.wearable.MessageEvent
import com.google.android.gms.wearable.Node
import com.google.android.gms.wearable.Wearable
import com.google.android.gms.wearable.WearableListenerService
import org.json.JSONObject
import java.io.DataInputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.security.MessageDigest
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/** Wear Data Layer entry point for live PCM, finite WAV archives, status and receipts. */
class CaptureListenerService : WearableListenerService() {
    private val repository by lazy { CaptureRepository(this) }
    private val coordinator by lazy { PhoneCaptureCoordinator.shared(this) }
    private val work: ExecutorService by lazy { Executors.newSingleThreadExecutor() }

    override fun onCreate() {
        super.onCreate()
    }

    override fun onDataChanged(events: DataEventBuffer) {
        if (isWatch()) return
        events.forEach { event ->
            if (event.type != DataEvent.TYPE_CHANGED) return@forEach
            val uri = event.dataItem.uri
            val pathId = uri.path.orEmpty().removePrefix("${CaptureWire.PENDING}/")
            if (!uri.path.orEmpty().startsWith("${CaptureWire.PENDING}/") || !CaptureProtocol.isValidUuid(pathId)) return@forEach
            val map = runCatching { com.google.android.gms.wearable.DataMapItem.fromDataItem(event.dataItem).dataMap }.getOrNull() ?: return@forEach
            val id = map.getString("captureId") ?: return@forEach
            if (!CaptureProtocol.isValidUuid(id) || !id.equals(pathId, ignoreCase = true) || map.getString("protocol") != CaptureWire.PROTOCOL) return@forEach
            // The item is discovery metadata only; audio and durable receipts use the channel/message paths.
        }
    }

    override fun onMessageReceived(event: MessageEvent) {
        if (isWatch()) return
        if (event.data.size > CaptureProtocol.MAX_MESSAGE_BYTES) return
        val json = runCatching { JSONObject(String(event.data, Charsets.UTF_8)) }.getOrNull() ?: return
        if (!CaptureWire.isProtocol(json)) return
        val captureId = json.optString("captureId")
        if (!CaptureProtocol.isValidUuid(captureId)) return
        when (event.path) {
            CaptureWire.START -> {
                val sessionId = json.optString("sessionId")
                val format = CaptureWire.formatFrom(json)
                if (event.sourceNodeId.isBlank() || !CaptureProtocol.isValidUuid(sessionId) || !CaptureProtocol.validateFormat(format)) return
                val startedAt = json.optString("startedAt")
                if (runCatching { java.time.Instant.parse(startedAt) }.isFailure) return
                coordinator.beginLive(captureId, sessionId, event.sourceNodeId, format, startedAt)
            }
            CaptureWire.STOP -> {
                val sessionId = json.optString("sessionId")
                val frames = json.optLong("frameCount", -1)
                if (event.sourceNodeId.isNotBlank() && CaptureProtocol.isValidUuid(sessionId) && frames >= 0) {
                    coordinator.noteLiveStop(captureId, sessionId, event.sourceNodeId, frames)
                }
            }
            CaptureWire.STATUS -> handleStatus(event.sourceNodeId, json)
        }
    }

    override fun onChannelOpened(channel: ChannelClient.Channel) {
        if (isWatch()) {
            runCatching { Tasks.await(Wearable.getChannelClient(this).close(channel)) }
            return
        }
        val liveMatch = Regex("^${Regex.escape(CaptureWire.LIVE)}/([0-9a-fA-F-]{36})/([0-9a-fA-F-]{36})$").matchEntire(channel.path)
        val archiveMatch = Regex("^${Regex.escape(CaptureWire.ARCHIVE)}/([0-9a-fA-F-]{36})/([0-9a-fA-F-]{36})$").matchEntire(channel.path)
        if (liveMatch == null && archiveMatch == null) {
            runCatching { Tasks.await(Wearable.getChannelClient(this).close(channel)) }
            return
        }
        work.execute {
            val input = runCatching { Tasks.await(Wearable.getChannelClient(this).getInputStream(channel)) }.getOrNull()
            if (input == null) {
                runCatching { Tasks.await(Wearable.getChannelClient(this).close(channel)) }
                return@execute
            }
            try {
                if (liveMatch != null) receiveLive(channel, DataInputStream(input), liveMatch.groupValues[1], liveMatch.groupValues[2])
                else receiveArchive(channel, DataInputStream(input), archiveMatch!!.groupValues[1], archiveMatch.groupValues[2])
            } catch (failure: Exception) {
                val id = liveMatch?.groupValues?.get(1) ?: archiveMatch?.groupValues?.get(1)
                if (id != null && CaptureProtocol.isValidUuid(id)) coordinator.markArchiveFailure(id, failure.javaClass.simpleName.lowercase())
            } finally {
                runCatching { input.close() }
                runCatching { Tasks.await(Wearable.getChannelClient(this).close(channel)) }
            }
        }
    }

    private fun receiveLive(channel: ChannelClient.Channel, input: DataInputStream, captureId: String, sessionId: String) {
        if (channel.nodeId.isBlank() || !CaptureProtocol.isValidUuid(captureId) || !CaptureProtocol.isValidUuid(sessionId)) throw IOException("invalid-live-identity")
        val header = CaptureWire.liveHeader(CaptureWire.readJsonHeader(input, CaptureWire.MAX_LIVE_HEADER_BYTES))
        if (!CaptureWire.validLiveStart(header, captureId, sessionId)) throw IOException("invalid-live-header")
        val format = header.format
        coordinator.beginLive(captureId, sessionId, channel.nodeId, format, header.startedAt)
        var previous: Long? = null
        var totalPcmBytes = 0L
        val maxPcmBytes = format.sampleRate.toLong() * format.channels * format.bitsPerSample / 8 * MAX_CAPTURE_MILLIS / 1_000
        while (true) {
            val sequence = input.readLong()
            if (sequence == -1L) break
            val count = input.readInt()
            if (!CaptureProtocol.validateFrame(format, sequence, count, previous)) throw IOException("invalid-live-frame")
            if (totalPcmBytes > maxPcmBytes - count) throw IOException("live-audio-exceeds-capture-limit")
            val pcm = ByteArray(count)
            input.readFully(pcm)
            coordinator.appendLive(sessionId, pcm)
            previous = sequence
            totalPcmBytes += count
        }
        // The stop message is a separate channel and may arrive before EOF; count received aligned samples.
        val frames = totalPcmBytes / (format.channels * format.bitsPerSample / 8)
        coordinator.finishLive(captureId, sessionId, channel.nodeId, frames)
    }

    private fun receiveArchive(channel: ChannelClient.Channel, input: DataInputStream, captureId: String, attemptId: String) {
        if (channel.nodeId.isBlank() || !CaptureProtocol.isValidUuid(captureId) || !CaptureProtocol.isValidUuid(attemptId)) throw IOException("invalid-archive-identity")
        val header = CaptureWire.archiveHeader(CaptureWire.readJsonHeader(input, CaptureWire.MAX_HEADER_BYTES))
        if (!CaptureWire.validArchiveHeader(header, captureId, attemptId)) throw IOException("invalid-archive-header")
        val format = header.format
        val expectedLength = header.wavBytes
        val streamLength = input.readLong()
        val maximumPcmBytes = format.sampleRate.toLong() * format.channels * format.bitsPerSample / 8 * MAX_CAPTURE_MILLIS / 1_000
        if (streamLength != expectedLength || streamLength !in 44..(44 + maximumPcmBytes)) throw IOException("invalid-archive-length")

        val old = repository.get(captureId)
        if (old?.sha256 != null && (!old.sha256.equals(header.sha256, true) || old.finalizedByteLength != expectedLength)) {
            throw IOException("capture-id-content-conflict")
        }
        if (old != null && old.sha256 == null && old.localAudioPath != null) {
            throw IOException("capture-id-has-nonprovisional-record")
        }

        val directory = File(File(filesDir, "captures"), captureId.lowercase())
        if (!directory.exists() && !directory.mkdirs()) throw IOException("archive-directory-create-failed")
        val finalWav = File(directory, "received.wav")
        val part = File(directory, "received.wav.part")
        if (finalWav.exists()) {
            if (finalWav.length() != expectedLength || !sha256(finalWav).equals(header.sha256, ignoreCase = true)) {
                throw IOException("existing-staged-audio-conflict")
            }
            verifyWav(finalWav, format, header.frameCount)
            coordinator.onArchiveReceived(archiveRecord(header, old, finalWav), channel.nodeId)
            return
        }
        val digest = MessageDigest.getInstance("SHA-256")
        FileOutputStream(part).use { output ->
            val buffer = ByteArray(64 * 1024)
            var remaining = streamLength
            while (remaining > 0) {
                val count = input.read(buffer, 0, minOf(buffer.size.toLong(), remaining).toInt())
                if (count < 0) throw IOException("truncated-archive")
                digest.update(buffer, 0, count)
                output.write(buffer, 0, count)
                remaining -= count
            }
            output.fd.sync()
        }
        val actualHash = digest.digest().joinToString("") { "%02x".format(it) }
        if (part.length() != expectedLength || !actualHash.equals(header.sha256, ignoreCase = true)) {
            part.delete()
            throw IOException("archive-sha256-or-length-mismatch")
        }
        verifyWav(part, format, header.frameCount)
        if (!part.renameTo(finalWav)) throw IOException("cannot-finalize-staged-archive")

        coordinator.onArchiveReceived(archiveRecord(header, old, finalWav), channel.nodeId)
    }

    private fun archiveRecord(header: CaptureWire.ArchiveHeader, old: CaptureRecord?, wav: File): CaptureRecord {
        val finishedAt = header.finishedAtUtc ?: old?.finishedAtUtc ?: Instant.now().toString()
        return (old ?: CaptureRecord(captureId = header.captureId, createdAtUtc = header.createdAtUtc ?: finishedAt)).copy(
            sourceKind = "watch",
            startedAtUtc = header.startedAtUtc ?: old?.startedAtUtc,
            finishedAtUtc = finishedAt,
            finishedLocalDate = LocalDate.parse(header.finishDate).toString(),
            timezoneId = ZoneId.of(header.timezone).id,
            captureState = header.captureState,
            stopReason = header.stopReason ?: old?.stopReason,
            observedFrames = header.frameCount,
            durableFrames = header.frameCount,
            format = header.format,
            localAudioPath = wav.absolutePath,
            finalizedByteLength = header.wavBytes,
            sha256 = header.sha256.lowercase(),
            transferState = TransferState.PHONE_STAGED,
            archiveState = ArchiveState.STAGED,
            orgState = if (old?.finalText.isNullOrBlank()) OrgState.WAITING_FOR_TRANSCRIPT else OrgState.READY,
        )
    }

    private fun verifyWav(file: File, format: CaptureFormat, frames: Long) {
        if (file.length() != 44L + frames * format.channels * format.bitsPerSample / 8) throw IOException("wav-frame-length-mismatch")
        WavPcmReader(file, format).use { if (it.frameCount != frames) throw IOException("wav-header-does-not-match-manifest") }
    }

    private fun sha256(file: File): String = FileInputStream(file).use { input ->
        val digest = MessageDigest.getInstance("SHA-256")
        val buffer = ByteArray(64 * 1024)
        while (true) {
            val count = input.read(buffer)
            if (count < 0) break
            digest.update(buffer, 0, count)
        }
        digest.digest().joinToString("") { "%02x".format(it) }
    }

    private fun handleStatus(nodeId: String, json: JSONObject) {
        val requestId = json.optString("requestId")
        if (!CaptureProtocol.isValidUuid(requestId)) return
        val record = repository.get(json.optString("captureId")) ?: return
        if (record.sha256 == null) return
        val stage = when {
            record.transferState == TransferState.PHONE_ARCHIVED -> "PHONE_ARCHIVED"
            record.transferState == TransferState.PHONE_STAGED -> "PHONE_STAGED"
            else -> return
        }
        val response = JSONObject().put("protocol", CaptureWire.PROTOCOL).put("captureId", record.captureId)
            .put("requestId", requestId).put("knownStage", stage).put("archiveHash", record.sha256)
            .put("stage", stage).put("destinationId", record.exportDestinationId)
            .toString().toByteArray(Charsets.UTF_8)
        work.execute { runCatching { Tasks.await(Wearable.getMessageClient(this).sendMessage(nodeId, CaptureWire.RECEIPT, response)) } }
    }

    override fun onPeerConnected(node: Node) {
        // No background UI/service launch on connectivity alone. The next explicit live request starts processing.
    }

    private fun isWatch(): Boolean = packageManager.hasSystemFeature("android.hardware.type.watch")

    companion object {
        const val ACTION_STATE = "dev.gitblame.voxgolem.capture.STATE"
        private const val MAX_CAPTURE_MILLIS = 10 * 60 * 1_000L
    }
}
