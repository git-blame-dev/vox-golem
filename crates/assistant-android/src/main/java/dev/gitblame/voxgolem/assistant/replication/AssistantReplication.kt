package dev.gitblame.voxgolem.assistant.replication

import android.app.job.JobInfo
import android.app.job.JobScheduler
import android.content.ComponentName
import android.content.Context
import android.os.Build
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.DataItem
import com.google.android.gms.wearable.MessageEvent
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.assistant.AssistantDomainNative
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.DomainGateway
import java.io.File
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Future
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.zip.ZipFile

enum class AssistantReplicationStatus { IDLE, SYNCING, WAITING_FOR_PEER, RETRY_PENDING, REJECTED, NATIVE_UNAVAILABLE, ASSOCIATION_UNAVAILABLE }

/** Shared native process owner: no WebView, provider execution or capture hooks. */
object AssistantReplication {
    @Volatile private var owner: Owner? = null
    @Volatile var status = AssistantReplicationStatus.IDLE
        private set

    @Synchronized fun start(context: Context, peerAssociationProvider: PeerAssociationProvider, onCommitted: () -> Unit = {}) {
        val application = context.applicationContext
        val current = owner ?: Owner(application, peerAssociationProvider).also { owner = it }
        current.association = peerAssociationProvider
        current.onCommitted = onCommitted
        current.schedulePeriodic()
        current.reconcile()
    }

    /** Startup, foreground resume and settled local mutations never execute generation. */
    fun reconcileNow() { owner?.reconcile() }
    fun reconcileNow(context: Context) { resolve(context)?.reconcile() }

    fun pairingState(context: Context, result: (PeerPairingState) -> Unit) {
        val current = resolve(context, schedulePeriodic = false) ?: run { result(PeerPairingState(PeerPairingReason.UNAVAILABLE)); return }
        current.readPairingState(result)
    }

    fun onData(context: Context, item: DataItem) {
        if (!acceptsData(item)) {
            // Old queued history is inert after the alpha epoch reset; pairing/identity
            // retain their independent v1 namespace and are not classified as history.
            val path = item.uri.path
            val oldV1 = path?.removePrefix("/voxgolem/assistant/v1/")?.split('/')
            val oldV2 = path?.removePrefix(ReplicationWire.PREFIX)?.split('/')
            if ((path?.startsWith("/voxgolem/assistant/v1/") == true && oldV1?.size == 3 && oldV1[2] in ReplicationWire.kinds) ||
                (path?.startsWith(ReplicationWire.PREFIX) == true && oldV2?.size == 3 && oldV2[2] in ReplicationWire.kinds)
            ) {
                status = AssistantReplicationStatus.REJECTED
            }
            return
        }
        val current = resolve(context) ?: return
        // The identity DataItem event is a hint only. Re-query capability and exact host-scoped SDK identity.
        current.reconcile()
    }

    internal fun acceptsData(item: DataItem): Boolean =
        item.uri.host != null &&
            item.uri.path == ReplicationWire.IDENTITY_PATH &&
            (item.data?.size ?: 0) <= 2048 &&
            item.assets.isEmpty()

    internal fun acceptsMessage(message: MessageEvent): Boolean =
        message.sourceNodeId.isNotEmpty() &&
            message.path == ReplicationWire.HINT_PATH &&
            message.data.size in 2..ReplicationWire.MAX_HINT_BYTES

    fun onMessage(context: Context, message: MessageEvent) {
        if (message.path != ReplicationWire.HINT_PATH) return
        if (!acceptsMessage(message)) {
            status = AssistantReplicationStatus.REJECTED
            return
        }
        val current = resolve(context) ?: return
        current.onHint(message.sourceNodeId, message.data.copyOf())
    }

    internal fun acceptsChannel(channel: ChannelClient.Channel): Boolean =
        channel.nodeId.isNotEmpty() && ReplicationWire.validSessionPath(channel.path)

