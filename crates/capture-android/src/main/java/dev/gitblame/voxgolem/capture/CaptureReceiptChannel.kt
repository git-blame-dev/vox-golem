package dev.gitblame.voxgolem.capture

import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.InputStream
import java.io.OutputStream
import java.util.concurrent.TimeUnit

/** Same v1 JSON receipt as MessageClient, framed only for the private point-to-point channel. */
object CaptureReceiptChannel {
    const val CONSUMED = 1
    fun path(captureId: String): String {
        require(CaptureProtocol.isValidUuid(captureId))
        return "${CaptureProtocol.RECEIPT}/$captureId"
    }
    fun captureId(path: String): String? = path.substringAfterLast('/').takeIf {
        CaptureProtocol.isValidUuid(it) && path == CaptureReceiptChannel.path(it)
    }
    fun write(output: OutputStream, bytes: ByteArray) {
        require(bytes.isNotEmpty() && CaptureProtocol.validatePayloadSize(bytes.size, channelResult = true))
        DataOutputStream(output).apply { writeInt(bytes.size); write(bytes); flush() }
    }
    fun read(input: InputStream): ByteArray {
        val stream = DataInputStream(input)
        val size = stream.readInt()
        require(size > 0 && CaptureProtocol.validatePayloadSize(size, channelResult = true))
        val bytes = ByteArray(size)
        stream.readFully(bytes)
        require(stream.read() == -1) { "receipt-trailing-data" }
        return bytes
    }
}

/** Admission stays charged through failed retirement and through SDK acquisitions completing late. */
class CaptureChannelRetirementOwner(private val capacity: Int = 10) {
    init { require(capacity > 0) }
    private val lock = java.lang.Object()
    private val owned = mutableSetOf<CaptureChannelLifetime>()
    val size: Int get() = synchronized(lock) { owned.size }
    fun reserve(timeoutMs: Long = 15_000, onReleased: () -> Unit = {}): CaptureChannelLifetime? = synchronized(lock) {
        if (owned.size >= capacity) null
        else CaptureChannelLifetime(this, timeoutMs, onReleased).also { owned += it; it.startDeadline() }
    }
    /** Backpressure for a native handle already delivered by GMS; never silently discard a failed close. */
    fun awaitReservation(timeoutMs: Long = 15_000, onReleased: () -> Unit = {}): CaptureChannelLifetime = synchronized(lock) {
        var interrupted = false
        while (owned.size >= capacity) try { lock.wait() } catch (_: InterruptedException) { interrupted = true }
        reserve(timeoutMs, onReleased)!!.also { if (interrupted) Thread.currentThread().interrupt() }
    }
    internal fun released(lifetime: CaptureChannelLifetime) { synchronized(lock) { owned.remove(lifetime); lock.notifyAll() } }
    /** One fresh bounded recovery round, only for revoked resources; never preempts a live capture. */
    fun retryFailedRetirements() = synchronized(lock) { owned.toList() }.forEach { it.retryRetirement() }
}

