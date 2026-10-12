package dev.gitblame.voxgolem.assistant.inputtransport

import android.content.Context
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.CapabilityClient
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.Wearable
import dev.gitblame.voxgolem.assistant.replication.AssistantPeerHello
import dev.gitblame.voxgolem.assistant.replication.AssistantPeerRole
import dev.gitblame.voxgolem.assistant.replication.AssistantPeerSetupOwner
import dev.gitblame.voxgolem.assistant.replication.BoundedGmsWait
import dev.gitblame.voxgolem.assistant.replication.EnrolledPeerRegistry
import dev.gitblame.voxgolem.assistant.replication.GmsReplicationTransport
import dev.gitblame.voxgolem.assistant.replication.PeerPairingReason
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Future
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit

data class AssistantInputPeerRoute(val nodeId: String, val localInstallation: String, val peerInstallation: String)

/** Dedicated input lifecycle facade over the Application's approved conversation association. */
interface AssistantInputPeers : SourcePeerBinding, AutoCloseable {
    fun selected(): AssistantInputPeerRoute?
    fun onForeground(active: Boolean)
    fun refresh()
}

internal data class InputPeerSelection(val local: String, val peer: String, val replica: String, val association: String, val peerRole: AssistantPeerRole)
internal interface InputPeerNativeState {
    fun selected(): InputPeerSelection?
    fun stillApproved(selection: InputPeerSelection): Boolean
}
internal interface InputPeerSdk {
    fun capability(): Task<CapabilityInfo>
    fun hello(node: String, deadlineNanos: Long): AssistantPeerHello?
}

internal class ApplicationInputPeerState(context: Context) : InputPeerNativeState {
    private val application = context.applicationContext
    override fun selected(): InputPeerSelection? {
        val setup = (application as? AssistantPeerSetupOwner)?.assistantPeerSetup ?: return null
        val selected = setup.state()
        if (selected.reason != PeerPairingReason.ENROLLED) return null
        val peer = selected.peerInstallationId ?: return null
        val replica = selected.peerReplica ?: return null
        val association = selected.associationId ?: return null
        val local = setup.installation()
        listOf(local, peer, replica, association).forEach(AssistantInputWire::uuid)
        if (local == peer) return null
        return EnrolledPeerRegistry(application).use { registry ->
            val binding = registry.bindingFor(peer, replica) ?: return@use null
            if (replica !in registry.replicas() || binding.association != association) null
            else InputPeerSelection(local, peer, replica, association,
                if (setup.role == AssistantPeerRole.PHONE) AssistantPeerRole.WATCH else AssistantPeerRole.PHONE)
        }
    }
    override fun stillApproved(selection: InputPeerSelection) = selected() == selection
}

private class AndroidInputPeerSdk(context: Context) : InputPeerSdk {
    private val application = context.applicationContext
    override fun capability() = Wearable.getCapabilityClient(application)
        .getCapability(AssistantInputWire.CAPABILITY, CapabilityClient.FILTER_REACHABLE)
    override fun hello(node: String, deadlineNanos: Long) =
        GmsReplicationTransport(application, BoundedGmsWait(deadlineNanos, 2000)).helloFor(node)
}

/** Fresh SDK routes are volatile; unknown/revoked/ambiguous peers never use a saved node fallback. */
class ApplicationAssistantInputPeers internal constructor(
    private val state: InputPeerNativeState,
    private val sdk: InputPeerSdk,
    private val now: () -> Long = SystemClock::elapsedRealtime,
) : AssistantInputPeers {
    constructor(context: Context) : this(ApplicationInputPeerState(context), AndroidInputPeerSdk(context))
    private data class Snapshot(val selection: InputPeerSelection, val route: AssistantInputPeerRoute, val expires: Long)
    private val main = Handler(Looper.getMainLooper())
    private val worker = ThreadPoolExecutor(1, 1, 30, TimeUnit.SECONDS, ArrayBlockingQueue(1),
        { job -> Thread(job, "assistant-input-peer").apply { isDaemon = true } }, ThreadPoolExecutor.AbortPolicy())
        .apply { allowCoreThreadTimeOut(true) }
    private var active = false
    private var closed = false
    private var epoch = 0L
    private var inFlight = false
    private var nextRefresh = 0L
    private var pending: Future<*>? = null
    private var snapshot: Snapshot? = null
    init { requireInputMain() }

    override fun selected(): AssistantInputPeerRoute? {
        requireInputMain()
        val current = snapshot ?: return null
        if (!active || closed || now() >= current.expires || !runCatching { state.stillApproved(current.selection) }.getOrDefault(false)) {
            snapshot = null; return null
        }
        return current.route
    }
    override fun accepts(node: String, source: String, target: String): Boolean = selected()?.let {
        it.nodeId == node && it.peerInstallation == source && it.localInstallation == target
    } ?: false

    override fun onForeground(active: Boolean) {
        requireInputMain(); if (closed) return
        if (this.active == active) return
        this.active = active; epoch++; snapshot = null; nextRefresh = 0
        pending?.cancel(true); pending = null; inFlight = false; worker.purge()
        if (active) refresh()
    }
    override fun refresh() {
        requireInputMain()
        if (closed || !active || inFlight || now() < nextRefresh) return
        val owner = epoch
        inFlight = true; nextRefresh = now() + 1000
        try {
            pending = worker.submit {
                val resolved = runCatching {
                    val selected = state.selected() ?: return@runCatching null
                    val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(2)
                    val capability = Tasks.await(sdk.capability(), 2, TimeUnit.SECONDS)
                    require(capability.name == AssistantInputWire.CAPABILITY && capability.nodes.size <= 64)
                    val matches = capability.nodes.mapNotNull { node ->
                        check(System.nanoTime() < deadline && !Thread.currentThread().isInterrupted)
                        val hello = sdk.hello(node.id, deadline)
                        if (hello?.compatible == true && hello.role == selected.peerRole && hello.replica == selected.replica && hello.installation == selected.peer)
                            AssistantInputPeerRoute(node.id, selected.local, selected.peer) else null
                    }
                    val route = matches.singleOrNull() ?: return@runCatching null
                    if (!state.stillApproved(selected)) return@runCatching null
                    selected to route
                }.getOrNull()
                main.post {
                    if (!closed && active && epoch == owner) {
                        inFlight = false; pending = null
                        snapshot = resolved?.let { Snapshot(it.first, it.second, now() + 3000) }
                    }
                }
            }
        } catch (_: Exception) { inFlight = false; pending = null; snapshot = null }
    }
    override fun close() {
        requireInputMain(); if (closed) return
        onForeground(false); closed = true; worker.shutdownNow()
    }
}
