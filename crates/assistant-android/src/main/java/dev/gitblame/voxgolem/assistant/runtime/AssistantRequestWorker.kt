package dev.gitblame.voxgolem.assistant.runtime

import java.util.concurrent.Executor
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage

interface AssistantRequestListener {
    fun onDelta(requestId: String, text: String)
    fun onStatus(requestId: String, status: String) {}
    fun onOutcome(requestId: String, outcomeJson: String)
    fun onRetired(requestId: String) {}
    fun onFirstText(requestId: String, milliseconds: Long) {}
}

/** Transport-only owner. Supply a background executor and promptly returning listeners. */
class AssistantRequestWorker(
    private val executor: Executor,
    private val native: ProviderNativeCalls = AssistantProviderNative,
) : AutoCloseable {
    companion object { internal const val MAX_CALLBACK_BYTES = 256 * 1024 }
    private class Request(val id: String) {
        var cancelled = false
        var cancellationRegistered = false
        var deliveredBytes = 0L
        var deliveredStatuses = 0
        var firstTextMeasured = false
        var dispatchMeasured = false
        var returned = false
        var retired = false
    }

    private val lock = Any()
    private var active: Request? = null
    private var closed = false

    /** Rust checks requestId against the request projection; IDs must not be reused. */
    fun submit(
        requestId: String,
        authPath: String,
        requestJson: String,
        listener: AssistantRequestListener,
        timing: AssistantTiming? = null,
    ): Boolean {
        val request = synchronized(lock) {
            if (closed || active != null) return false
            Request(requestId).also { active = it }
        }
        try {
            executor.execute {
                try {
                    val admitted = synchronized(lock) { active === request && !request.cancelled }
                    if (!admitted) return@execute
                    timing?.mark(Stage.WORKER_STARTED)
                    val callback = object : ProviderDeltaCallback {
                        override fun onRequestDispatched() = synchronized(lock) {
                            if (active !== request || request.returned || request.cancelled || request.dispatchMeasured) return@synchronized
                            request.dispatchMeasured = true
                            // Android elapsed-realtime receipt, not the Rust dispatch instant or network headers.
                            timing?.mark(Stage.REQUEST_DISPATCHED)
                        }
                        override fun onFirstText(milliseconds: Long) = synchronized(lock) {
                            if (active !== request || request.returned || request.cancelled || request.firstTextMeasured) return@synchronized
                            check(milliseconds in 0..86_400_000) { "Invalid provider first-text duration" }
                            request.firstTextMeasured = true
                            listener.onFirstText(request.id, milliseconds)
                        }
                        override fun onReady() = synchronized(lock) {
                            if (active !== request || request.returned) return@synchronized
                            request.cancellationRegistered = true
                            timing?.mark(Stage.NATIVE_READY)
                            if (request.cancelled) native.cancel(request.id)
                        }

                        override fun onDelta(text: String) = synchronized(lock) {
                            if (active !== request || request.returned || request.cancelled) return@synchronized
                            request.deliveredBytes += text.toByteArray(Charsets.UTF_8).size
                            check(request.deliveredBytes <= MAX_CALLBACK_BYTES) { "Native output exceeds callback budget" }
                            listener.onDelta(request.id, text)
                        }

                        override fun onStatus(status: String) = synchronized(lock) {
                            if (active !== request || request.returned || request.cancelled) return@synchronized
                            check(AssistantSearchModes.isValidStatus(status)) { "Invalid native tool status" }
                            check(++request.deliveredStatuses <= AssistantSearchModes.MAX_STATUS_EVENTS) { "Native status exceeds callback budget" }
                            listener.onStatus(request.id, status)
                        }
                    }
                    val outcome = try {
                        native.respond(request.id, authPath, requestJson, callback)
                    } catch (_: RuntimeException) {
                        """{"status":"error","code":"internal_error"}"""
                    }
                    val deliverOutcome = synchronized(lock) { request.returned = true; active === request && !request.cancelled }
                    if (deliverOutcome) listener.onOutcome(request.id, outcome)
                } finally {
                    synchronized(lock) { request.returned = true }
                    native.whenRetired(request.id) {
                        val first = synchronized(lock) {
                            if (request.retired) false else {
                                request.retired = true
                                if (active === request) active = null
                                true
                            }
                        }
                        if (first) listener.onRetired(request.id)
                    }
                }
            }
        } catch (error: RuntimeException) {
            synchronized(lock) { if (active === request) active = null }
            throw error
        }
        return true
    }

    fun cancel(): Unit = synchronized(lock) {
        active?.let { request ->
            if (!request.cancelled) {
                request.cancelled = true
                if (request.cancellationRegistered) native.cancel(request.id)
            }
        }
    }

    override fun close(): Unit = synchronized(lock) {
        closed = true
        cancel()
    }
}
