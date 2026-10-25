package dev.gitblame.voxgolem.assistant.replication

import android.content.Context
import android.net.Uri
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.CapabilityClient
import com.google.android.gms.wearable.CapabilityInfo
import com.google.android.gms.wearable.ChannelClient
import com.google.android.gms.wearable.DataItem
import com.google.android.gms.wearable.DataMapItem
import com.google.android.gms.wearable.Node
import com.google.android.gms.wearable.PutDataMapRequest
import com.google.android.gms.wearable.PutDataRequest
import com.google.android.gms.wearable.Wearable
import org.json.JSONObject
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.io.InputStream
import java.io.OutputStream
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

internal class GmsReplicationTransport(
    internal val sdk: GmsReplicationSdk,
    private val wait: BoundedGmsWait,
) : ReplicationTransport {
    constructor(context: Context, wait: BoundedGmsWait) : this(AndroidGmsReplicationSdk(context), wait)

    internal fun withWait(operationWait: BoundedGmsWait) = GmsReplicationTransport(sdk, operationWait)

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

    fun closeChannel(channel: ChannelClient.Channel) {
        runCatching { sdk.closeChannel(channel) }
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
}