/** Revocation is immediate. Native retirement requires close success or a matching SDK close callback. */
class CaptureChannelLifetime internal constructor(
    private val owner: CaptureChannelRetirementOwner,
    timeoutMs: Long,
    private val onReleased: () -> Unit,
) : AutoCloseable {
    private val lock = Any()
    private val started = System.nanoTime()
    private val timeoutNanos = TimeUnit.MILLISECONDS.toNanos(timeoutMs)
    private var revoked = false
    private var released = false
    private var nativePending = true
    private var nativeConfirmed = false
    private var nativeClose: (((Boolean) -> Unit) -> Unit)? = null
    private var nativeAttempts = 0
    private var nativeInFlight = false
    private var nativeRetryRequested = false
    private var attemptId = 0L
    private var pendingStreams = 0
    private class Stream(val handle: AutoCloseable) { var attempts = 0; var inFlight = false; var retryRequested = false }
    private val streams = java.util.IdentityHashMap<AutoCloseable, Stream>()
    private var deadline: java.util.concurrent.ScheduledFuture<*>? = null
    init { require(timeoutMs > 0) }
    internal fun startDeadline() { deadline = timers.schedule({ close() }, timeoutNanos, TimeUnit.NANOSECONDS) }
    private fun expired(): Boolean = System.nanoTime() - started >= timeoutNanos
    val isRetired: Boolean get() = synchronized(lock) { revoked || expired() }
    val nativeRetired: Boolean get() = synchronized(lock) { nativeConfirmed }
    internal val retirementExhausted: Boolean get() = synchronized(lock) {
        revoked && ((!nativeConfirmed && !nativePending && !nativeInFlight && nativeAttempts >= MAX_ATTEMPTS) ||
            streams.values.any { !it.inFlight && it.attempts >= MAX_ATTEMPTS })
    }

    /** Must be completed by the actual open Task, even after the awaiting caller has timed out. */
    fun acquiredNative(close: ((Boolean) -> Unit) -> Unit): Boolean {
        val accepted = synchronized(lock) {
            check(nativePending && nativeClose == null)
            nativePending = false; nativeClose = close
            !revoked && !expired()
        }
        if (!accepted) this.close()
        return accepted
    }
    fun nativeAcquisitionFailed() {
        synchronized(lock) { nativePending = false; nativeConfirmed = true }
        close()
    }
    fun beginStreamAcquisition() = synchronized(lock) { check(!released); pendingStreams++ }
    fun streamAcquisitionCompleted(acquired: AutoCloseable?): Boolean {
        val accepted = synchronized(lock) {
            check(pendingStreams > 0); pendingStreams--
            if (acquired != null) streams[acquired] = Stream(acquired)
            !revoked && !expired()
        }
        if (!accepted) close()
        maybeRelease()
        return accepted
    }
    /** For a stream already in hand, not an unregistered asynchronous SDK acquisition. */
    fun own(acquired: AutoCloseable): Boolean {
        beginStreamAcquisition()
        return streamAcquisitionCompleted(acquired)
    }
    fun ifActive(action: () -> Unit): Boolean = synchronized(lock) {
        if (revoked || expired()) false else { action(); true }
    }
    override fun close() {
        synchronized(lock) { revoked = true; deadline?.cancel(false); deadline = null }
        retireNative()
        synchronized(lock) { streams.values.toList() }.forEach(::retireStream)
        maybeRelease()
    }
    fun confirmNativeRetired() {
        synchronized(lock) { nativeConfirmed = true; nativeInFlight = false }
        close()
    }
    internal fun retryRetirement() {
        synchronized(lock) {
            if (!revoked || released) return
            if (!nativeInFlight) nativeAttempts = 0 else nativeRetryRequested = true
            streams.values.forEach { if (it.inFlight) it.retryRequested = true else it.attempts = 0 }
        }
        close()
    }
    private fun retireNative() {
        val request = synchronized(lock) {
            if (nativePending || nativeConfirmed || nativeInFlight || nativeAttempts >= MAX_ATTEMPTS) return
            val closer = nativeClose ?: return
            nativeAttempts++; nativeInFlight = true; attemptId++
            closer to attemptId
        }
        // No SDK call, stream close, or task wait ever runs on the deadline scheduler.
        try { nativeWork.execute {
            val timeout = timers.schedule({ nativeCompleted(request.second, false) }, CLOSE_TIMEOUT_MS, TimeUnit.MILLISECONDS)
            try { request.first { success -> timeout.cancel(false); nativeCompleted(request.second, success) } }
            catch (_: Exception) { timeout.cancel(false); nativeCompleted(request.second, false) }
        } } catch (_: java.util.concurrent.RejectedExecutionException) { nativeCompleted(request.second, false) }
    }
    private fun nativeCompleted(id: Long, success: Boolean) {
        val retry = synchronized(lock) {
            if (success) { nativeConfirmed = true; nativeInFlight = false; false }
            else if (id != attemptId || nativeConfirmed) false
            else {
                nativeInFlight = false
                if (nativeRetryRequested) { nativeRetryRequested = false; nativeAttempts = 0 }
                nativeAttempts < MAX_ATTEMPTS
            }
        }
        if (retry) timers.schedule({ retireNative() }, RETRY_MS, TimeUnit.MILLISECONDS)
        maybeRelease()
    }
    private fun retireStream(stream: Stream) {
        synchronized(lock) {
            if (streams[stream.handle] !== stream || stream.inFlight || stream.attempts >= MAX_ATTEMPTS) return
            stream.attempts++; stream.inFlight = true
        }
        try { streamWork.execute {
            val success = runCatching { stream.handle.close() }.isSuccess
            streamCompleted(stream, success)
        } } catch (_: java.util.concurrent.RejectedExecutionException) { streamCompleted(stream, false) }
    }
    private fun streamCompleted(stream: Stream, success: Boolean) {
        val retry = synchronized(lock) {
            if (success) { streams.remove(stream.handle); false }
            else {
                stream.inFlight = false
                if (stream.retryRequested) { stream.retryRequested = false; stream.attempts = 0 }
                stream.attempts < MAX_ATTEMPTS
            }
        }
        if (retry) timers.schedule({ retireStream(stream) }, RETRY_MS, TimeUnit.MILLISECONDS)
        maybeRelease()
    }
    private fun maybeRelease() {
        val completed = synchronized(lock) {
            if (released || !revoked || !nativeConfirmed || nativePending || pendingStreams != 0 || streams.isNotEmpty()) false
            else { released = true; true }
        }
        if (completed) { owner.released(this); runCatching(onReleased) }
    }
    companion object {
        private const val MAX_ATTEMPTS = 3
        private const val CLOSE_TIMEOUT_MS = 1_000L
        private const val RETRY_MS = 100L
        private val timers = java.util.concurrent.ScheduledThreadPoolExecutor(1) { task ->
            Thread(task, "capture-channel-deadline").apply { isDaemon = true }
        }.apply { removeOnCancelPolicy = true }
        private fun workers(count: Int, name: String) = java.util.concurrent.ThreadPoolExecutor(count, count, 0, TimeUnit.MILLISECONDS,
            java.util.concurrent.ArrayBlockingQueue<Runnable>(256), { task -> Thread(task, name).apply { isDaemon = true } })
        private val nativeWork = workers(1, "capture-native-retirement")
        private val streamWork = workers(2, "capture-stream-retirement")
    }
}
