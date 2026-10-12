package dev.gitblame.voxgolem.android.wear.transport

import com.google.android.gms.tasks.Tasks
import dev.gitblame.voxgolem.capture.CaptureProtocol
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureStartupTiming
import dev.gitblame.voxgolem.capture.CapturePurpose
import org.json.JSONObject
import java.io.BufferedOutputStream
import java.io.DataOutputStream
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

internal interface LiveCaptureHandle : AutoCloseable {
    fun offer(frame: ByteArray)
    fun finish()
    fun abort()
    override fun close() = abort()
}

internal class LiveCapturePipe(
    private val record: CaptureRecord,
    private val sdk: CaptureWearSdk,
    private val acquisitionMs: Long = 10_000,
    private val finishMs: Long = 10_000,
    private val onUnavailable: (String) -> Unit = {},
) : LiveCaptureHandle {
    private val timing = CaptureStartupTiming.forCapture(record.captureId)
    private val queue = ArrayBlockingQueue<ByteArray>(20)
    private val lock = Any()
    private val executor = Executors.newSingleThreadExecutor()
    private val ownership = CaptureChannelOwnership(sdk, acquisitionMs)
    private var finishing = false
    private var aborted = false
    private var completed = false
    private var deadline: java.util.concurrent.ScheduledFuture<*>? = null

    fun start() {
        require(record.purpose == CapturePurpose.NOTE) { "non-note-live-transfer-not-allowed" }
        executor.execute {
            try {
                timing?.mark("live-connect-start")
                val nodes = Tasks.await(sdk.phones(), acquisitionMs, TimeUnit.MILLISECONDS).nodes
                val node = nodes.singleOrNull()
                    ?: run {
                        timing?.mark("live-phone-unavailable")
                        unavailable(if (nodes.isEmpty()) "phone-unavailable" else "multiple-phone-receivers")
                        return@execute
                    }
                check(!isAborted())
                timing?.mark("live-phone-selected")
                val sessionId = record.recognitionSessionId ?: return@execute
                val start = JSONObject().apply {
                    put("protocol", CaptureProtocol.PROTOCOL); put("captureId", record.captureId)
                    put("sessionId", sessionId); put("sampleRate", record.format.sampleRate)
                    put("channels", record.format.channels); put("bitsPerSample", record.format.bitsPerSample)
                    put("startedAt", record.startedAtUtc)
                    put("purpose", record.purpose.name); putOpt("requestedLanguage", record.requestedLanguage)
                    put("speechNetworkFallback", record.speechNetworkFallback)
                }.toString().toByteArray(Charsets.UTF_8)
                sdk.send(node.id, CaptureProtocol.START, start)
                timing?.mark("live-start-message-requested")
                val channel = ownership.open(node.id, "${CaptureProtocol.LIVE}/${record.captureId}/$sessionId")
                timing?.mark("live-channel-opened")
                val output = ownership.output(channel)
                timing?.mark("live-output-ready")
                val stream = DataOutputStream(BufferedOutputStream(output))
                stream.writeInt(start.size); stream.write(start); stream.flush()
                timing?.mark("live-header-flushed")
                var sequence = 0L
                var pcmBytes = 0L
                while (!isAborted()) {
                    val bytes = queue.poll(200, TimeUnit.MILLISECONDS)
                    if (bytes == null) {
                        if (synchronized(lock) { finishing && queue.isEmpty() }) break
                        continue
                    }
                    stream.writeLong(sequence++); stream.writeInt(bytes.size); stream.write(bytes)
                    pcmBytes += bytes.size
                    stream.flush(); timing?.mark("first-pcm-flushed")
                }
                check(!isAborted())
                stream.writeLong(-1L); stream.flush()
                timing?.mark("live-input-eof-flushed")
                val stop = JSONObject().put("protocol", CaptureProtocol.PROTOCOL).put("captureId", record.captureId)
                    .put("sessionId", sessionId).put("frameCount", pcmBytes / (record.format.channels * record.format.bitsPerSample / 8))
                    .toString().toByteArray(Charsets.UTF_8)
                val stopTask = synchronized(lock) {
                    check(!aborted)
                    sdk.send(node.id, CaptureProtocol.STOP, stop)
                }
                Tasks.await(stopTask, acquisitionMs, TimeUnit.MILLISECONDS)
            } catch (_: Exception) {
                timing?.mark("live-stream-failed")
                if (!isAborted()) unavailable("live-stream-failed")
            }
            finally {
                synchronized(lock) { completed = true; deadline?.cancel(false) }
                ownership.close()
                executor.shutdown()
            }
        }
    }
    private fun isAborted() = synchronized(lock) { aborted }
    private fun unavailable(code: String) { runCatching { onUnavailable(code) } }
    override fun offer(frame: ByteArray) {
        val overflow = synchronized(lock) {
            !finishing && !aborted && !completed && frame.isNotEmpty() && frame.size <= 3_200 && !queue.offer(frame)
        }
        if (overflow) { timing?.mark("live-queue-overflow"); unavailable("live-queue-overflow"); abort() }
    }
    override fun finish() {
        synchronized(lock) {
            if (finishing || aborted || completed) return
            finishing = true
            deadline = deadlines.schedule({ unavailable("live-finish-timeout"); abort() }, finishMs, TimeUnit.MILLISECONDS)
        }
        executor.shutdown()
    }
    override fun abort() {
        synchronized(lock) {
            if (aborted || completed) return
            aborted = true; finishing = true; queue.clear(); deadline?.cancel(false)
        }
        ownership.close()
        executor.shutdownNow()
    }
    companion object {
        private val deadlines = Executors.newSingleThreadScheduledExecutor { runnable -> Thread(runnable, "capture-live-deadline").apply { isDaemon = true } }
    }
}