    fun onChannelOpened(context: Context, channel: ChannelClient.Channel) {
        if (!channel.path.startsWith(ReplicationWire.SESSION_PREFIX)) return
        if (!acceptsChannel(channel)) {
            owner?.let { runCatching { it.transport().closeChannel(channel) } }
                ?: runCatching { Wearable.getChannelClient(context.applicationContext).close(channel) }
            status = AssistantReplicationStatus.REJECTED
            return
        }
        val current = resolve(context)
        if (current == null) {
            runCatching { Wearable.getChannelClient(context.applicationContext).close(channel) }
            return
        }
        current.onChannelOpened(channel)
    }

    internal fun runJob(context: Context, finished: () -> Unit): Future<*>? {
        val current = resolve(context) ?: return null
        return current.submit { try { current.reconcileBlocking() } finally { finished() } }
    }

    @Synchronized private fun resolve(context: Context, schedulePeriodic: Boolean = true): Owner? {
        owner?.let { return it }
        val application = context.applicationContext
        val association = (application as? AssistantPeerAssociationOwner)?.assistantPeerAssociationProvider
        if (association == null) { status = AssistantReplicationStatus.ASSOCIATION_UNAVAILABLE; return null }
        return Owner(application, association).also { owner = it; if (schedulePeriodic) it.schedulePeriodic() }
    }

