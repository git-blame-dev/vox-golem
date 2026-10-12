package dev.gitblame.voxgolem.assistant.replication

import android.net.Uri
import android.os.ParcelFileDescriptor
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.TaskCompletionSource
import com.google.android.gms.tasks.Tasks
import com.google.android.gms.wearable.*
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.File
import java.io.InputStream
import java.io.FileDescriptor
import java.io.FileInputStream
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class GmsReplicationTransportTest {
    @get:Rule val folder = TemporaryFolder()
    private val a = "00000000-0000-4000-8000-000000000001"
    private val b = "00000000-0000-4000-8000-000000000002"

    @Test fun actualSdkSerializationAssetsExactAckAndDeletionCleanupConverge() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val leftSdk = mailbox.sdk("synthetic-left-node")
        val rightSdk = mailbox.sdk("synthetic-right-node")
        val left = FakeReplicationStore(a)
        val right = FakeReplicationStore(b)
        val original = record()
        left.add(original)
        val leftState = MemoryReplicationState().apply { enrolled.add(b) }
        val rightState = MemoryReplicationState().apply { enrolled.add(a) }
        val leftTransport = GmsReplicationTransport(leftSdk, budget(), regularFileIo())
        val rightTransport = GmsReplicationTransport(rightSdk, budget(), regularFileIo())
        val association: (String) -> String? = { node -> when (node) { "synthetic-left-node" -> a; "synthetic-right-node" -> b; else -> null } }
        val leftController = ReplicationController(left, FakeReplicationDomain(1), leftState, leftTransport, association, onRejected = { throw AssertionError(it) })
        val rightController = ReplicationController(right, FakeReplicationDomain(1), rightState, rightTransport, association, onRejected = { throw AssertionError(it) })
        rightController.reconcile()
        var count = 0
        while (mailbox.events.isNotEmpty()) {
            check(count++ < 200)
            val item = mailbox.events.removeAt(0)
            val sourceIsLeft = item.uri.host == "synthetic-left-node"
            val transport = if (sourceIsLeft) rightTransport else leftTransport
            val controller = if (sourceIsLeft) rightController else leftController
            assertTrue(transport.receive(item, controller))
            assertTrue(transport.receive(item, controller)) // Duplicate SDK delivery is harmless.
        }
        assertEquals(left.records.keys, right.records.keys)
        assertEquals(left.records.keys, left.acknowledged[b])
        assertTrue(mailbox.releasedAssets > 0)
        assertEquals(mailbox.openedAssets, mailbox.releasedAssets)
        assertTrue(mailbox.slots.size <= ReplicationWire.kinds.size * 2)

        // The DB no longer retains the body. Even a previously staged Asset must be removed.
        left.quarantined.add("$a/1")
        left.revision++
        leftTransport.cleanup(a, left.snapshot(), leftState)
        assertFalse(mailbox.slots.values.any { it.uri.host == "synthetic-left-node" && it.uri.lastPathSegment == "records" })
        assertEquals(1, left.records.size)
    }

    @Test fun timedOutPutIsCleanedWhenSdkCompletesLateAndRetriesUseDistinctDeliveryNonce() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val sdk = mailbox.sdk("synthetic-left-node")
        val path = ReplicationWire.path(a, b, "hint")
        val frame = ReplicationWire.encode(ReplicationWire.frame(a, b, a, b, "hint", JSONObject()))
        sdk.deferPut = true
        assertThrows(TimeoutException::class.java) { GmsReplicationTransport(sdk, budget(10)).publish(path, frame, emptyMap()) }
        sdk.completePut()
        assertTrue(mailbox.slots.isEmpty())
        sdk.deferPut = false
        val transport = GmsReplicationTransport(sdk, budget())
        transport.publish(path, frame, emptyMap())
        val first = DataMapItem.fromDataItem(mailbox.events.last()).dataMap.getString("delivery")
        transport.publish(path, frame, emptyMap())
        val second = DataMapItem.fromDataItem(mailbox.events.last()).dataMap.getString("delivery")
        assertNotEquals(first, second)
        assertEquals(1, mailbox.slots.size)
    }

    @Test fun lateAssetResultReleasesDescriptorAfterTimeout() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val sdk = mailbox.sdk("synthetic-left-node")
        val task = TaskCompletionSource<DataClient.GetFdForAssetResponse>()
        assertThrows(TimeoutException::class.java) { budget(10).await(task.task) { it.release() } }
        val response = mailbox.response("Synthetic late Asset".toByteArray())
        task.setResult(response)
        assertEquals(1, mailbox.releasedAssets)
        assertEquals(1, mailbox.openedAssets)
        assertEquals("synthetic-left-node", Tasks.await(sdk.localNode()).id)
    }

    @Test fun pollingUsesOnlyCanonicalExactSlotsNotUnboundedSdkPrefixQueries() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val sdk = mailbox.sdk("synthetic-right-node")
        mailbox.sdk("synthetic-left-node")
        val state = MemoryReplicationState().apply { enrolled.add(a) }
        val transport = GmsReplicationTransport(sdk, budget())
        val controller = ReplicationController(FakeReplicationStore(b), FakeReplicationDomain(1), state, transport, { a })
        assertTrue(transport.poll(b, state, PeerAssociationProvider { a }, controller))
        assertEquals(ReplicationWire.kinds.map { ReplicationWire.path(a, b, it) }.toSet(), sdk.lookups.map { it.path }.toSet())
    }

    @Test fun publicIdentityMetadataDoesNotEnrollAndLegacyOrUnknownVersionsAreNotCompatible() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val sender = GmsReplicationTransport(mailbox.sdk("synthetic-left-node"), budget())
        val receiver = GmsReplicationTransport(mailbox.sdk("synthetic-right-node"), budget())
        sender.publish(ReplicationWire.IDENTITY_PATH, JSONObject().put("version", 1).put("replica", a).toString().toByteArray(), emptyMap())
        assertEquals(a, receiver.identityFor("synthetic-left-node"))
        assertFalse(receiver.helloFor("synthetic-left-node")!!.compatible)
        assertNull(receiver.identityFor("synthetic-unavailable-node"))
        assertTrue(MemoryReplicationState().enrolledReplicas().isEmpty())
        assertTrue(mailbox.slots.values.single().assets.isEmpty())
        sender.publishIdentity(a, AssistantPeerRole.PHONE)
        assertTrue(receiver.helloFor("synthetic-left-node")!!.compatible)
        val hello = JSONObject(String(DataMapItem.fromDataItem(mailbox.events.last()).dataMap.getByteArray("frame")!!))
        assertEquals(setOf("version", "replica", "role"), hello.keys().asSequence().toSet())
        assertEquals(3, hello.getInt("version"))
        sender.publish(ReplicationWire.IDENTITY_PATH, JSONObject().put("version", 2).put("replica", a).put("installation", b)
            .put("role", "phone").toString().toByteArray(), emptyMap())
        assertFalse(receiver.helloFor("synthetic-left-node")!!.compatible)
    }

    @Test fun identityPublicationHasNoDeliveryNonceSoReconnectCannotCauseAChangeEventPingPong() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val sender = GmsReplicationTransport(mailbox.sdk("synthetic-phone-node"), budget())
        sender.publishIdentity(a, AssistantPeerRole.PHONE)
        val before = mailbox.events.last().data!!
        sender.publishIdentity(a, AssistantPeerRole.PHONE)
        assertArrayEquals(before, mailbox.events.last().data)
        assertEquals(setOf("frame"), DataMapItem.fromDataItem(mailbox.events.last()).dataMap.keySet())
    }

    @Test fun oversizedAssetAndLateSdkAssetAreReleasedWithoutImportOrAck() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val senderSdk = mailbox.sdk("synthetic-left-node")
        val receiverSdk = mailbox.sdk("synthetic-right-node")
        val record = record()
        val id = record.getJSONObject("operation").getJSONObject("id")
        val entry = JSONObject().put("id", id).put("conversation", id).put("tombstone", false).put("body_key", record.getString("body_key"))
        val state = MemoryReplicationState().apply { enrolled.add(a) }
        val request = ReplicationWire.frame(b, a, a, b, "record_request", JSONObject().put("entries", JSONArray().put(entry)).put("phase", "records").put("revision", "1"))
        state.save(a, JSONObject().put("session", a).put("phase", "records").put("revision", "1").put("pages", 1)
            .put("remaining", JSONArray()).put("next", JSONObject.NULL).put("pending", request))
        val target = FakeReplicationStore(b)
        val receiver = GmsReplicationTransport(receiverSdk, budget(10), regularFileIo())
        val controller = ReplicationController(target, FakeReplicationDomain(1), state, receiver, { a })
        val transfers = JSONArray().put(JSONObject().put("entry", entry).put("asset", "body_0").put("bytes", record.toString().toByteArray().size))
        val frame = ReplicationWire.frame(a, b, a, b, "records", JSONObject().put("records", transfers).put("unavailable", JSONArray()))
        val sender = GmsReplicationTransport(senderSdk, budget())
        val path = ReplicationWire.path(a, b, "records")
        sender.publish(path, ReplicationWire.encode(frame), mapOf("body_0" to ByteArray(4096)))
        assertFalse(receiver.receive(mailbox.events.last(), controller))
        assertEquals(1, mailbox.openedAssets); assertEquals(1, mailbox.releasedAssets)
        assertTrue(target.records.isEmpty())
        receiverSdk.deferAsset = true
        sender.publish(path, ReplicationWire.encode(frame), mapOf("body_0" to record.toString().toByteArray()))
        assertFalse(receiver.receive(mailbox.events.last(), controller))
        receiverSdk.completeAsset()
        assertEquals(2, mailbox.openedAssets); assertEquals(2, mailbox.releasedAssets)
        assertTrue(target.records.isEmpty())
        assertFalse(mailbox.slots.values.any { it.uri.lastPathSegment == "ack" })
    }

    private fun budget(timeout: Long = 5_000) = BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30), timeout)
    private fun record() = JSONObject().put("body_key", "1".repeat(64)).put("operation", JSONObject().put("version", 1)
        .put("id", JSONObject().put("replica", a).put("sequence", "1"))
        .put("body", JSONObject().put("kind", "conversation_created").put("title", "Synthetic SDK conversation")))
    private fun worker(block: () -> Unit) {
        val executor = Executors.newSingleThreadExecutor()
        try { executor.submit(block).get(60, TimeUnit.SECONDS) } finally { executor.shutdownNow() }
    }
    private fun regularFileIo() = object : AssetDescriptorIo {
        private val streams = mutableMapOf<FileDescriptor, FileInputStream>()
        // Robolectric Os.poll is a no-op. Synthetic regular files are always read-ready;
        // the production bounded read loop and SDK-owned descriptor release still execute.
        override fun poll(descriptor: FileDescriptor, timeoutMillis: Int) = 1
        override fun read(descriptor: FileDescriptor, buffer: ByteArray, length: Int): Int =
            streams.getOrPut(descriptor) { FileInputStream(descriptor) }.read(buffer, 0, length).coerceAtLeast(0)
    }
}

