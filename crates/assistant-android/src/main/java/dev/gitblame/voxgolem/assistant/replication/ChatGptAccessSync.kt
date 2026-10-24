package dev.gitblame.voxgolem.assistant.replication

import android.content.Context
import android.content.pm.PackageManager
import android.os.Handler
import android.os.Looper
import android.util.Log
import com.google.android.gms.wearable.MessageEvent
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.assistant.runtime.AssistantProviderNative
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import dev.gitblame.voxgolem.assistant.runtime.ChatGptAuth
import org.json.JSONObject
import java.io.File
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Executor
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicReference

/** Transient package/signature-isolated GMS messages, separate from durable history replication. */
object ChatGptAccessSync {
    @Volatile private var owner: Owner? = null
    @Synchronized fun start(context: Context) {
        if (!dev.gitblame.voxgolem.voice.backgroundServicesAdmitted(context)) return
        if (owner != null) return
        val app = context.applicationContext
        val setup = (app as? AssistantPeerSetupOwner)?.assistantPeerSetup ?: return
        owner = Owner(app, setup).also { it.changed() }
    }
    fun changed(context: Context) { start(context); owner?.changed() }
    fun changed() { owner?.changed() }
    /** Reachability is a retry opportunity, not evidence of changed credentials. */
    fun opportunity(context: Context) { start(context); owner?.opportunity() }
    /** Fail closed during startup/approval mutations without DB or file IO on admission. */
    internal fun managedAllowed(context: Context): Boolean =
        if (!context.packageManager.hasSystemFeature(PackageManager.FEATURE_WATCH)) true
        else owner?.watchApproved() == true
    fun onMessage(context: Context, message: MessageEvent) {
        if (message.path !in setOf(ChatGptAccessProtocol.PREFIX + "hint", ChatGptAccessProtocol.PREFIX + "request", ChatGptAccessProtocol.PREFIX + "state", ChatGptAccessProtocol.PREFIX + "ack") ||
            message.data.size > ChatGptAccessProtocol.MAX_BYTES) return
        start(context)
        val data = message.data.copyOf()
        val source = message.sourceNodeId
        val kind = message.path.removePrefix(ChatGptAccessProtocol.PREFIX)
        owner?.submit { receive(source, kind, data) }
    }