    internal class Owner(val application: Context, @Volatile var association: PeerAssociationProvider,
        private val transportFactory: () -> GmsReplicationTransport = { GmsReplicationTransport(application, BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(50))) },
        private val nativeAvailable: () -> Boolean = { packagedNativeLibrary(application) },
    ) {
        val setup = (application as? AssistantPeerSetupOwner)?.assistantPeerSetup
        @Volatile var onCommitted: () -> Unit = {}
        val state = DurableReplicationState(application)
        private val executor = ThreadPoolExecutor(1, 1, 30, TimeUnit.SECONDS, ArrayBlockingQueue(16),
            { command -> Thread(command, "assistant-replication") }, ThreadPoolExecutor.AbortPolicy()).apply { allowCoreThreadTimeOut(true) }
        private var nativeStore: NativeReplicationStore? = null
        private var nativeDomain: NativeReplicationDomain? = null
        @Volatile private var reconcileQueued = false
        private val pendingHints = LinkedHashMap<String, ByteArray>()
        private var hintQueued = false
        private val periodicRequested = AtomicBoolean(false)
        private val retryRequested = AtomicBoolean(false)
        private val scheduling = Any()

        fun transport() = transportFactory()

        fun submit(work: () -> Unit): Future<*>? = synchronized(scheduling) {
            try {
                executor.submit {
                    try { scheduleRequestedPeriodic(); work() }
                    catch (_: LinkageError) { status = AssistantReplicationStatus.NATIVE_UNAVAILABLE; retry() }
                    catch (_: Exception) { status = AssistantReplicationStatus.RETRY_PENDING; retry() }
                    finally {
                        // Rejection happens only while accepted work owns a full queue. That work
                        // drains scheduler requests without making the foreground caller do DB IO.
                        drainRetryRequest()
                    }
                }
            } catch (_: RejectedExecutionException) {
                status = AssistantReplicationStatus.RETRY_PENDING
                retryRequested.set(true)
                null
            }
        }

        private fun drainRetryRequest() {
            if (synchronized(scheduling) { retryRequested.getAndSet(false) }) retry()
        }

        @Synchronized fun reconcile() {
            if (reconcileQueued) return
            reconcileQueued = true
            if (submit { try { reconcileBlocking() } finally { reconcileQueued = false } } == null) reconcileQueued = false
        }

        fun onHint(nodeId: String, bytes: ByteArray) {
            synchronized(scheduling) {
                if (nodeId !in pendingHints && pendingHints.size >= ReplicationWire.MAX_PEERS) {
                    status = AssistantReplicationStatus.REJECTED
                    return
                }
                pendingHints[nodeId] = bytes
                if (hintQueued) return
                hintQueued = true
            }
            if (submit { drainHints() } == null) {
                synchronized(scheduling) {
                    hintQueued = false
                    pendingHints.clear()
                }
            }
        }

        private fun drainHints() {
            while (true) {
                val (nodeId, bytes) = synchronized(scheduling) {
                    val entry = pendingHints.entries.firstOrNull()
                    if (entry == null) {
                        hintQueued = false
                        return
                    }
                    pendingHints.remove(entry.key)
                    entry.key to entry.value
                }
                withController { transport, store, controller ->
                    val linked = link(transport, store.replica)
                    if (linked.reason != PeerPairingReason.ENROLLED || state.enrolledReplicas().isEmpty()) {
                        status = AssistantReplicationStatus.WAITING_FOR_PEER
                        retry()
                        return@withController
                    }
                    status = AssistantReplicationStatus.SYNCING
                    val role = setup?.role ?: AssistantPeerRole.PHONE
                    if (controller.onHint(nodeId, bytes)) {
                        finishSync(store, sessionCompleted = role == AssistantPeerRole.PHONE)
                    } else {
                        status = AssistantReplicationStatus.REJECTED
                        retry()
                    }
                }
            }
        }

        fun onChannelOpened(channel: ChannelClient.Channel) {
            if (submit { acceptChannelBlocking(channel) } == null) {
                runCatching { transport().closeChannel(channel) }
            }
        }

        private fun acceptChannelBlocking(channel: ChannelClient.Channel) {
            var sessionOpened = false
            try {
                val ran = withController { transport, store, controller ->
                    val linked = link(transport, store.replica)
                    if (linked.reason != PeerPairingReason.ENROLLED || state.enrolledReplicas().isEmpty()) {
                        transport.closeChannel(channel)
                        status = AssistantReplicationStatus.WAITING_FOR_PEER
                        retry()
                        return@withController
                    }
                    status = AssistantReplicationStatus.SYNCING
                    val session = transport.acceptSession(channel)
                    sessionOpened = true
                    if (controller.accept(session)) {
                        finishSync(store, sessionCompleted = true)
                    } else {
                        status = AssistantReplicationStatus.REJECTED
                        retry()
                    }
                }
                if (!ran) runCatching { transport().closeChannel(channel) }
            } finally {
                if (!sessionOpened) runCatching { transport().closeChannel(channel) }
            }
        }

        fun readPairingState(result: (PeerPairingState) -> Unit) {
            try {
                executor.submit {
                    try {
                        val outcome = try { scheduleRequestedPeriodic(); pairingStateBlocking() }
                        catch (_: LinkageError) { PeerPairingState(PeerPairingReason.UNAVAILABLE) }
                        catch (_: Exception) { PeerPairingState(PeerPairingReason.STORAGE_FAILURE) }
                        result(outcome)
                    } finally { drainRetryRequest() }
                }
            } catch (_: RejectedExecutionException) { result(PeerPairingState(PeerPairingReason.UNAVAILABLE)) }
        }

        fun pairingStateBlocking(): PeerPairingState = setup?.state() ?: PeerPairingState(PeerPairingReason.UNAVAILABLE)

        private fun link(transport: GmsReplicationTransport, replica: String): PeerPairingState {
            val currentSetup = setup ?: return PeerPairingState(PeerPairingReason.UNAVAILABLE)
            try { transport.publishIdentity(replica, currentSetup.role) }
            catch (_: Exception) {
                currentSetup.recordFailure(PeerPairingReason.DISCOVERY_FAILED)
                return PeerPairingState(PeerPairingReason.DISCOVERY_FAILED)
            }
            return currentSetup.retry(replica, transport)
        }

        fun reconcileBlocking() {
            withController { transport, store, controller ->
                val linked = link(transport, store.replica)
                if (linked.reason != PeerPairingReason.ENROLLED || state.enrolledReplicas().isEmpty()) {
                    status = AssistantReplicationStatus.WAITING_FOR_PEER
                    // Persisted bounded backoff also covers initial publication and peer-not-ready races.
                    retry(); return@withController
                }
                status = AssistantReplicationStatus.SYNCING
                val role = setup?.role ?: AssistantPeerRole.PHONE
                val routes = operationAssociation ?: association
                val reached = transport.reachable().take(64)
                    .mapNotNull { routes.replicaForNode(it.nodeId) }
                    .filter { it != store.replica && it in state.enrolledReplicas() }
                    .toSet()
                if (reached.isEmpty()) {
                    status = AssistantReplicationStatus.WAITING_FOR_PEER
                    retry()
                    return@withController
                }
                if (controller.reconcile()) {
                    finishSync(store, sessionCompleted = role == AssistantPeerRole.PHONE && reached.containsAll(state.enrolledReplicas()))
                } else {
                    status = AssistantReplicationStatus.REJECTED
                    retry()
                }
            }
        }

        private fun finishSync(store: NativeReplicationStore, sessionCompleted: Boolean) {
            val pendingPublications = state.enrolledReplicas().any { store.publications(it, null, 1).length() > 0 }
            if (!sessionCompleted || pendingPublications) {
                if (scheduler().getPendingJob(RETRY_JOB) == null) retry()
                status = AssistantReplicationStatus.SYNCING
            } else {
                state.succeeded()
                scheduler().cancel(RETRY_JOB)
                status = AssistantReplicationStatus.IDLE
            }
        }

        fun withController(work: (GmsReplicationTransport, NativeReplicationStore, ReplicationController) -> Unit): Boolean {
            if (!nativeAvailable()) {
                setup?.recordFailure(PeerPairingReason.UNAVAILABLE)
                status = AssistantReplicationStatus.NATIVE_UNAVAILABLE; retry(); return false
            }
            val gateway = DomainGateway(AssistantDomainNative::apply)
            val store = nativeStore ?: NativeReplicationStore(AssistantStore(application, gateway)) { onCommitted() }.also { nativeStore = it }
            val domain = nativeDomain ?: NativeReplicationDomain(gateway).also { nativeDomain = it }
            val transport = transport()
            val routes = setup?.operationAssociation(transport) ?: association
            operationAssociation = routes
            val controller = ReplicationController(
                store,
                domain,
                state,
                transport,
                { routes.replicaForNode(it) },
                checkBudget = transport::checkBudget,
                role = setup?.role ?: AssistantPeerRole.PHONE,
            )
            try {
                work(transport, store, controller)
            } finally { operationAssociation = null }
            return true
        }

        private var operationAssociation: PeerAssociationProvider? = null

        fun retry() {
            val delay = state.failed(System.currentTimeMillis())
            scheduler().schedule(JobInfo.Builder(RETRY_JOB, component()).setPersisted(true).setMinimumLatency(delay)
                .setBackoffCriteria(30_000, JobInfo.BACKOFF_POLICY_EXPONENTIAL).build())
        }

        fun schedulePeriodic() {
            periodicRequested.set(true)
            submit {}
        }
        private fun scheduleRequestedPeriodic() {
            if (!synchronized(scheduling) { periodicRequested.getAndSet(false) }) return
            scheduler().schedule(JobInfo.Builder(PERIODIC_JOB, component()).setPersisted(true).setPeriodic(30 * 60 * 1000L).build())
            val remaining = state.retryDelay(System.currentTimeMillis())
            if (remaining > 0) scheduler().schedule(JobInfo.Builder(RETRY_JOB, component()).setPersisted(true).setMinimumLatency(remaining).build())
        }
        private fun component() = ComponentName(application, AssistantReplicationJobService::class.java)
        private fun scheduler() = application.getSystemService(Context.JOB_SCHEDULER_SERVICE) as JobScheduler
    }

    private fun packagedNativeLibrary(context: Context): Boolean {
        val info = context.applicationInfo
        if (File(info.nativeLibraryDir, "libwear_assistant_native.so").isFile) return true
        return (listOf(info.sourceDir) + (info.splitSourceDirs?.toList() ?: emptyList())).any { apk ->
            runCatching { ZipFile(apk).use { zip -> Build.SUPPORTED_ABIS.any { zip.getEntry("lib/$it/libwear_assistant_native.so") != null } } }.getOrDefault(false)
        }
    }
    internal const val PERIODIC_JOB = 0x564701
    internal const val RETRY_JOB = 0x564702
}
