package dev.gitblame.voxgolem.assistant.replication

import android.content.Context
import android.net.Uri
import android.system.Os
import android.system.OsConstants
import android.system.StructPollfd
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.Asset
import com.google.android.gms.wearable.CapabilityClient
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.DataClient
import com.google.android.gms.wearable.DataItem
import com.google.android.gms.wearable.DataMapItem
import com.google.android.gms.wearable.PutDataMapRequest
import com.google.android.gms.wearable.PutDataRequest
import com.google.android.gms.wearable.Node
import com.google.android.gms.wearable.Wearable
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.FileDescriptor
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

internal class BoundedGmsWait(private val deadlineNanos: Long, private val taskTimeoutMillis: Long = 5_000) {
    fun <T> issue(lateCleanup: (T) -> Unit = {}, task: () -> Task<T>): T {
        remainingMillis()
        return await(task(), lateCleanup)
    }
    fun remainingMillis(): Long = TimeUnit.NANOSECONDS.toMillis(deadlineNanos - System.nanoTime()).also {
        if (it <= 0 || Thread.currentThread().isInterrupted) throw TimeoutException("Assistant replication budget exhausted")
    }

    fun <T> await(task: Task<T>, lateCleanup: (T) -> Unit = {}): T {
        try {
            return Tasks.await(task, minOf(taskTimeoutMillis, remainingMillis()), TimeUnit.MILLISECONDS)
        } catch (error: Exception) {
            // Tasks cannot be cancelled generally. A result arriving after timeout still
            // owns buffers/FDs, or may have staged content which must not be abandoned.
            task.addOnSuccessListener(Executor { it.run() }) { result -> runCatching { lateCleanup(result) } }
            if (error is InterruptedException) Thread.currentThread().interrupt()
            throw error
        }
    }
}

internal interface GmsReplicationSdk {
    fun capability(filter: Int): Task<CapabilityInfo>
    fun localNode(): Task<Node>
    fun getItem(uri: Uri): Task<DataItem>
    fun putItem(request: PutDataRequest): Task<DataItem>
    fun deleteItems(uri: Uri, filter: Int): Task<Int>
    fun asset(asset: Asset): Task<DataClient.GetFdForAssetResponse>
}

private class AndroidGmsReplicationSdk(context: Context) : GmsReplicationSdk {
    private val application = context.applicationContext
    private val data = Wearable.getDataClient(application)
    override fun capability(filter: Int) = Wearable.getCapabilityClient(application).getCapability(ReplicationWire.CAPABILITY, filter)
    override fun localNode() = Wearable.getNodeClient(application).localNode
    override fun getItem(uri: Uri) = data.getDataItem(uri)
    override fun putItem(request: PutDataRequest) = data.putDataItem(request)
    override fun deleteItems(uri: Uri, filter: Int) = data.deleteDataItems(uri, filter)
    override fun asset(asset: Asset) = data.getFdForAsset(asset)
}

internal interface AssetDescriptorIo {
    fun poll(descriptor: FileDescriptor, timeoutMillis: Int): Int
    fun read(descriptor: FileDescriptor, buffer: ByteArray, length: Int): Int
}

private object AndroidAssetDescriptorIo : AssetDescriptorIo {
    override fun poll(descriptor: FileDescriptor, timeoutMillis: Int): Int {
        val poll = StructPollfd().apply { fd = descriptor; events = OsConstants.POLLIN.toShort() }
        return Os.poll(arrayOf(poll), timeoutMillis)
    }
    override fun read(descriptor: FileDescriptor, buffer: ByteArray, length: Int) = Os.read(descriptor, buffer, 0, length)
}

