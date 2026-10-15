package dev.gitblame.voxgolem.assistant.runtime

import android.util.Log
import org.json.JSONObject
import java.io.DataInputStream
import java.io.DataOutputStream
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.ScheduledThreadPoolExecutor
import java.util.concurrent.ScheduledFuture
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

private data class RelayPending(val bytes: ByteArray, val after: (() -> Unit)?)

/** One process admission, including unconfirmed retirement. No per-message threads or global ID map. */
internal class PhoneRelayRuntime(
    private val platform: PhoneRelayPlatform,
    private val durationMs: Long = 200_000,
    private val cleanupMs: Long = 15_000,
    private val provider: () -> ProviderNativeCalls,
) : PhoneRelayClient, AutoCloseable {
    private val io = ThreadPoolExecutor(4, 4, 30, TimeUnit.SECONDS, ArrayBlockingQueue(4),
        { runnable -> Thread(runnable, "assistant-relay-io").apply {
            isDaemon = true
            uncaughtExceptionHandler = Thread.UncaughtExceptionHandler { _, _ -> Log.w("PhoneRelay", "relay_worker_failed") }
        } }, ThreadPoolExecutor.AbortPolicy()).apply { allowCoreThreadTimeOut(true) }
    private val deadlines = ScheduledThreadPoolExecutor(1) { runnable ->
        Thread(runnable, "assistant-relay-deadline").apply { isDaemon = true }
    }.apply { removeOnCancelPolicy = true }
    private val lock = Any()
    private var active: Session? = null
    private var closed = false

    override fun respond(requestId: String, requestJson: String, callback: ProviderDeltaCallback): String {
        if (!platform.watch) return error("relay_unavailable")
        val session = synchronized(lock) {
            if (closed || active != null) return error("relay_busy")
            Session(requestId, true, callback).also { active = it }
        }
        // Register the cancellation target before exposing readiness, even before peer discovery.
        try {
            session.arm(durationMs)
            callback.onReady()
            io.execute { session.connect(requestJson) }
            session.returned.await()
        } catch (_: Exception) {
            session.fail()
        }
        return session.result()
    }

    override fun cancel(requestId: String) { current()?.takeIf { it.watch && it.id == requestId }?.cancel() }

    override fun whenRetired(requestId: String, retired: () -> Unit) {
        val session = current()?.takeIf { it.watch && it.id == requestId }
        if (session == null) retired() else session.whenRetired(retired)
    }

    /** Callback admission is O(1); discovery, streams and parsing run exclusively on bounded I/O workers. */
    fun accept(connection: PhoneRelayConnection) {
        if (platform.watch || !PhoneRelayWire.validPath(connection.path)) { connection.close(); return }
        val session = synchronized(lock) {
            if (active?.connection?.identity == connection.identity) return // duplicate SDK delivery, not a new admission
            if (closed || active != null) null else Session("", false, null).also { it.connection = connection; active = it }
        }
        if (session == null) { connection.close(); return }
        session.arm(durationMs)
        try { io.execute { session.receive() } } catch (_: RuntimeException) { session.fail() }
    }

    private fun current() = synchronized(lock) { active }
    private fun release(session: Session) { synchronized(lock) { if (active === session) active = null } }
    override fun close() {
        synchronized(lock) { closed = true }
        current()?.fail()
        io.shutdownNow()
        deadlines.shutdownNow()
    }

    private inner class Session(val id: String, val watch: Boolean, private var callback: ProviderDeltaCallback?) {
        private val state = Object()
        val returned = CountDownLatch(1)
        @Volatile var connection: PhoneRelayConnection? = null
        private var route: PhoneRelayRoute? = null
        private var timer: ScheduledFuture<*>? = null
        private var worker: AssistantRequestWorker? = null
        private var execution: AutoCloseable? = null
        private var executionUnavailable = false
        private var wireId = UUID.randomUUID().toString()
        private val localId = "phone-relay-local-${UUID.randomUUID()}"
        private var cancelled = false
        private var contacted = false
        private var admitted = false
        private var retired = false
        private var stopped = false
        private var outcomeSent = false
        private var outcome: String? = null
        private var retirement: (() -> Unit)? = null
        private var nextSeq = 1L
        private var controlSeq = 1L
        private var cancelSent = false
        private var ackReceived = false
        private var cancellationQueued = false
        private val outbox = ArrayDeque<RelayPending>()
        private var queuedBytes = 0

        fun arm(milliseconds: Long) = synchronized(state) {
            if (stopped) return@synchronized
            timer?.cancel(false)
            timer = deadlines.schedule({ fail() }, milliseconds, TimeUnit.MILLISECONDS)
        }

        fun result(): String = synchronized(state) { outcome ?: if (cancelled) CANCELLED else error("relay_transport") }

        fun whenRetired(action: () -> Unit) {
            val now = synchronized(state) {
                if (retired && stopped) true else { check(retirement == null) { "Relay retirement already observed" }; retirement = action; false }
            }
            if (now) action()
        }

        fun connect(requestJson: String) {
            try {
                // Validate before discovery, but never use the wire ID as a local cancellation key.
                PhoneRelayWire.prepareRequest(id, requestJson, localId)
                val selected = platform.route() ?: throw IllegalStateException()
                synchronized(state) { route = selected; if (cancelled || stopped) { finish(); return } }
                val path = PhoneRelayWire.PREFIX + UUID.randomUUID()
                val opened = platform.open(selected, path)
                synchronized(state) {
                    connection = opened
                    check(opened.nodeId == selected.nodeId && opened.path == path)
                    if (cancelled || stopped) { finish(); return }
                }
                val input = DataInputStream(opened.input())
                val output = DataOutputStream(opened.output())
                startWriter(output)
                synchronized(state) {
                    check(selected.stillApproved())
                    if (cancelled || stopped) { finish(); return }
                    contacted = true // From here, transport loss cannot prove the phone did not admit work.
                    send(JSONObject().put("type", "request").put("seq", 0).put("requestId", wireId)
                        .put("requestJson", JSONObject(requestJson).put("requestId", wireId).toString()))
                }
                val events = PhoneRelayEvents(wireId)
                while (true) {
                    val event = events.accept(PhoneRelayWire.read(input))
                    val deliver = synchronized(state) {
                        check(selected.stillApproved())
                        val target = callback.takeUnless { cancelled }
                        when (event.getString("type")) {
                            "dispatched" -> target?.let { { it.onRequestDispatched() } }
                            "status" -> target?.let { { it.onStatus(event.getString("status")) } }
                            "first_text" -> target?.let { { it.onFirstText(event.getLong("milliseconds")) } }
                            "delta" -> target?.let { { it.onDelta(event.getString("text")) } }
                            "outcome" -> {
                                if (!cancelled) outcome = event.getString("outcomeJson")
                                callback = null
                                returned.countDown()
                                arm(cleanupMs)
                                null
                            }
                            "retired" -> {
                                retired = true // Provider's public retirement contract, not a remote TPU hardware ACK.
                                sendControl("retired_ack") { finish() }
                                arm(cleanupMs)
                                return
                            }
                            else -> null
                        }
                    }
                    deliver?.invoke()
                }
            } catch (_: Exception) { fail() }
        }

        fun receive() {
            try {
                val channel = requireNotNull(connection)
                val selected = platform.route(channel.nodeId) ?: throw IllegalStateException()
                check(selected.nodeId == channel.nodeId)
                route = selected
                val input = DataInputStream(channel.input())
                val output = DataOutputStream(channel.output())
                startWriter(output)
                val header = PhoneRelayWire.read(input)
                check(header.getString("type") == "request" && header.get("seq") == 0)
                val remoteId = header.get("requestId") as? String ?: throw IllegalStateException()
                val requestJson = header.get("requestJson") as? String ?: throw IllegalStateException()
                val request = PhoneRelayWire.prepareRequest(remoteId, requestJson, localId)
                synchronized(state) { wireId = remoteId }
                // Keep the ordered control reader live while foreground promotion awaits the main
                // service thread. Stop received during admission must not start late generation.
                io.execute { admit(selected, request) }
                var expected = 1L
                var cancelSeen = false
                while (true) {
                    val control = PhoneRelayWire.read(input)
                    synchronized(state) {
                        check(selected.stillApproved() && control.get("requestId") == wireId)
                        val seq = control.get("seq")
                        check((seq is Int || seq is Long) && (seq as Number).toLong() == expected++)
                        when (control.getString("type")) {
                            "cancel" -> { check(!cancelSeen); cancelSeen = true; cancel() }
                            "retired_ack" -> { check(retired && !ackReceived); ackReceived = true; finish(); return }
                            else -> throw IllegalStateException()
                        }
                    }
                }
            } catch (_: Exception) { fail() }
        }

        private fun admit(selected: PhoneRelayRoute, request: String) {
            try {
                synchronized(state) {
                    check(!stopped && selected.stillApproved())
                    if (cancelled) { rejectAdmission(CANCELLED); return }
                }
                // Neither confirmation nor its failure callback may wait while holding session state.
                val protection = try {
                    platform.acquireExecution(durationMs + cleanupMs, ::executionLost)
                } catch (_: Exception) {
                    rejectAdmission(error("unavailable"))
                    return
                }
                var protectionOwned = false
                try {
                    synchronized(state) {
                        check(!stopped && selected.stillApproved())
                        execution = protection
                        protectionOwned = true
                        if (executionUnavailable) { rejectAdmission(error("unavailable")); return }
                        if (cancelled) { rejectAdmission(CANCELLED); return }
                        val nativeWorker = AssistantRequestWorker(io, provider())
                        worker = nativeWorker
                        admitted = nativeWorker.submit(localId, "", request, object : AssistantRequestListener {
                            override fun onRequestDispatched(requestId: String) { event("dispatched") }
                            override fun onDelta(requestId: String, text: String) { event("delta", "text", text) }
                            override fun onStatus(requestId: String, status: String) { event("status", "status", status) }
                            override fun onFirstText(requestId: String, milliseconds: Long) { event("first_text", "milliseconds", milliseconds) }
                            override fun onOutcome(requestId: String, outcomeJson: String) = synchronized(state) {
                                emitOutcome(outcomeJson)
                                Unit
                            }
                            override fun onRetired(requestId: String) = synchronized(state) {
                                retired = true
                                if (stopped) { finish(); return@synchronized }
                                if (!outcomeSent && !emitOutcome(CANCELLED)) return@synchronized
                                event("retired")
                                // Keep process importance through the brief terminal flush/ACK too.
                                arm(cleanupMs)
                            }
                        })
                        check(admitted)
                    }
                } finally {
                    if (!protectionOwned) protection.close()
                }
            } catch (_: Exception) { fail() }
        }

        private fun executionLost() {
            val running = synchronized(state) {
                if (retired || stopped) return
                executionUnavailable = true
                admitted
            }
            if (running) fail()
        }

        /** Authenticated, parsed admission rejection is proof that no provider ever owned this request. */
        private fun rejectAdmission(result: String) = synchronized(state) {
            if (stopped || retired) return@synchronized
            check(!admitted)
            retired = true
            if (!emitOutcome(result)) return@synchronized
            event("retired", "execution", "not_admitted")
            arm(cleanupMs)
        }

        private fun emitOutcome(raw: String): Boolean = synchronized(state) {
            if (stopped) return@synchronized false
            try {
                check(!outcomeSent && route?.stillApproved?.invoke() == true)
                val bytes = PhoneRelayWire.encodeOutcome(wireId, nextSeq, raw)
                enqueue(bytes)
                nextSeq++
                outcomeSent = true
                true
            } catch (_: Exception) { fail(); false }
        }

        private fun event(type: String, key: String? = null, value: Any? = null) = synchronized(state) {
            if (stopped) return@synchronized
            if (route?.stillApproved?.invoke() != true) { fail(); return@synchronized }
            val json = JSONObject().put("type", type).put("requestId", wireId).put("seq", nextSeq++)
            if (key != null) json.put(key, value)
            send(json)
        }

        private fun sendControl(type: String, after: (() -> Unit)? = null) = synchronized(state) {
            send(JSONObject().put("type", type).put("requestId", wireId).put("seq", controlSeq++), after)
        }

        private fun send(json: JSONObject, after: (() -> Unit)? = null) {
            try {
                val bytes = PhoneRelayWire.encode(json)
                enqueue(bytes, after)
            } catch (_: Exception) { fail() }
        }

        private fun enqueue(bytes: ByteArray, after: (() -> Unit)? = null) = synchronized(state) {
            check(!stopped && outbox.size < MAX_OUTBOX_RECORDS && queuedBytes + bytes.size <= MAX_OUTBOX_BYTES)
            outbox.addLast(RelayPending(bytes, after))
            queuedBytes += bytes.size
            state.notifyAll()
        }

        private fun startWriter(output: DataOutputStream) {
            io.execute {
                try {
                    while (true) {
                        val pending = synchronized(state) {
                            while (outbox.isEmpty() && !stopped) state.wait()
                            if (stopped) return@execute
                            outbox.first() to route
                        }
                        // Re-fence each queued frame immediately before I/O, without holding state
                        // or enrollment locks. Bytes already in flight cannot be recalled.
                        check(pending.second?.stillApproved?.invoke() == true)
                        PhoneRelayWire.write(output, pending.first.bytes)
                        synchronized(state) {
                            if (stopped) return@execute
                            outbox.removeFirst()
                            queuedBytes -= pending.first.bytes.size
                        }
                        pending.first.after?.invoke()
                    }
                } catch (_: Exception) { fail() }
            }
        }

        fun cancel() {
            val nativeWorker = synchronized(state) {
                if (cancelled) return
                cancelled = true
                callback = null
                outcome = CANCELLED
                returned.countDown()
                if (watch && contacted && !stopped && !cancelSent) { cancelSent = true; sendControl("cancel") }
                arm(cleanupMs)
                worker
            }
            nativeWorker?.let(::cancelWork)
        }

        fun fail() {
            val nativeWorker = synchronized(state) {
                if (stopped) { if (retired) finish(); return }
                Log.w("PhoneRelay", "relay_failed_or_retirement_unconfirmed")
                if (outcome == null && !cancelled) outcome = error("relay_transport")
                callback = null
                returned.countDown()
                worker
            }
            nativeWorker?.let(::cancelWork)
            finish()
        }

        // Never acquire the worker's callback lock while holding the session lock: callbacks offer
        // into this session while the worker owns that lock. At most one cancellation task is admitted.
        private fun cancelWork(nativeWorker: AssistantRequestWorker) {
            synchronized(state) {
                if (cancellationQueued) return
                cancellationQueued = true
            }
            try { io.execute { nativeWorker.cancel() } } catch (_: RuntimeException) {
                // Executor shutdown leaves ownership quarantined; never infer retirement.
            }
        }

        /** Close transport at a finite deadline. Keep the one admission quarantined if retirement is unknown. */
        private fun finish() {
            val action = synchronized(state) {
                stopped = true
                callback = null
                timer?.cancel(false)
                timer = null
                outbox.clear()
                queuedBytes = 0
                state.notifyAll()
                connection?.close()
                if ((!watch && !admitted) || (watch && !contacted)) retired = true
                if (retired) releaseExecution()
                if (retired) retirement.also { retirement = null } else null
            }
            if (synchronized(state) { retired }) release(this)
            if (action != null) {
                try { io.execute { try { action() } catch (_: RuntimeException) { Log.w("PhoneRelay", "relay_retirement_listener_failed") } } } catch (_: RuntimeException) {
                    // Runtime shutdown only: ownership is already retired, and the caller may
                    // observe that directly via whenRetired. No unknown retirement is promoted.
                }
            }
        }

        private fun releaseExecution() {
            val lease = execution ?: return
            execution = null
            try { lease.close() } catch (_: RuntimeException) { Log.w("PhoneRelay", "relay_wake_release_failed") }
        }
    }

    companion object {
        const val MAX_OUTBOX_RECORDS = 64
        const val MAX_OUTBOX_BYTES = 4 * 1024 * 1024
        private const val CANCELLED = """{"status":"cancelled"}"""
        private fun error(code: String) = """{"status":"error","code":"$code","message":"Phone relay unavailable or retirement unconfirmed"}"""
    }
}