    internal class Owner(val app: Context, val setup: AssistantPeerSetup,
        private val export: (String) -> String = { path -> AssistantProviderNative.exportChatGptAccess(path) },
        private val message: ((AssistantVerifiedRoute, String, ByteArray) -> Unit)? = null,
        private val executor: Executor = ThreadPoolExecutor(1, 1, 30, TimeUnit.SECONDS, ArrayBlockingQueue(8),
            { work -> Thread(work, "chatgpt-peer-access") }, ThreadPoolExecutor.AbortPolicy()).apply { allowCoreThreadTimeOut(true) },
        private val schedule: (Runnable, Long) -> (() -> Unit) = { task, delay ->
            val main = Handler(Looper.getMainLooper())
            main.postDelayed(task, delay)
            val remove: () -> Unit = { main.removeCallbacks(task) }
            remove
        },
        private val now: () -> Long = System::currentTimeMillis,
    ) {
        companion object {
            const val EXPIRY_MARGIN = 60_000L
            const val MAX_ATTEMPTS = 5
            private val RETRY_DELAYS = longArrayOf(2_000, 5_000, 10_000, 20_000)
        }
        private class Exchange(val association: String, val nonce: String = UUID.randomUUID().toString(), var attempts: Int = 0) {
            var packet: JSONObject? = null // One bounded transient access-only snapshot, never a DataItem.
            var receipt: JSONObject? = null
            var unavailableObserved = false
            var reconnectPending = false
        }
        private val queued = AtomicBoolean()
        private val changeRequested = AtomicBoolean()
        private val opportunityQueued = AtomicBoolean()
        private val opportunityRequested = AtomicBoolean()
        private val file = ChatGptAuth.managedFile(app)
        private val generationFile = File(app.noBackupFilesDir, "chatgpt-peer-generation.json")
        private var exchange: Exchange? = null
        private var boundAssociation: String? = null
        private var lastHint: String? = null
        private var persistedReceipt: JSONObject? = null
        private var timer: (() -> Unit)? = null
        private var timerVersion = 0L
        private val timerReady = AtomicReference<Runnable?>()
        @Volatile private var validatedApproval: Long? = null
        fun watchApproved() = EnrolledPeerRegistry.approvalStable() && validatedApproval == EnrolledPeerRegistry.approvalVersion()
        private val failed = AtomicInteger()
        private val dropped = AtomicInteger()
        private var operationWait: BoundedGmsWait? = null
        private var operationRoute: AssistantVerifiedRoute? = null
        private var routeSelected = false
        private fun selectedRoute(expectedNode: String? = null): AssistantVerifiedRoute? {
            val wait = requireNotNull(operationWait)
            wait.remainingMillis()
            if (!routeSelected) {
                operationRoute = setup.selectedRoute(null, wait)
                routeSelected = true
                exchange?.let { current ->
                    if (operationRoute == null) current.unavailableObserved = true
                    else if (current.unavailableObserved) {
                        current.unavailableObserved = false
                        current.reconnectPending = true
                    }
                }
            }
            return operationRoute?.takeIf { it.stillApproved() && (expectedNode == null || it.nodeId == expectedNode) }
        }
        private var credentialObserved = false
        private var credentialFingerprint: ByteArray? = null

        private fun currentCredentialFingerprint(): ByteArray? {
            val stored = ChatGptAccessProtocol.read(file) ?: return null
            val token = stored.optJSONObject("openai")
            val relevant = if (token != null) JSONObject().apply {
                for (key in listOf("access", "expires", "accountId")) put(key, token.opt(key))
            } else if (stored.optInt("voxgolemChatgpt") == 1 && stored.optBoolean("signedOut"))
                ChatGptAccessProtocol.tombstone() else return null
            // Retain no extra bearer/account copy and never export this local comparison marker.
            return MessageDigest.getInstance("SHA-256").digest(relevant.toString().toByteArray(Charsets.UTF_8))
        }
        private fun report(counter: AtomicInteger, category: String) {
            val count = counter.updateAndGet { if (it == Int.MAX_VALUE) it else it + 1 }
            if ((count and (count - 1)) == 0) Log.w("ChatGptAccess", "stage=$category count=$count")
        }
        fun submit(work: Owner.() -> Unit): Boolean = try {
            executor.execute {
                operationWait = BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30))
                operationRoute = null; routeSelected = false
                try { work() } catch (_: Exception) { report(failed, "sync_failed") }
                catch (_: LinkageError) { report(failed, "native_unavailable") }
                finally {
                    rearmObservedReconnect()
                    operationWait = null; operationRoute = null; routeSelected = false
                    drainTimer()
                    if (changeRequested.get()) scheduleChange()
                    if (opportunityRequested.get()) scheduleOpportunity()
                }
            }; true
        } catch (_: RejectedExecutionException) { report(dropped, "queue_dropped"); false }

        fun changed() {
            changeRequested.set(true)
            scheduleChange()
        }

        fun opportunity() {
            opportunityRequested.set(true)
            scheduleOpportunity()
        }

        private fun scheduleOpportunity() {
            if (!opportunityQueued.compareAndSet(false, true)) return
            if (!submit {
                try {
                    opportunityRequested.set(false)
                    val association = invalidateAssociation() ?: return@submit
                    val current = exchange ?: return@submit
                    if (current.association != association) return@submit
                    // Observe actual fresh discovery even while attempts remain active. SDK
                    // callback repetition is not itself evidence of a reconnect transition.
                    selectedRoute()
                } finally { opportunityQueued.set(false) }
            }) opportunityQueued.set(false)
        }

        private fun rearmObservedReconnect() {
            val current = exchange ?: return
            if (current.attempts < MAX_ATTEMPTS || !current.reconnectPending) return
            val exhaustedAttempts = current.attempts
            current.reconnectPending = false
            current.attempts = 0
            // An observed unavailable -> approved-route edge grants one finite budget.
            // Keep nonce/receipt, and let a late ACK retire this delayed retry normally.
            try { later(RETRY_DELAYS[0]) { attempt() } }
            catch (_: Exception) {
                current.attempts = exhaustedAttempts
                current.reconnectPending = true
                report(failed, "retry_schedule_failed")
            }
        }

        private fun scheduleChange() {
            if (!queued.compareAndSet(false, true)) return
            if (!submit {
                try {
                    changeRequested.set(false)
                    val previousAssociation = boundAssociation
                    val association = invalidateAssociation() ?: return@submit
                    val fingerprint = currentCredentialFingerprint()
                    val credentialChanged = fingerprint != null &&
                        (credentialFingerprint == null || !fingerprint.contentEquals(credentialFingerprint))
                    val initial = !credentialObserved
                    credentialObserved = true
                    if (fingerprint != null) credentialFingerprint = fingerprint
                    if (!initial && !credentialChanged && previousAssociation == association) return@submit
                    begin(association)
                } finally { queued.set(false) }
            }) queued.set(false)
        }

        private fun invalidateAssociation(): String? {
            val version = EnrolledPeerRegistry.approvalVersion()
            // Reachability/setup failures are not revocation. Offline access ends at token expiry.
            if (!EnrolledPeerRegistry.approvalStable()) return null
            val association = setup.credentialAssociation()
            if (!EnrolledPeerRegistry.approvalStable() || version != EnrolledPeerRegistry.approvalVersion()) return null
            if (boundAssociation != association) {
                retire()
                lastHint = null
                persistedReceipt = null
                boundAssociation = association
            }
            if (setup.role != AssistantPeerRole.WATCH) return association
            val stored = ChatGptAccessProtocol.read(file) ?: return association
            if (stored.optJSONObject(ChatGptAccessProtocol.META)?.optString("association") != (association ?: "")) {
                ChatGptAccessProtocol.publish(file, ChatGptAccessProtocol.tombstone().put(ChatGptAccessProtocol.META,
                    JSONObject().put("association", association ?: "").put("generation", 0)))
                AssistantRuntime.refreshIfStarted()
            }
            if (validatedApproval != version) {
                validatedApproval = version
                AssistantRuntime.refreshIfStarted()
            }
            return association
        }

        private fun cancelTimer() { timer?.invoke(); timer = null; timerVersion++; timerReady.set(null) }
        private fun drainTimer() {
            val work = timerReady.getAndSet(null) ?: return
            if (!submit { work.run() }) timerReady.compareAndSet(null, work)
        }
        private fun later(delay: Long, work: () -> Unit) {
            cancelTimer()
            val version = timerVersion
            timer = schedule(Runnable {
                // All protocol state remains actor-owned. A removed/dequeued timer is fenced.
                timerReady.set(Runnable fired@ {
                    if (version != timerVersion) return@fired
                    timer = null
                    work()
                })
                drainTimer()
            }, delay)
        }

        private fun retire() { cancelTimer(); exchange = null }
        private fun begin(association: String) {
            retire()
            exchange = Exchange(association)
            attempt()
        }
        private fun retry(current: Exchange) {
            if (exchange !== current) return
            if (current.attempts < MAX_ATTEMPTS) {
                later(RETRY_DELAYS[(current.attempts - 1).coerceAtLeast(0)]) { attempt() }
            } else {
                // Retain only the nonce/receipt to accept an in-flight final response/ACK.
                // Drop bearer material and never immediately re-arm inside the expiry margin.
                current.packet = null
                cancelTimer()
                idleExpiry()
            }
        }
        private fun idleExpiry() {
            if (setup.role != AssistantPeerRole.WATCH) return
            val expires = ChatGptAccessProtocol.read(file)?.optJSONObject("openai")?.optLong("expires") ?: return
            val delay = expires - now() - EXPIRY_MARGIN
            if (delay > 0) later(delay) {
                val association = invalidateAssociation() ?: return@later
                begin(association)
            }
        }
        private fun attempt() {
            val current = exchange ?: return
            val association = invalidateAssociation()
            if (exchange !== current) return
            if (association != null && association != current.association) { retire(); return }
            current.attempts++
            // Arm before SDK/export work: failures and acknowledgements lost after send need retry.
            retry(current)
            val route = selectedRoute() ?: return
            if (setup.role == AssistantPeerRole.PHONE) send(route, "hint", JSONObject().put("exchange", current.nonce))
            else send(route, "request", JSONObject().put("association", current.association).put("nonce", current.nonce))
        }

        private fun send(route: AssistantVerifiedRoute, kind: String, value: JSONObject) {
            val data = value.toString().toByteArray(Charsets.UTF_8)
            require(data.size <= ChatGptAccessProtocol.MAX_BYTES)
            check(route.stillApproved())
            val wait = requireNotNull(operationWait)
            wait.remainingMillis()
            if (message != null) message.invoke(route, kind, data)
            else wait.issue {
                check(route.stillApproved())
                Wearable.getMessageClient(app).sendMessage(route.nodeId, ChatGptAccessProtocol.PREFIX + kind, data)
            }
        }

        fun receive(source: String, kind: String, bytes: ByteArray) {
            val previous = operationWait
            if (previous == null) {
                operationWait = BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30))
                operationRoute = null; routeSelected = false
            }
            try { receiveInOperation(source, kind, bytes) }
            finally {
                operationWait = previous
                if (previous == null) {
                    rearmObservedReconnect()
                    operationRoute = null; routeSelected = false
                }
            }
        }

        private fun receiveInOperation(source: String, kind: String, bytes: ByteArray) {
            require(bytes.size <= ChatGptAccessProtocol.MAX_BYTES)
            val association = invalidateAssociation() ?: return
            val route = selectedRoute(source) ?: return
            if (setup.role == AssistantPeerRole.PHONE) {
                if (kind == "ack") {
                    val receipt = ChatGptAccessProtocol.receipt(bytes)
                    val pending = exchange?.receipt ?: return
                    if (ChatGptAccessProtocol.sameReceipt(receipt, pending)) retire()
                    return
                }
                if (kind != "request" || !file.exists()) return
                val request = JSONObject(bytes.toString(Charsets.UTF_8))
                require(ChatGptAccessProtocol.keys(request) == setOf("association", "nonce"))
                require(request.getString("association") == association)
                val nonce = request.getString("nonce")
                require(UUID.fromString(nonce).toString() == nonce)
                val current = exchange?.takeIf { it.association == association } ?: Exchange(association, nonce, attempts = 1).also { exchange = it }
                retry(current)
                current.packet?.takeIf { it.getString("nonce") == nonce && (it.isNull("credential") ||
                    it.getJSONObject("credential").getJSONObject("openai").getLong("expires") > now() + EXPIRY_MARGIN)
                }?.let { send(route, "state", it); return }
                current.packet = null
                current.receipt = null
                val exported = export(file.path)
                // An error is not a signed-out snapshot, even when a previously issued token expires.
                if (exported.toByteArray(Charsets.UTF_8).size > ChatGptAccessProtocol.MAX_BYTES) return
                val access = if (exported == "null") {
                    // Missing/corrupt storage is not an explicit sign-out. Confirm the native
                    // tombstone before withdrawing a watch's previously issued access token.
                    val signedOut = ChatGptAccessProtocol.read(file) ?: return
                    if (signedOut.optInt("voxgolemChatgpt") != 1 || !signedOut.optBoolean("signedOut") || signedOut.has("openai")) return
                    null
                } else JSONObject(exported).let {
                    if (it.has("error")) { report(failed, "export_failed"); return }
                    ChatGptAccessProtocol.access(it, now())
                }
                val previous = ChatGptAccessProtocol.read(generationFile)?.getLong("generation") ?: 0
                check(previous < Long.MAX_VALUE)
                val generation = previous + 1
                ChatGptAccessProtocol.publish(generationFile, JSONObject().put("generation", generation)) { route.stillApproved() }
                val packet = JSONObject().put("version", 1).put("association", association).put("nonce", nonce)
                    .put("generation", generation).put("credential", access ?: JSONObject.NULL)
                current.packet = packet
                current.receipt = ChatGptAccessProtocol.receiptFor(packet)
                send(route, "state", packet)
            } else when (kind) {
                "hint" -> {
                    require(bytes.size <= 128)
                    val hint = JSONObject(bytes.toString(Charsets.UTF_8))
                    require(ChatGptAccessProtocol.keys(hint) == setOf("exchange"))
                    val id = hint.getString("exchange")
                    require(UUID.fromString(id).toString() == id)
                    if (id == exchange?.nonce) {
                        // The phone is retrying an exchange initiated by this watch (e.g. expiry).
                        // Its hint must not reset the existing attempt budget or request nonce.
                        lastHint = id
                    } else if (id == lastHint || id == persistedReceipt?.optString("nonce")) {
                        lastHint = id
                        persistedReceipt?.let { send(route, "ack", it) }
                    } else { lastHint = id; persistedReceipt = null; begin(association) }
                }
                "state" -> {
                    val current = exchange ?: return
                    val nonce = current.nonce
                    val stored = ChatGptAccessProtocol.read(file)
                    val meta = stored?.optJSONObject(ChatGptAccessProtocol.META)
                    val previous = if (meta?.optString("association") == association) meta.optLong("generation") else 0
                    val received = JSONObject(bytes.toString(Charsets.UTF_8))
                    val duplicate = previous > 0 && received.optLong("generation") == previous && meta?.optString("nonce") == nonce
                    val state = ChatGptAccessProtocol.packet(bytes, association, nonce, if (duplicate) previous - 1 else previous, now())
                    // Do not turn an unusable old export into an immediate renewal loop.
                    state.optJSONObject("openai")?.let { require(it.getLong("expires") > now() + EXPIRY_MARGIN) }
                    val approval = EnrolledPeerRegistry.approvalVersion()
                    check(route.stillApproved())
                    if (duplicate) require(stored.toString() == state.toString())
                    else ChatGptAccessProtocol.publish(file, state) { route.stillApproved() }
                    credentialFingerprint = currentCredentialFingerprint()
                    credentialObserved = true
                    validatedApproval = approval
                    val receipt = ChatGptAccessProtocol.receiptFor(received)
                    persistedReceipt = receipt
                    retire()
                    idleExpiry()
                    AssistantRuntime.refreshIfStarted()
                    send(route, "ack", receipt)
                }
            }
        }
    }
}