internal class FakeGmsMailbox(private val directory: File) {
    val slots = linkedMapOf<Uri, DataItem>()
    val events = mutableListOf<DataItem>()
    val nodes = linkedMapOf<String, Node>()
    val unreachableNodes = mutableSetOf<String>()
    val assets = linkedMapOf<String, ByteArray>()
    var openedAssets = 0
    var releasedAssets = 0
    fun sdk(node: String) = FakeSdk(node).also { nodes[node] = fakeNode(node) }
    fun response(bytes: ByteArray): DataClient.GetFdForAssetResponse {
        val file = File(directory, "synthetic-asset-${openedAssets++}")
        file.writeBytes(bytes)
        val descriptor = ParcelFileDescriptor.open(file, ParcelFileDescriptor.MODE_READ_ONLY)
        return object : DataClient.GetFdForAssetResponse() {
            override fun getFdForAsset() = descriptor
            override fun getInputStream(): InputStream = ParcelFileDescriptor.AutoCloseInputStream(descriptor)
            override fun release() { releasedAssets++; descriptor.close() }
        }
    }
    inner class FakeSdk(private val node: String) : GmsReplicationSdk {
        var capabilityError: Exception? = null
        var deleteError: Exception? = null
        var deferPut = false
        var deferAsset = false
        private var pending: Pair<TaskCompletionSource<DataItem>, DataItem>? = null
        private var pendingAsset: Pair<TaskCompletionSource<DataClient.GetFdForAssetResponse>, ByteArray>? = null
        val lookups = mutableListOf<Uri>()
        val calls = mutableListOf<String>()
        override fun capability(filter: Int): Task<CapabilityInfo> {
            calls.add("capability")
            return capabilityError?.let { Tasks.forException(it) } ?: Tasks.forResult(object : CapabilityInfo {
                override fun getName() = ReplicationWire.CAPABILITY
                override fun getNodes(): Set<Node> = this@FakeGmsMailbox.nodes.filterKeys {
                    it != node && (filter == CapabilityClient.FILTER_ALL || it !in unreachableNodes)
                }.values.toSet()
            })
        }
        override fun localNode(): Task<Node> { calls.add("localNode"); return Tasks.forResult(nodes.getValue(node)) }
        override fun getItem(uri: Uri): Task<DataItem> {
            calls.add("getItem")
            lookups.add(uri)
            return Tasks.forResult(slots[uri])
        }
        override fun putItem(request: PutDataRequest): Task<DataItem> {
            calls.add("putItem")
            val uri = request.uri.buildUpon().authority(node).build()
            val itemAssets = request.assets.mapValues { (key, asset) ->
                val id = "synthetic-asset-${assets.size}"
                assets[id] = asset.zza() ?: error("Synthetic Asset lacks bytes")
                object : DataItemAsset {
                    override fun getId() = id
                    override fun getDataItemKey() = key
                    override fun isDataValid() = true
                    override fun freeze(): DataItemAsset = this
                }
            }
            val item = object : DataItem {
                private var bytes = request.data
                override fun getUri() = uri
                override fun getData() = bytes
                override fun setData(data: ByteArray?): DataItem { bytes = data; return this }
                override fun getAssets() = itemAssets
                override fun isDataValid() = true
                override fun freeze(): DataItem = this
            }
            if (deferPut) return TaskCompletionSource<DataItem>().also { pending = it to item }.task
            stage(item)
            return Tasks.forResult(item)
        }
        fun completePut() { val (task, item) = pending!!; stage(item); task.setResult(item); pending = null }
        private fun stage(item: DataItem) { slots[item.uri] = item; events.add(item) }
        override fun deleteItems(uri: Uri, filter: Int): Task<Int> {
            calls.add("deleteItems")
            deleteError?.let { return Tasks.forException(it) }
            val removed = slots.keys.filter { it == uri || (filter == DataClient.FILTER_PREFIX && it.host == uri.host && it.path!!.startsWith(uri.path!!)) }
            removed.forEach(slots::remove)
            return Tasks.forResult(removed.size)
        }
        override fun asset(asset: Asset): Task<DataClient.GetFdForAssetResponse> {
            calls.add("asset")
            val bytes = assets.getValue(asset.digest!!)
            if (deferAsset) return TaskCompletionSource<DataClient.GetFdForAssetResponse>().also { pendingAsset = it to bytes }.task
            return Tasks.forResult(response(bytes))
        }
        fun completeAsset() { val (task, bytes) = pendingAsset!!; task.setResult(response(bytes)); pendingAsset = null }
    }
    private fun fakeNode(id: String) = object : Node {
        override fun getId() = id
        override fun getDisplayName() = "Synthetic SDK peer"
        override fun isNearby() = true
    }
}
