package dev.gitblame.voxgolem.assistant.recognition

import java.io.OutputStream
import java.util.ArrayDeque
import java.util.concurrent.atomic.AtomicBoolean

/** One PCM writer; one coalesced control drive owns native teardown, EOF and writer retirement. */
internal class PreRollPcmPump(
    private val capture: Capture,
    private val output: OutputStream,
    private val frameBytes: Int,
    private val maxFrames: Int,
    private val closeInput: () -> Unit = {},
    private val startWriter: (Thread) -> Unit = { it.start() },
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
    private var outputClosed = false
    private var inputClosed = false
    private var writerJoined = false
    private val control = Any()
    private var driving = false
    private var failure = false
    private var completion: ((Boolean) -> Unit)? = null
    private var captureStopped = false
    private var captureReleased = false
    private var startupAttempted = false
    private var acquisitionSettled = false
    @Volatile private var writerAccepted = false
    private val writer = Thread(::run, "voxgolem-preroll-audio").apply { isDaemon = true }
    val isRecording: Boolean get() = writerAccepted && !stopping.get() && !ended.get()

    init {
        require(frameBytes > 0 && maxFrames > 0)
    }

    /** Resource slots must be final before this call; a failed start never creates another writer. */
    fun startCapture() {
        synchronized(control) {
            check(!startupAttempted && !acquisitionSettled && !stopping.get())
            startupAttempted = true
        }
        var accepted = false
        try { startWriter(writer); accepted = true }
        finally {
            if (!accepted) {
                cancelled.set(true); stopping.set(true)
                // An injected/platform start failure may occur after the real Thread started.
                if (writer.state == Thread.State.NEW) ended.set(true)
            }
            synchronized(control) { writerAccepted = accepted; acquisitionSettled = true }
            drive()
        }
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
        synchronized(control) { settleUnstarted() }
        stopCapture()
    }

    private fun settleUnstarted() {
        if (!startupAttempted) { acquisitionSettled = true; ended.set(true) }
    }

    /** One explicit retry after failure; completion never means merely requested. */
    fun retire(completed: (Boolean) -> Unit) {
        cancelled.set(true)
        stopping.set(true)
        val released = synchronized(control) {
            settleUnstarted()
            if (fullyRetired()) true else {
                completion = completed
                failure = false
                false
            }
        }
        if (released) completed(true) else drive()
    }

    private fun fullyRetired() = captureReleased && writerJoined && outputClosed && (!cancelled.get() || inputClosed)

    /** Called under control. All native work, descriptor closes and joins run on the control worker. */
    private fun nextWork(): Int = when {
        failure || !acquisitionSettled || !stopping.get() -> 0
        !captureStopped -> 1
        cancelled.get() && !inputClosed -> 3
        (cancelled.get() || ended.get()) && !outputClosed -> 4
        ended.get() && !writerJoined -> 5
        writerJoined && !captureReleased -> 2
        else -> 0
    }

    private fun drive() {
        val launch = synchronized(control) {
            if (driving || nextWork() == 0) false
            else { driving = true; true }
        }
        if (!launch) return
        try { Thread({
            while (true) {
                val work = synchronized(control) {
                    nextWork()
                }
                if (work != 0) {
                    val success = runCatching {
                        when (work) {
                            1 -> capture.stop()
                            2 -> capture.release()
                            3 -> closeInput()
                            4 -> output.close()
                            5 -> writer.join()
                        }
                    }.isSuccess
                    val callback = synchronized(control) {
                        if (success) {
                            when (work) {
                                1 -> captureStopped = true
                                2 -> captureReleased = true
                                3 -> inputClosed = true
                                4 -> outputClosed = true
                                5 -> writerJoined = true
                            }
                        } else failure = true
                        if (!success || fullyRetired()) completion.also { completion = null } else null
                    }
                    runCatching { callback?.invoke(success) }
                }
                val again = synchronized(control) {
                    if (nextWork() != 0) true
                    else { driving = false; false }
                }
                if (!again) break
            }
        }, "voxgolem-preroll-retirement").apply { isDaemon = true; start() } } catch (_: Throwable) {
            val callback = synchronized(control) {
                driving = false; failure = true
                completion.also { completion = null }
            }
            runCatching { callback?.invoke(false) }
        }
    }
}