internal class GmsReplicationTransport(private val sdk: GmsReplicationSdk, private val wait: BoundedGmsWait,
    private val assetIo: AssetDescriptorIo = AndroidAssetDescriptorIo,
) : ReplicationTransport {
    constructor(context: Context, wait: BoundedGmsWait) : this(AndroidGmsReplicationSdk(context), wait)

    internal fun withWait(operationWait: BoundedGmsWait) = GmsReplicationTransport(sdk, operationWait, assetIo)

    override fun reachable() = peers(CapabilityClient.FILTER_REACHABLE)
    fun knownPeers() = peers(CapabilityClient.FILTER_ALL)
    private fun peers(filter: Int): List<PeerNode> {
        val capability = wait.issue { sdk.capability(filter) }
        require(capability.name == ReplicationWire.CAPABILITY && capability.nodes.size <= 64)
        return capability.nodes.map { PeerNode(it.id, it.displayName) }
    }

    override fun publish(path: String, bytes: ByteArray, assets: Map<String, ByteArray>) {
        require(bytes.size <= ReplicationWire.MAX_CONTROL_BYTES && assets.size <= ReplicationWire.MAX_ASSETS)
        require(assets.values.sumOf { it.size.toLong() } <= ReplicationWire.MAX_ASSET_BYTES)
        val request = PutDataMapRequest.create(path)
        request.dataMap.putByteArray("frame", bytes)
        request.dataMap.putString("delivery", java.util.UUID.randomUUID().toString())
        for ((name, body) in assets) {
            require(name.matches(Regex("body_[0-7]")) && body.size <= ReplicationWire.MAX_RECORD_BYTES)
            request.dataMap.putAsset(name, Asset.createFromBytes(body))
        }
        // Finite fixed slots per enrolled pair; target fields are routing, not encryption.
        wait.issue(lateCleanup = { late: DataItem ->
            sdk.deleteItems(late.uri, DataClient.FILTER_LITERAL)
        }) { sdk.putItem(request.asPutDataRequest().setUrgent()) }
    }

    fun receive(item: DataItem, controller: ReplicationController): Boolean {
        val path = item.uri.path ?: return false
        val node = item.uri.host ?: return false
        if (!path.startsWith(ReplicationWire.PREFIX)) return false
        require((item.data?.size ?: 0) <= ReplicationWire.MAX_CONTROL_BYTES + 16 * 1024)
        val map = DataMapItem.fromDataItem(item).dataMap
        val frame = map.getByteArray("frame") ?: return false
        require(map.keySet().size <= ReplicationWire.MAX_ASSETS + 2)
        return controller.onData(node, path, frame) { name, max ->
            val asset = map.getAsset(name) ?: error("Missing record Asset")
            readAsset(asset, max)
        }
    }

    fun checkBudget() { wait.remainingMillis() }

    fun publishIdentity(replica: String, role: AssistantPeerRole) {
        val identity = JSONObject().put("version", 3).put("replica", ReplicationWire.uuid(replica))
        identity.put("role", role.name.lowercase())
        val request = PutDataMapRequest.create(ReplicationWire.IDENTITY_PATH)
        request.dataMap.putByteArray("frame", identity.toString().toByteArray(Charsets.UTF_8))
        // Unchanged identity must not emit another change event and cause reciprocal publication loops.
        wait.issue { sdk.putItem(request.asPutDataRequest().setUrgent()) }
    }

    fun identityFor(nodeId: String): String? = helloFor(nodeId)?.replica

    fun helloFor(nodeId: String): AssistantPeerHello? {
        val uri = Uri.Builder().scheme("wear").authority(nodeId).path(ReplicationWire.IDENTITY_PATH).build()
        val item: DataItem? = wait.issue { sdk.getItem(uri) }
        if (item == null) return null
        require(item.uri == uri && (item.data?.size ?: 0) <= 2048 && item.assets.isEmpty())
        val bytes = DataMapItem.fromDataItem(item).dataMap.getByteArray("frame") ?: return null
        return try {
            val identity = ReplicationWire.decode(bytes, 512)
            require(identity.length() in 2..4 && identity.keys().asSequence().all { it in setOf("version", "replica", "installation", "role") })
            val version = identity.get("version")
            require(version is Int && version > 0)
            if (version == 3) require(identity.length() == 3 && !identity.has("installation"))
            AssistantPeerHello(ReplicationWire.uuid(identity.getString("replica")),
                version,
                when (identity.optString("role")) { "phone" -> AssistantPeerRole.PHONE; "watch" -> AssistantPeerRole.WATCH; else -> null })
        } catch (error: org.json.JSONException) { throw IllegalArgumentException("Incompatible Assistant identity", error) }
    }

    fun poll(local: String, state: ReplicationState, association: PeerAssociationProvider, controller: ReplicationController): Boolean {
        var accepted = true
        val enrolled = state.enrolledReplicas()
        for (node in reachable()) {
            wait.remainingMillis()
            val peer = association.replicaForNode(node.nodeId) ?: continue
            wait.remainingMillis()
            if (peer !in enrolled || peer == local) continue
            for (kind in ReplicationWire.kinds) {
                val uri = Uri.Builder().scheme("wear").authority(node.nodeId).path(ReplicationWire.path(peer, local, kind)).build()
                val item: DataItem? = wait.issue { sdk.getItem(uri) }
                // Exact slots avoid loading an attacker-sized prefix buffer in the SDK.
                if (item != null && !receive(item, controller)) accepted = false
            }
        }
        return accepted
    }

    fun cleanup(local: String, store: ReplicationStore, state: ReplicationState) {
        fun checkedRevision(value: String): String {
            require(value.matches(Regex("0|[1-9][0-9]{0,19}")) &&
                java.math.BigInteger(value) <= java.math.BigInteger("18446744073709551615"))
            return value
        }
        val node = wait.issue { sdk.localNode() }.id
        val enrolled = state.enrolledReplicas()
        val pendingCleanup = state.cleanupReplicas()
        require(enrolled.size <= ReplicationWire.MAX_PEERS && pendingCleanup.size <= ReplicationWire.MAX_PEERS * 2)
        for (peer in pendingCleanup) {
            if (peer in enrolled) continue
            val uri = Uri.Builder().scheme("wear").authority(node).path("${ReplicationWire.PREFIX}$local/$peer/").build()
            wait.issue { sdk.deleteItems(uri, DataClient.FILTER_PREFIX) }
            state.cleanupComplete(peer)
        }
        for (peer in enrolled) {
            for (kind in listOf("hint", "inventory", "records")) {
                wait.remainingMillis()
                val uri = Uri.Builder().scheme("wear").authority(node).path(ReplicationWire.path(local, peer, kind)).build()
                val item: DataItem? = wait.issue { sdk.getItem(uri) }
                if (item == null) continue
                val selection = runCatching {
                    require((item.data?.size ?: 0) <= ReplicationWire.MAX_CONTROL_BYTES + 16 * 1024)
                    val raw = DataMapItem.fromDataItem(item).dataMap.getByteArray("frame") ?: error("Invalid frame")
                    val frame = ReplicationWire.decode(raw)
                    require(frame.getString("sender") == local)
                    require(frame.getString("target") == peer)
                    val payload = frame.getJSONObject("payload")
                    val entries = when (frame.getString("kind")) {
                        "inventory" -> org.json.JSONArray()
                        "records" -> org.json.JSONArray(ReplicationWire.arrayObjects(payload.getJSONArray("records"))
                            .map { it.getJSONObject("entry") })
                        "hint" -> payload.optJSONArray("offers") ?: org.json.JSONArray()
                        else -> return@runCatching null
                    }
                    ReplicationWire.entries(entries)
                    require(entries.toString().toByteArray(Charsets.UTF_8).size <= ReplicationWire.MAX_CONTROL_BYTES)
                    require(ReplicationWire.arrayObjects(entries).all { it.length() == 4 && !it.isNull("body_key") })
                    val revision = if (frame.getString("kind") == "inventory") {
                        val value = payload.getJSONObject("page").get("revision")
                        require(value is String)
                        checkedRevision(value)
                    } else null
                    entries to revision
                }
                val remove = if (selection.isFailure) true else {
                    val (entries, revision) = selection.getOrNull() ?: continue
                    // Only demonstrably malformed frames authorize deletion on parse
                    // failure. Unknown canonical state or an invalid native response
                    // must propagate to the retry owner, never become evidence to delete.
                    val disposition = store.retainedDescriptors(entries, revision)
                    require(disposition.length() == 3 && disposition.get("revision") is String && disposition.get("revision_matches") is Boolean)
                    val currentRevision = checkedRevision(disposition.getString("revision"))
                    val revisionMatches = disposition.getBoolean("revision_matches")
                    require(revisionMatches == (revision == null || revision == currentRevision))
                    val retained = disposition.getJSONArray("retained")
                    require(retained.length() == entries.length() && (0 until retained.length()).all { retained.get(it) is Boolean })
                    !revisionMatches || (0 until retained.length()).any { !retained.getBoolean(it) }
                }
                if (remove) wait.issue { sdk.deleteItems(item.uri, DataClient.FILTER_LITERAL) }
            }
        }
    }

    private fun readAsset(asset: Asset, max: Int): ByteArray {
        require(max in 2..ReplicationWire.MAX_RECORD_BYTES)
        val result = wait.issue(lateCleanup = { it: DataClient.GetFdForAssetResponse -> it.release() }) { sdk.asset(asset) }
        try {
            val descriptor = result.fdForAsset
            val output = ByteArrayOutputStream(minOf(max, 8192))
            val buffer = ByteArray(8192)
            while (output.size() <= max) {
                val timeout = minOf(5_000, wait.remainingMillis()).toInt()
                if (assetIo.poll(descriptor.fileDescriptor, timeout) == 0) throw TimeoutException("Assistant Asset read timed out")
                wait.remainingMillis()
                val count = assetIo.read(descriptor.fileDescriptor, buffer, minOf(buffer.size, max + 1 - output.size()))
                if (count == 0) return output.toByteArray()
                output.write(buffer, 0, count)
            }
            error("Assistant Asset exceeds declared bound")
        } finally { result.release() }
    }
}
