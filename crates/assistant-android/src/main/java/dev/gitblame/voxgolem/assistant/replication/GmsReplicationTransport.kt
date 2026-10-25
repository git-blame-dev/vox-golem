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
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.DataClient
import com.google.android.gms.wearable.DataItem
import com.google.android.gms.wearable.DataMapItem
import com.google.android.gms.wearable.PutDataMapRequest
import com.google.android.gms.wearable.PutDataRequest
import com.google.android.gms.wearable.Node
import com.google.android.gms.wearable.Wearable
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.io.FileDescriptor
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicBoolean

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
    fun sendMessage(nodeId: String, path: String, bytes: ByteArray): Task<Int> =
        throw UnsupportedOperationException()
    fun openChannel(nodeId: String, path: String): Task<ChannelClient.Channel> =
        throw UnsupportedOperationException()
    fun getInputStream(channel: ChannelClient.Channel): Task<InputStream> =
        throw UnsupportedOperationException()
    fun getOutputStream(channel: ChannelClient.Channel): Task<OutputStream> =
        throw UnsupportedOperationException()
    fun closeChannel(channel: ChannelClient.Channel): Task<Void> =
        throw UnsupportedOperationException()
}

private class AndroidGmsReplicationSdk(context: Context) : GmsReplicationSdk {
    private val application = context.applicationContext
    private val data = Wearable.getDataClient(application)
    private val message = Wearable.getMessageClient(application)
    private val channels = Wearable.getChannelClient(application)
    override fun capability(filter: Int) = Wearable.getCapabilityClient(application).getCapability(ReplicationWire.CAPABILITY, filter)
    override fun localNode() = Wearable.getNodeClient(application).localNode
    override fun getItem(uri: Uri) = data.getDataItem(uri)
    override fun putItem(request: PutDataRequest) = data.putDataItem(request)
    override fun deleteItems(uri: Uri, filter: Int) = data.deleteDataItems(uri, filter)
    override fun asset(asset: Asset) = data.getFdForAsset(asset)
    override fun sendMessage(nodeId: String, path: String, bytes: ByteArray) = message.sendMessage(nodeId, path, bytes)
    override fun openChannel(nodeId: String, path: String) = channels.openChannel(nodeId, path)
    override fun getInputStream(channel: ChannelClient.Channel) = channels.getInputStream(channel)
    override fun getOutputStream(channel: ChannelClient.Channel) = channels.getOutputStream(channel)
    override fun closeChannel(channel: ChannelClient.Channel) = channels.close(channel)
}

internal class GmsSessionConnection(
    private val channel: ChannelClient.Channel,
    override val session: String,
    private val input: DataInputStream,
    private val output: DataOutputStream,
    private val wait: BoundedGmsWait,
    private val sdk: GmsReplicationSdk,
) : ReplicationSessionConnection {
    private val closed = AtomicBoolean(false)
    override val nodeId: String get() = channel.nodeId
    override val path: String get() = channel.path

    override fun readFrame(): JSONObject {
        wait.remainingMillis()
        val length = input.readInt()
        require(length in 2..ReplicationWire.MAX_CONTROL_BYTES) { "Invalid control frame length: $length" }
        val bytes = readExact(length)
        return ReplicationWire.decode(bytes, ReplicationWire.MAX_CONTROL_BYTES)
    }

    override fun writeFrame(frame: JSONObject) {
        wait.remainingMillis()
        val bytes = ReplicationWire.encode(frame)
        wait.remainingMillis()
        output.writeInt(bytes.size)
        output.write(bytes)
        output.flush()
    }

    override fun readBytes(length: Int): ByteArray {
        require(length in 1..ReplicationWire.MAX_CHUNK_BYTES) { "Invalid chunk length: $length" }
        return readExact(length)
    }

    override fun writeBytes(bytes: ByteArray, offset: Int, length: Int) {
        require(length in 1..ReplicationWire.MAX_CHUNK_BYTES) { "Invalid chunk length: $length" }
        require(offset >= 0 && offset <= bytes.size - length) { "Invalid chunk bounds" }
        wait.remainingMillis()
        output.write(bytes, offset, length)
        output.flush()
    }

    private fun readExact(length: Int): ByteArray {
        val bytes = ByteArray(length)
        var offset = 0
        while (offset < length) {
            wait.remainingMillis()
            val step = minOf(8192, length - offset)
            val read = input.read(bytes, offset, step)
            if (read < 0) throw EOFException("Unexpected end of replication stream")
            offset += read
        }
        return bytes
    }

    override fun close() {
        if (!closed.compareAndSet(false, true)) return
        runCatching { input.close() }
        runCatching { output.close() }
        runCatching { sdk.closeChannel(channel) }
    }
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

    override fun sendHint(nodeId: String, sender: String, target: String) {
        require(nodeId.isNotEmpty())
        val bytes = ReplicationWire.encodeHint(sender, target)
        wait.issue { sdk.sendMessage(nodeId, ReplicationWire.HINT_PATH, bytes) }
    }

    override fun openSession(nodeId: String, session: String): ReplicationSessionConnection {
        require(nodeId.isNotEmpty())
        val path = ReplicationWire.sessionPath(session)
        val channel = wait.issue(lateCleanup = { sdk.closeChannel(it) }) { sdk.openChannel(nodeId, path) }
        return attachStreams(channel, session)
    }

    fun acceptSession(channel: ChannelClient.Channel): ReplicationSessionConnection {
        val session = requireNotNull(ReplicationWire.parseSessionPath(channel.path)) { "Invalid replication session path" }
        return attachStreams(channel, session)
    }

    private fun attachStreams(channel: ChannelClient.Channel, session: String): ReplicationSessionConnection {
        var inputStream: InputStream? = null
        var outputStream: OutputStream? = null
        try {
            inputStream = wait.issue(lateCleanup = { it.close() }) { sdk.getInputStream(channel) }
            outputStream = wait.issue(lateCleanup = { it.close() }) { sdk.getOutputStream(channel) }
            return GmsSessionConnection(
                channel,
                session,
                DataInputStream(inputStream),
                DataOutputStream(outputStream),
                wait,
                sdk,
            )
        } catch (error: Throwable) {
            runCatching { inputStream?.close() }
            runCatching { outputStream?.close() }
            runCatching { sdk.closeChannel(channel) }
            throw error
        }
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
