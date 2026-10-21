package dev.gitblame.voxgolem.assistant.recognition

import java.io.OutputStream
import java.util.ArrayDeque
import java.util.concurrent.atomic.AtomicBoolean

/** Capture worker owns PCM/EOF; one coalesced control drive serializes native stop and release. */
internal class PreRollPcmPump(
    private val capture: Capture,
    private val output: OutputStream,
    private val frameBytes: Int,
    private val maxFrames: Int,
) : AutoCloseable {
    interface Capture {
        fun read(buffer: ByteArray): Int
        fun stop()
        fun release()
    }

    private val streaming = AtomicBoolean(false)
    private val stopping = AtomicBoolean(false)
    private val cancelled = AtomicBoolean(false)
    private val ended = AtomicBoolean(false)
    private val outputClosed = AtomicBoolean(false)
    private val control = Any()
    private var driving = false
    private var driveRequested = false
    private var failure = false
    private var completion: ((Boolean) -> Unit)? = null
    private var captureStopped = false
    private var captureReleased = false
    val isRecording: Boolean get() = !stopping.get() && !ended.get()

    init {
        require(frameBytes > 0 && maxFrames > 0)
        Thread(::run, "voxgolem-preroll-audio").apply { isDaemon = true; start() }
    }

    private fun run() {
        val ring = ArrayDeque<ByteArray>(maxFrames)
        val buffer = ByteArray(frameBytes)
        fun drain() {
            while (ring.isNotEmpty() && !cancelled.get()) output.write(ring.removeFirst())
        }
        try {
            while (!stopping.get()) {
                val read = capture.read(buffer)
                if (read <= 0) break
                // A read already in progress when graceful stop arrives still belongs to this turn.
                val frame = buffer.copyOf(read)
                if (streaming.get()) {
                    drain()
                    if (!cancelled.get()) output.write(frame)
                } else {
                    if (ring.size == maxFrames) ring.removeFirst()
                    ring.addLast(frame)
                }
            }
        } catch (_: Exception) {
            // Pipe/service failure retires this capture; caller's recognizer owns the user outcome.
        } finally {
            try {
                if (streaming.get() && !cancelled.get()) drain()
            } catch (_: Exception) {
                // A retired consumer may have already closed the read end.
            } finally {
                stopping.set(true)
                ended.set(true)
                closeOutput()
                drive()
            }
        }
    }

    fun startStreaming() { streaming.set(true) }

    fun stopCapture() {
        stopping.set(true)
        drive()
    }

    override fun close() {
        cancelled.set(true)
        closeOutput()
        stopCapture()
    }

    /** One explicit retry after failure; completion never means merely requested. */
    fun retire(completed: (Boolean) -> Unit) {
        val released = synchronized(control) {
            if (captureReleased) true else {
                completion = completed
                failure = false
                false
            }
        }
        if (released) completed(true) else close()
    }

    private fun drive() {
        val launch = synchronized(control) {
            driveRequested = true
            if (driving || failure || captureReleased ||
                (!stopping.get() || (captureStopped && !ended.get()))) false
            else { driving = true; true }
        }
        if (!launch) return
        Thread({
            while (true) {
                val work = synchronized(control) {
                    driveRequested = false
                    when {
                        failure || captureReleased -> 0
                        stopping.get() && !captureStopped -> 1
                        ended.get() && captureStopped -> 2
                        else -> 0
                    }
                }
                if (work != 0) {
                    val success = runCatching {
                        if (work == 1) capture.stop() else capture.release()
                    }.isSuccess
                    val callback = synchronized(control) {
                        if (success) {
                            if (work == 1) captureStopped = true else captureReleased = true
                        } else failure = true
                        if (!success || captureReleased) completion.also { completion = null } else null
                    }
                    runCatching { callback?.invoke(success) }
                }
                val again = synchronized(control) {
                    if (!failure && !captureReleased && (driveRequested ||
                            (stopping.get() && !captureStopped) || (ended.get() && captureStopped))) true
                    else { driving = false; false }
                }
                if (!again) break
            }
        }, "voxgolem-preroll-retirement").apply { isDaemon = true; start() }
    }

    private fun closeOutput() {
        if (outputClosed.compareAndSet(false, true)) runCatching { output.close() }
    }
}
