package dev.gitblame.voxgolem.assistant.replication

import android.net.Uri
import android.os.Parcel
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
import java.io.FileDescriptor
import java.io.FileInputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.nio.ByteBuffer
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class GmsReplicationTransportTest {
    @get:Rule val folder = TemporaryFolder()
    private val a = "00000000-0000-4000-8000-000000000001"
    private val b = "00000000-0000-4000-8000-000000000002"

    @Test fun exhaustedOrInterruptedBudgetDoesNotIssueSdkTask() = worker {
        var issued = 0
        val expired = BoundedGmsWait(System.nanoTime() - 1)
        assertThrows(TimeoutException::class.java) {
            expired.issue<Int> { issued++; Tasks.forResult(1) }
        }
        Thread.currentThread().interrupt()
        try {
            assertThrows(TimeoutException::class.java) {
                budget().issue<Int> { issued++; Tasks.forResult(1) }
            }
            assertTrue(Thread.currentThread().isInterrupted)
        } finally { Thread.interrupted() }
        assertEquals(0, issued)
    }

    @Test fun coalescedHintAndBidirectionalSessionStreamAssembleMultiChunkBatchAndCloseCleanly() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val leftSdk = mailbox.sdk("synthetic-left-node")
        val rightSdk = mailbox.sdk("synthetic-right-node")
        val leftTransport = GmsReplicationTransport(leftSdk, budget())
        val rightTransport = GmsReplicationTransport(rightSdk, budget())

        leftTransport.sendHint("synthetic-right-node", a, b)
        assertEquals(1, mailbox.messages.size)
        val hintMessage = mailbox.messages.single()
        assertEquals("synthetic-left-node", hintMessage.sourceNodeId)
        assertEquals("synthetic-right-node", hintMessage.targetNodeId)
        assertEquals(ReplicationWire.HINT_PATH, hintMessage.path)
        assertTrue(hintMessage.bytes.size <= ReplicationWire.MAX_HINT_BYTES)
        ReplicationWire.validateHint(hintMessage.bytes, b, a)

        val session = UUID(0, 77).toString()
        val token = UUID(0, 78).toString()
        val transfer = UUID(0, 79).toString()
        val largeRecord = JSONObject()
            .put("body_key", "2".repeat(64))
            .put(
                "operation",
                JSONObject()
                    .put("version", 2)
                    .put("id", JSONObject().put("replica", a).put("sequence", "1"))
                    .put("body", JSONObject().put("kind", "conversation_created").put("title", "z".repeat(140_000))),
            )
        val largeBytes = largeRecord.toString().toByteArray(Charsets.UTF_8)
        val largeEntry = ReplicationWire.descriptor(largeRecord)

        leftTransport.openSession("synthetic-right-node", session).use { initiator ->
            val incoming = mailbox.incomingChannels.removeFirst()
            assertEquals("synthetic-left-node", incoming.nodeId)
            assertEquals(ReplicationWire.sessionPath(session), incoming.path)
            rightTransport.acceptSession(incoming).use { responder ->
                ReplicationWire.writeRecordBatch(
                    connection = initiator,
                    sender = a,
                    target = b,
                    session = session,
                    token = token,
                    transfer = transfer,
                    bodies = listOf(PreparedRecordTransfer(largeEntry, largeBytes)),
                    unavailable = JSONArray(),
                )
                val firstFrame = responder.readFrame()
                val assembled = ReplicationWire.readRecordBatch(
                    connection = responder,
                    recordsFrame = firstFrame,
                    session = session,
                    local = b,
                    peer = a,
                    expectedEntries = JSONArray().put(largeEntry),
                    phase = "records",
                )
                assertEquals(1, assembled.records.length())
                assertEquals(largeRecord.toString(), assembled.records.getJSONObject(0).toString())
            }
            // Idempotent close must not double-close streams or channels.
            initiator.close()
        }
        assertEquals(2, mailbox.openedChannels)
        assertEquals(2, mailbox.closedChannels)
        assertEquals(4, mailbox.openedStreams)
        assertEquals(4, mailbox.closedStreams)
    }

    @Test fun sessionChannelEnforcesControlAndChunkSizeCapsAndCloseOnLateCompletionOrPartialOpenFailure() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val leftSdk = mailbox.sdk("synthetic-left-node")
        val rightSdk = mailbox.sdk("synthetic-right-node")
        val session = UUID(0, 88).toString()

        // 1. Late openChannel completion closes the late channel via closeChannel.
        leftSdk.deferOpenChannel = true
        assertThrows(TimeoutException::class.java) {
            GmsReplicationTransport(leftSdk, budget(10)).openSession("synthetic-right-node", session)
        }
        leftSdk.completeOpenChannel()
        assertEquals(1, mailbox.openedChannels)
        assertEquals(1, mailbox.closedChannels)
        leftSdk.deferOpenChannel = false
        mailbox.incomingChannels.clear()

        // 2. Late getInputStream completion closes the channel immediately and closes the late inputStream when it arrives.
        leftSdk.deferInputStream = true
        assertThrows(TimeoutException::class.java) {
            GmsReplicationTransport(leftSdk, budget(10)).openSession("synthetic-right-node", session)
        }
        assertEquals(2, mailbox.closedChannels)
        leftSdk.completeInputStream()
        assertEquals(1, mailbox.openedStreams)
        assertEquals(1, mailbox.closedStreams)
        leftSdk.deferInputStream = false
        mailbox.incomingChannels.clear()

        // 3. Partial open failure on getOutputStream closes already-opened inputStream, closes channel, and closes late outputStream.
        leftSdk.deferOutputStream = true
        assertThrows(TimeoutException::class.java) {
            GmsReplicationTransport(leftSdk, budget(10)).openSession("synthetic-right-node", session)
        }
        assertEquals(3, mailbox.closedChannels)
        assertEquals(2, mailbox.openedStreams) // step 2 inputStream + step 3 inputStream
        assertEquals(2, mailbox.closedStreams) // step 3 inputStream closed immediately on catch
        leftSdk.completeOutputStream()
        assertEquals(3, mailbox.openedStreams)
        assertEquals(3, mailbox.closedStreams)
        leftSdk.deferOutputStream = false
        mailbox.incomingChannels.clear()

        // 4. Oversized frame length (> 32 KiB) on the wire and oversized chunk write/read (> 64 KiB) reject.
        val leftTransport = GmsReplicationTransport(leftSdk, budget())
        val rightTransport = GmsReplicationTransport(rightSdk, budget())
        leftTransport.openSession("synthetic-right-node", session).use { initiator ->
            val incoming = mailbox.incomingChannels.removeFirst()
            rightTransport.acceptSession(incoming).use { responder ->
                assertThrows(IllegalArgumentException::class.java) {
                    initiator.writeBytes(ByteArray(ReplicationWire.MAX_CHUNK_BYTES + 1))
                }
                assertThrows(IllegalArgumentException::class.java) {
                    responder.readBytes(ReplicationWire.MAX_CHUNK_BYTES + 1)
                }
                // Inject raw oversized 4-byte frame length onto initiator's stream and verify readFrame rejects before allocating.
                initiator.writeBytes(ByteBuffer.allocate(4).putInt(ReplicationWire.MAX_CONTROL_BYTES + 1).array())
                assertThrows(IllegalArgumentException::class.java) {
                    responder.readFrame()
                }
            }
        }
    }

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
        leftTransport.cleanup(a, left, leftState)
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
    private fun record() = JSONObject().put("body_key", "1".repeat(64)).put("operation", JSONObject().put("version", 2)
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
    internal data class FakeMessage(val sourceNodeId: String, val targetNodeId: String, val path: String, val bytes: ByteArray)

    internal class InMemoryPipe(private val mailbox: FakeGmsMailbox) {
        private val lock = Object()
        private val chunks = ArrayDeque<ByteArray>()
        private var headOffset = 0
        private var closed = false
        private var inputCountedClosed = false
        private var outputCountedClosed = false

        fun openInput(): InputStream {
            synchronized(lock) { mailbox.openedStreams++ }
            return object : InputStream() {
                override fun read(): Int {
                    val single = ByteArray(1)
                    val count = read(single, 0, 1)
                    return if (count <= 0) -1 else single[0].toInt() and 0xff
                }

                override fun read(b: ByteArray, off: Int, len: Int): Int {
                    if (len == 0) return 0
                    synchronized(lock) {
                        while (chunks.isEmpty() && !closed) {
                            lock.wait(5_000)
                            if (chunks.isEmpty() && !closed) break
                        }
                        val head = chunks.peekFirst() ?: return -1
                        val count = minOf(len, head.size - headOffset)
                        System.arraycopy(head, headOffset, b, off, count)
                        headOffset += count
                        if (headOffset == head.size) {
                            chunks.removeFirst()
                            headOffset = 0
                        }
                        return count
                    }
                }

                override fun close() {
                    synchronized(lock) {
                        if (!inputCountedClosed) {
                            inputCountedClosed = true
                            mailbox.closedStreams++
                        }
                        closed = true
                        lock.notifyAll()
                    }
                }
            }
        }

        fun openOutput(): OutputStream {
            synchronized(lock) { mailbox.openedStreams++ }
            return object : OutputStream() {
                override fun write(b: Int) = write(byteArrayOf(b.toByte()), 0, 1)

                override fun write(b: ByteArray, off: Int, len: Int) {
                    if (len == 0) return
                    synchronized(lock) {
                        if (closed) throw IOException("Replication channel closed")
                        chunks.addLast(b.copyOfRange(off, off + len))
                        lock.notifyAll()
                    }
                }

                override fun close() {
                    synchronized(lock) {
                        if (!outputCountedClosed) {
                            outputCountedClosed = true
                            mailbox.closedStreams++
                        }
                        closed = true
                        lock.notifyAll()
                    }
                }
            }
        }

        fun closeBoth() {
            synchronized(lock) {
                closed = true
                lock.notifyAll()
            }
        }
    }

    internal class FakeChannel(
        private val peerNodeId: String,
        private val channelPath: String,
        val inboundPipe: InMemoryPipe,
        val outboundPipe: InMemoryPipe,
        private val mailbox: FakeGmsMailbox,
    ) : ChannelClient.Channel {
        var peerChannel: FakeChannel? = null
        private var closed = false

        override fun getNodeId(): String = peerNodeId
        override fun getPath(): String = channelPath
        override fun describeContents(): Int = 0
        override fun writeToParcel(dest: Parcel, flags: Int) = Unit

        fun markClosed() {
            synchronized(mailbox) {
                if (!closed) {
                    closed = true
                    mailbox.closedChannels++
                    inboundPipe.closeBoth()
                    outboundPipe.closeBoth()
                }
            }
        }
    }

    val slots = linkedMapOf<Uri, DataItem>()
    val events = mutableListOf<DataItem>()
    val messages = mutableListOf<FakeMessage>()
    val incomingChannels = ArrayDeque<FakeChannel>()
    val nodes = linkedMapOf<String, Node>()
    val unreachableNodes = mutableSetOf<String>()
    val assets = linkedMapOf<String, ByteArray>()
    var openedAssets = 0
    var releasedAssets = 0
    var openedChannels = 0
    var closedChannels = 0
    var openedStreams = 0
    var closedStreams = 0
    var onChannelOpened: ((String, FakeChannel) -> Unit)? = null

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
        var sendMessageError: Exception? = null
        var openChannelError: Exception? = null
        var inputStreamError: Exception? = null
        var outputStreamError: Exception? = null
        var deferPut = false
        var deferAsset = false
        var deferOpenChannel = false
        var deferInputStream = false
        var deferOutputStream = false
        private var pending: Pair<TaskCompletionSource<DataItem>, DataItem>? = null
        private var pendingAsset: Pair<TaskCompletionSource<DataClient.GetFdForAssetResponse>, ByteArray>? = null
        private var pendingOpenChannel: Pair<TaskCompletionSource<ChannelClient.Channel>, FakeChannel>? = null
        private var pendingInputStream: Pair<TaskCompletionSource<InputStream>, FakeChannel>? = null
        private var pendingOutputStream: Pair<TaskCompletionSource<OutputStream>, FakeChannel>? = null
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

        override fun sendMessage(nodeId: String, path: String, bytes: ByteArray): Task<Int> {
            calls.add("sendMessage")
            sendMessageError?.let { return Tasks.forException(it) }
            messages.add(FakeMessage(node, nodeId, path, bytes.copyOf()))
            return Tasks.forResult(messages.size)
        }

        override fun openChannel(nodeId: String, path: String): Task<ChannelClient.Channel> {
            calls.add("openChannel")
            openChannelError?.let { return Tasks.forException(it) }
            val pipeAtoB = InMemoryPipe(this@FakeGmsMailbox)
            val pipeBtoA = InMemoryPipe(this@FakeGmsMailbox)
            val initiator = FakeChannel(nodeId, path, pipeBtoA, pipeAtoB, this@FakeGmsMailbox)
            val responder = FakeChannel(node, path, pipeAtoB, pipeBtoA, this@FakeGmsMailbox)
            initiator.peerChannel = responder
            responder.peerChannel = initiator
            if (deferOpenChannel) {
                openedChannels++
                return TaskCompletionSource<ChannelClient.Channel>().also { pendingOpenChannel = it to initiator }.task
            }
            openedChannels += 2
            incomingChannels.addLast(responder)
            onChannelOpened?.invoke(nodeId, responder)
            return Tasks.forResult(initiator)
        }

        fun completeOpenChannel() {
            val (task, channel) = pendingOpenChannel!!
            pendingOpenChannel = null
            task.setResult(channel)
        }

        override fun getInputStream(channel: ChannelClient.Channel): Task<InputStream> {
            calls.add("getInputStream")
            inputStreamError?.let { return Tasks.forException(it) }
            val fake = channel as FakeChannel
            if (deferInputStream) {
                return TaskCompletionSource<InputStream>().also { pendingInputStream = it to fake }.task
            }
            return Tasks.forResult(fake.inboundPipe.openInput())
        }

        fun completeInputStream() {
            val (task, fake) = pendingInputStream!!
            pendingInputStream = null
            task.setResult(fake.inboundPipe.openInput())
        }

        override fun getOutputStream(channel: ChannelClient.Channel): Task<OutputStream> {
            calls.add("getOutputStream")
            outputStreamError?.let { return Tasks.forException(it) }
            val fake = channel as FakeChannel
            if (deferOutputStream) {
                return TaskCompletionSource<OutputStream>().also { pendingOutputStream = it to fake }.task
            }
            return Tasks.forResult(fake.outboundPipe.openOutput())
        }

        fun completeOutputStream() {
            val (task, fake) = pendingOutputStream!!
            pendingOutputStream = null
            task.setResult(fake.outboundPipe.openOutput())
        }

        override fun closeChannel(channel: ChannelClient.Channel): Task<Void> {
            calls.add("closeChannel")
            (channel as? FakeChannel)?.markClosed()
            return Tasks.forResult(null)
        }
    }
    private fun fakeNode(id: String) = object : Node {
        override fun getId() = id
        override fun getDisplayName() = "Synthetic SDK peer"
        override fun isNearby() = true
    }
}
