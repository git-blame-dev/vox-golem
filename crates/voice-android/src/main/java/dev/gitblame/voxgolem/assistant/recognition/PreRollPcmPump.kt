package dev.gitblame.voxgolem.assistant.recognition

import java.io.OutputStream
import java.util.ArrayDeque
import java.util.concurrent.atomic.AtomicBoolean

/** One capture worker owns buffering, ordered writes, EOF and native release. No UI-thread joins. */
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
    private val captureLock = Any()
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
                synchronized(captureLock) {
                    if (!captureStopped) runCatching { capture.stop(); captureStopped = true }
                    runCatching { capture.release(); captureReleased = true }
                }
                ended.set(true)
                closeOutput()
            }
        }
    }

    fun startStreaming() { streaming.set(true) }

    fun stopCapture() {
        stopping.set(true)
        synchronized(captureLock) {
            // Stop before releasing the lease. Failed stops remain retryable by SDK retirement.
            if (!captureStopped && !captureReleased) {
                capture.stop()
                captureStopped = true
            }
        }
    }

    override fun close() {
        cancelled.set(true)
        try { stopCapture() } finally { closeOutput() }
    }

    private fun closeOutput() {
        if (outputClosed.compareAndSet(false, true)) runCatching { output.close() }
    }
}
