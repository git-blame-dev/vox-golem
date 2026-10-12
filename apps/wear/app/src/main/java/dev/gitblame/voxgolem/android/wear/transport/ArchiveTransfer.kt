package dev.gitblame.voxgolem.android.wear.transport

import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.CapturePurpose
import org.json.JSONObject
import java.io.BufferedOutputStream
import java.io.DataOutputStream
import java.io.File
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

internal class ArchiveTransfer(private val sdk: CaptureWearSdk, private val timeoutMs: Long = 30_000) : AutoCloseable {
    private val ownership = CaptureChannelOwnership(sdk)
    fun transfer(nodeId: String, record: CaptureRecord, wav: File) {
        require(record.purpose == CapturePurpose.NOTE) { "non-note-archive-transfer-not-allowed" }
        val timeout = deadlines.schedule({ ownership.close() }, timeoutMs, TimeUnit.MILLISECONDS)
        try {
            val attemptId = java.util.UUID.randomUUID().toString()
            val channel = ownership.open(nodeId, "${CaptureProtocol.ARCHIVE}/${record.captureId}/$attemptId")
            val output = ownership.output(channel)
            val stream = DataOutputStream(BufferedOutputStream(output))
            val header = JSONObject().apply {
                put("protocol", CaptureProtocol.PROTOCOL); put("captureId", record.captureId)
                put("attemptId", attemptId); put("sampleRate", record.format.sampleRate)
                put("channels", record.format.channels); put("bitsPerSample", record.format.bitsPerSample)
                put("frameCount", record.durableFrames); put("wavBytes", record.finalizedByteLength); put("sha256", record.sha256)
                put("finishDate", record.finishedLocalDate); put("timezone", record.timezoneId)
                put("captureState", record.captureState.name); put("stopReason", record.stopReason)
                put("createdAtUtc", record.createdAtUtc); put("startedAtUtc", record.startedAtUtc); put("finishedAtUtc", record.finishedAtUtc)
                put("purpose", record.purpose.name); putOpt("requestedLanguage", record.requestedLanguage)
                put("speechNetworkFallback", record.speechNetworkFallback)
            }.toString().toByteArray(Charsets.UTF_8)
            require(header.size <= CaptureProtocol.MAX_HEADER_BYTES)
            stream.writeInt(header.size); stream.write(header); stream.writeLong(wav.length())
            wav.inputStream().buffered().use { input ->
                val buffer = ByteArray(16 * 1024)
                while (true) {
                    check(!ownership.isRetired && !Thread.currentThread().isInterrupted)
                    val count = input.read(buffer)
                    if (count < 0) break
                    stream.write(buffer, 0, count)
                }
            }
            stream.flush()
            check(!ownership.isRetired)
        } finally { timeout.cancel(false); ownership.close() }
    }
    override fun close() = ownership.close()
    companion object {
        private val deadlines = Executors.newSingleThreadScheduledExecutor { runnable -> Thread(runnable, "capture-archive-deadline").apply { isDaemon = true } }
    }
}
