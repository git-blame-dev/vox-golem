package dev.gitblame.voxgolem.assistant.replication

import android.net.Uri
import android.os.Parcel
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

    @Test fun publicIdentityMetadataDoesNotEnrollAndLegacyOrUnknownVersionsAreNotCompatible() = worker {
        val mailbox = FakeGmsMailbox(folder.root)
        val senderSdk = mailbox.sdk("synthetic-left-node")
        val sender = GmsReplicationTransport(senderSdk, budget())
        val receiver = GmsReplicationTransport(mailbox.sdk("synthetic-right-node"), budget())
        fun putRawIdentity(payload: JSONObject) {
            val request = PutDataMapRequest.create(ReplicationWire.IDENTITY_PATH)
            request.dataMap.putByteArray("frame", payload.toString().toByteArray(Charsets.UTF_8))
            senderSdk.putItem(request.asPutDataRequest().setUrgent())
        }
        putRawIdentity(JSONObject().put("version", 1).put("replica", a))
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
        putRawIdentity(JSONObject().put("version", 2).put("replica", a).put("installation", b).put("role", "phone"))
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

    private fun budget(timeout: Long = 5_000) = BoundedGmsWait(System.nanoTime() + TimeUnit.SECONDS.toNanos(30), timeout)
    private fun worker(block: () -> Unit) {
        val executor = Executors.newSingleThreadExecutor()
        try { executor.submit(block).get(60, TimeUnit.SECONDS) } finally { executor.shutdownNow() }
    }
}

internal class FakeGmsMailbox(@Suppress("UNUSED_PARAMETER") directory: File = File(".")) {
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
    var openedChannels = 0
    var closedChannels = 0
    var openedStreams = 0
    var closedStreams = 0
    var onChannelOpened: ((String, FakeChannel) -> Unit)? = null

    fun sdk(node: String) = FakeSdk(node).also { nodes[node] = fakeNode(node) }

    inner class FakeSdk(private val node: String) : GmsReplicationSdk {
        var capabilityError: Exception? = null
        var sendMessageError: Exception? = null
        var openChannelError: Exception? = null
        var inputStreamError: Exception? = null
        var outputStreamError: Exception? = null
        var deferPut = false
        var deferOpenChannel = false
        var deferInputStream = false
        var deferOutputStream = false
        private var pending: Pair<TaskCompletionSource<DataItem>, DataItem>? = null
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
            val item = object : DataItem {
                private var bytes = request.data
                override fun getUri() = uri
                override fun getData() = bytes
                override fun setData(data: ByteArray?): DataItem { bytes = data; return this }
                override fun getAssets(): Map<String, DataItemAsset> = emptyMap()
                override fun isDataValid() = true
                override fun freeze(): DataItem = this
            }
            if (deferPut) return TaskCompletionSource<DataItem>().also { pending = it to item }.task
            stage(item)
            return Tasks.forResult(item)
        }
        fun completePut() { val (task, item) = pending!!; stage(item); task.setResult(item); pending = null }
        private fun stage(item: DataItem) { slots[item.uri] = item; events.add(item) }

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
