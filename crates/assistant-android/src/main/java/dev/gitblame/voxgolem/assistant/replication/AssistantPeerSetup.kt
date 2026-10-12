package dev.gitblame.voxgolem.assistant.replication

import android.content.Context
import android.content.pm.PackageManager
import dev.gitblame.voxgolem.assistant.AssistantStore
import org.json.JSONObject
import java.util.UUID
import java.util.concurrent.TimeUnit

enum class AssistantPeerRole { PHONE, WATCH }
data class AssistantPeerHello(val replica: String, val version: Int = 3, val role: AssistantPeerRole? = null) {
    val compatible: Boolean get() = version == 3 && role != null
    val installation: String get() = replica
}
enum class PeerPairingReason { ENROLLED, WAITING_FOR_PEER, PEER_UPDATE_REQUIRED, MULTIPLE_PEERS, DISCOVERY_FAILED, STORAGE_FAILURE, UNAVAILABLE }
data class PeerPairingState(val reason: PeerPairingReason, val associationId: String? = null, val peerInstallationId: String? = null,
    val peerReplica: String? = null, val credentialsAllowed: Boolean = false)

interface AssistantPeerSetupOwner { val assistantPeerSetup: AssistantPeerSetup }

/** Data Layer package/signature isolation and native pairing authorize sharing; IDs only bind metadata. */
class AssistantPeerSetup internal constructor(
    context: Context,
    private val localReplica: () -> String,
    private val transportFactory: () -> GmsReplicationTransport,
) {
    private val application = context.applicationContext
    internal val role = if (application.packageManager.hasSystemFeature(PackageManager.FEATURE_WATCH)) AssistantPeerRole.WATCH else AssistantPeerRole.PHONE
    constructor(context: Context) : this(context, { AssistantStore(context).use { it.replica() } }, {
        GmsReplicationTransport(context.applicationContext, BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30)))
    })
    private fun registry() = EnrolledPeerRegistry(application)
    internal fun installation() = localReplica()

    internal fun replicaForNode(node: String): String? = verifiedBinding(node)?.replica

    private fun verifiedBinding(node: String): ApprovedPeerBinding? = runCatching {
        if (state().reason != PeerPairingReason.ENROLLED) return@runCatching null
        val io = transportFactory()
        val selection = discover(io)
        if (selection.reason != PeerPairingReason.ENROLLED || selection.node?.nodeId != node) return@runCatching null
        if (io.reachable().none { it.nodeId == node }) return@runCatching null
        val hello = requireNotNull(selection.hello)
        if (hello.installation == installation()) return@runCatching null
        registry().use { db ->
            db.bindingFor(hello.replica, hello.replica)?.takeIf { it.replica in db.replicas() }
        }
    }.getOrNull()

    fun state(): PeerPairingState = registry().use { db ->
        db.setupValue("status")?.let {
            PeerPairingState(PeerPairingReason.valueOf(it.getString("reason")),
                it.optString("association").takeIf(String::isNotEmpty), it.optString("installation").takeIf(String::isNotEmpty),
                it.optString("replica").takeIf(String::isNotEmpty))
        } ?: PeerPairingState(PeerPairingReason.WAITING_FOR_PEER)
    }

    internal fun recordFailure(reason: PeerPairingReason) {
        registry().use { it.saveSetup("status", JSONObject().put("reason", reason.name)) }
    }

    /** Shared startup, capability, identity-arrival and retry path; no invitation or ACK ceremony. */
    internal fun retry(localReplica: String, io: GmsReplicationTransport): PeerPairingState {
        val selection = discover(io)
        val result = try {
            if (selection.reason != PeerPairingReason.ENROLLED) PeerPairingState(selection.reason)
            else {
                val hello = requireNotNull(selection.hello)
                val peerInstallation = hello.replica
                val localInstallation = installation()
                if (peerInstallation == localInstallation || hello.replica == localReplica) PeerPairingState(PeerPairingReason.MULTIPLE_PEERS)
                else {
                    val phone = if (role == AssistantPeerRole.PHONE) localReplica else hello.replica
                    val watch = if (role == AssistantPeerRole.WATCH) localReplica else hello.replica
                    val association = UUID.nameUUIDFromBytes("voxgolem-native-pair-v3/$phone/$watch"
                        .toByteArray(Charsets.UTF_8)).toString()
                    val binding = ApprovedPeerBinding(hello.replica, association)
                    registry().use { it.rememberPeer(binding) }
                    PeerPairingState(PeerPairingReason.ENROLLED, association, peerInstallation, hello.replica)
                }
            }
        } catch (_: Exception) { PeerPairingState(PeerPairingReason.STORAGE_FAILURE) }
        return try {
            registry().use { db ->
                db.clearLegacySetup()
                db.saveSetup("status", JSONObject().put("reason", result.reason.name)
                    .put("association", result.associationId).put("installation", result.peerInstallationId).put("replica", result.peerReplica))
            }
            result
        } catch (_: Exception) { PeerPairingState(PeerPairingReason.STORAGE_FAILURE) }
    }

    private data class Selection(val reason: PeerPairingReason, val node: PeerNode? = null, val hello: AssistantPeerHello? = null)
    private fun discover(io: GmsReplicationTransport): Selection {
        return try {
            val candidates = mutableListOf<Pair<PeerNode, AssistantPeerHello>>()
            var missing = false
            var incompatible = false
            for (node in io.knownPeers()) {
                io.checkBudget()
                val hello = try { io.helloFor(node.nodeId) } catch (_: IllegalArgumentException) { incompatible = true; continue }
                when {
                    hello == null -> missing = true
                    !hello.compatible -> incompatible = true
                    hello.role != role -> candidates.add(node to hello)
                }
            }
            when {
                candidates.size > 1 -> Selection(PeerPairingReason.MULTIPLE_PEERS)
                incompatible -> Selection(PeerPairingReason.PEER_UPDATE_REQUIRED)
                missing || candidates.isEmpty() -> Selection(PeerPairingReason.WAITING_FOR_PEER)
                else -> candidates.single().let { Selection(PeerPairingReason.ENROLLED, it.first, it.second) }
            }
        } catch (_: Exception) { Selection(PeerPairingReason.DISCOVERY_FAILED) }
    }
}
