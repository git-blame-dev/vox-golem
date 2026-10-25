package dev.gitblame.voxgolem.assistant.replication

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.EOFException
import java.util.ArrayDeque

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ReplicationControllerTest {
    private val a = "00000000-0000-4000-8000-000000000001"
    private val b = "00000000-0000-4000-8000-000000000002"

    @Test fun discoveryNeverEnrollsAndWrongNodeVersionTargetOrSessionCannotImportOrAccept() {
        val pair = PairHarness(a, b)
        pair.left.reconcile()
        assertEquals(0, pair.openedSessions)
        assertTrue(pair.hints.isEmpty())

        pair.enroll()
        assertFalse(pair.right.connect("wrong-node"))
        assertFalse(pair.right.connect("node-a", expectedPeer = b))

        // Invalid node, invalid session UUID, or mismatched session path must reject in accept() and always close connection.
        val validSession = "00000000-0000-4000-8000-000000000099"
        for ((nodeId, sessionId, path) in listOf(
            Triple("wrong-node", validSession, ReplicationWire.sessionPath(validSession)),
            Triple("node-a", "not-a-uuid", "/voxgolem/assistant/v2/session/not-a-uuid"),
            Triple("node-a", validSession, ReplicationWire.sessionPath(b)),
        )) {
            val pipe = InMemorySessionPipe(validSession)
            val endpoint = pipe.endpoint("node-b", nodeId, sessionId = sessionId, sessionPath = path)
            assertFalse(pair.right.accept(endpoint))
            assertTrue(endpoint.closedEndpoint)
        }

        // Invalid hints (wrong node, wrong version, wrong target, oversized) must reject.
        val validHint = ReplicationWire.encodeHint(a, b)
        assertFalse(pair.right.onHint("wrong-node", validHint))
        assertFalse(
            pair.right.onHint(
                "node-a",
                JSONObject().put("version", 2).put("kind", "reconcile_hint").put("sender", a).put("target", b).toString().toByteArray(),
            )
        )
        assertFalse(
            pair.right.onHint(
                "node-a",
                JSONObject().put("version", 3).put("kind", "reconcile_hint").put("sender", a).put("target", a).toString().toByteArray(),
            )
        )
        assertFalse(pair.right.onHint("node-a", ByteArray(ReplicationWire.MAX_HINT_BYTES + 1)))

        // Tampered session frames (wrong version, wrong target, wrong session) must reject without importing.
        pair.leftStore.add(record(a, "1"))
        for (tamper in listOf<(JSONObject) -> Unit>(
            { it.put("version", 1) },
            { it.put("target", a) },
            { it.put("session", b) },
        )) {
            pair.tamperNextFrame = tamper
            assertFalse(pair.right.connect("node-a", a))
            assertTrue(pair.rightStore.imports.isEmpty())
        }
        pair.tamperNextFrame = null

        // Watch role sends a <= 256-byte coalesced hint rather than opening a channel; Phone onHint opens session and converges both ways.
        val watchPhone = PairHarness(a, b, leftRole = AssistantPeerRole.PHONE, rightRole = AssistantPeerRole.WATCH)
        watchPhone.enroll()
        watchPhone.leftStore.add(record(a, "1"))
        watchPhone.rightStore.add(record(b, "1"))
        watchPhone.right.reconcile()
        assertEquals(0, watchPhone.openedSessions)
        assertEquals(1, watchPhone.hints.size)
        val hint = watchPhone.hints.removeAt(0)
        assertTrue(hint.bytes.size <= ReplicationWire.MAX_HINT_BYTES)
        assertTrue(watchPhone.left.onHint("node-b", hint.bytes))
        assertEquals(1, watchPhone.openedSessions)
        assertEquals(setOf("$a/1", "$b/1"), watchPhone.leftStore.records.keys)
        assertEquals(setOf("$a/1", "$b/1"), watchPhone.rightStore.records.keys)
        assertEquals(setOf("$a/1", "$b/1"), watchPhone.leftStore.acknowledged[b])
        assertEquals(setOf("$a/1", "$b/1"), watchPhone.rightStore.acknowledged[a])
    }

    @Test fun exactGapsDuplicateAndReorderedFramesConvergeWithoutProviderExecution() {
        val pair = PairHarness(a, b)
        pair.enroll()
        pair.leftStore.add(record(a, "1"))
        pair.leftStore.add(record(a, "9"))
        pair.right.reconcile()
        pair.drain(duplicate = true, reverse = true)
        assertEquals(setOf("$a/1", "$a/9"), pair.rightStore.records.keys)
        assertEquals(setOf("$a/1", "$a/9"), pair.leftStore.acknowledged[b])
        assertEquals(2, pair.rightStore.records.size)
        assertNull(pair.leftState.load(b))
        assertNull(pair.rightState.load(a))
    }

    @Test fun tombstonesAcrossPagesAreCommittedBeforeAnyContentRequest() {
        val pair = PairHarness(a, b, pageSize = 1)
        pair.enroll()
        pair.leftStore.add(record(a, "1"))
        pair.leftStore.add(record(a, "8", tombstone = true))
        pair.right.reconcile()
        pair.drain()
        assertEquals("conversation_deleted", pair.rightStore.imports.first())
        assertFalse(pair.rightStore.imports.contains("conversation_created"))
    }

    @Test fun failedStoreCommitNeverAcknowledgesAndRestartRetriesTheSameExactBody() {
        val pair = PairHarness(a, b)
        pair.enroll()
        pair.leftStore.add(record(a, "1"))
        pair.rightStore.failCommit = true
        pair.right.reconcile()
        pair.drain()
        assertTrue(pair.leftStore.acknowledged.isEmpty())
        assertTrue(pair.rightStore.records.isEmpty())
        pair.rightStore.failCommit = false
        pair.restartRight()
        pair.right.reconcile()
        pair.drain()
        assertEquals(setOf("$a/1"), pair.leftStore.acknowledged[b])
        assertEquals(setOf("$a/1"), pair.rightStore.records.keys)
    }

    @Test fun crashAfterCommitBeforeAckAndThirtyMinuteDisconnectRecoverFromCommittedTruth() {
        val pair = PairHarness(a, b)
        pair.enroll()
        pair.leftStore.add(record(a, "1"))
        // Synthetic disconnect after durable SQLite commit before ACK frame reaches sender.
        pair.dropNextFrameKind = "ack"
        pair.right.reconcile()
        pair.drain()
        assertEquals(setOf("$a/1"), pair.rightStore.records.keys)
        assertTrue(pair.leftStore.acknowledged.isEmpty())
        assertNull(pair.leftState.load(b))
        assertNull(pair.rightState.load(a))

        // Synthetic disconnect after sender processes ACK before ack_confirmed reaches receiver.
        pair.dropNextFrameKind = "ack_confirmed"
        pair.restartRight()
        pair.right.reconcile()
        pair.drain()
        assertNull(pair.leftState.load(b))
        assertNull(pair.rightState.load(a))

        // Next connected session recovers strictly from canonical store truth.
        pair.restartRight()
        pair.right.reconcile()
        pair.drain()
        assertEquals(setOf("$a/1"), pair.leftStore.acknowledged[b])
        assertEquals(setOf("$a/1"), pair.rightStore.acknowledged[a])

        pair.online = false
        pair.leftStore.add(record(a, "3"))
        pair.rightStore.add(record(b, "7"))
        val sessionsBeforeOffline = pair.openedSessions
        pair.left.reconcile()
        pair.right.reconcile()
        assertEquals(sessionsBeforeOffline, pair.openedSessions)

        // No wall-clock TTL: the next reachable/resume trigger works after any interval.
        pair.online = true
        pair.left.reconcile()
        pair.right.reconcile()
        pair.drain(duplicate = true)
        assertEquals(pair.leftStore.records.keys, pair.rightStore.records.keys)
        assertEquals(pair.leftStore.records.keys, pair.leftStore.acknowledged[b])
        assertEquals(pair.rightStore.records.keys, pair.rightStore.acknowledged[a])
        assertNull(pair.leftState.load(b))
        assertNull(pair.rightState.load(a))
    }

    @Test fun forgedBodyAssetOversizeAndQuarantinedServingAreRejected() {
        val pair = PairHarness(a, b)
        pair.enroll()
        val original = record(a, "1")
        pair.leftStore.add(original)

        // 1. Forged body chunk (altered body_key inside record JSON) rejects without import or ack.
        pair.forgeNextBodyChunk = true
        assertFalse(pair.right.connect("node-a", a))
        assertTrue(pair.rightStore.records.isEmpty())
        assertTrue(pair.leftStore.acknowledged.isEmpty())

        // 2. Truncated stream before batch_end rejects without import or ack.
        pair.dropNextFrameKind = "batch_end"
        assertFalse(pair.right.connect("node-a", a))
        assertTrue(pair.rightStore.records.isEmpty())
        assertTrue(pair.leftStore.acknowledged.isEmpty())

        // 3. Oversized body_chunk declaration (> 64 KiB) rejects without import or ack.
        pair.tamperNextFrame = { frame ->
            if (frame.getString("kind") == "body_chunk") {
                frame.getJSONObject("payload").put("bytes", ReplicationWire.MAX_CHUNK_BYTES + 1)
            }
        }
        assertFalse(pair.right.connect("node-a", a))
        pair.tamperNextFrame = null
        assertTrue(pair.rightStore.records.isEmpty())
        assertTrue(pair.leftStore.acknowledged.isEmpty())

        // 4. Quarantined records are never served or imported.
        pair.leftStore.quarantined.add("$a/1")
        pair.leftStore.filterPublicationsByQuarantine = false
        assertEquals(1, pair.leftStore.publications(b, null, 128).length())
        pair.left.reconcile()
        pair.right.reconcile()
        pair.drain()
        assertTrue(pair.rightStore.records.isEmpty())
    }

    @Test fun threeEnrolledReplicasConvergeUnderSeededLossReorderDuplicatesAndOfflinePeer() {
        val replicas = listOf(a, b, "00000000-0000-4000-8000-000000000003")
        val stores = replicas.associateWith { FakeReplicationStore(it).apply { add(record(it, "1")); add(record(it, "9")) } }
        val states = replicas.associateWith { replica -> MemoryReplicationState().apply { enrolled.addAll(replicas.filter { it != replica }) } }
        var online = replicas.take(2).toSet()
        val random = kotlin.random.Random(7123)
        var currentRound = 0
        val controllers = mutableMapOf<String, ReplicationController>()
        for (replica in replicas) {
            controllers[replica] = ReplicationController(
                stores.getValue(replica),
                FakeReplicationDomain(2),
                states.getValue(replica),
                object : ReplicationTransport {
                    override fun reachable() = if (replica !in online) emptyList() else online.filter { it != replica }.map { PeerNode("synthetic-$it", "Synthetic peer") }
                    override fun publish(path: String, bytes: ByteArray, assets: Map<String, ByteArray>) = Unit
                    override fun openSession(nodeId: String, session: String): ReplicationSessionConnection {
                        val targetReplica = replicas.first { nodeId == "synthetic-$it" }
                        check(replica in online && targetReplica in online) { "Offline peer cannot open session" }
                        val pipe = InMemorySessionPipe(
                            session = session,
                            onFrame = { _, frame ->
                                if (currentRound < 5 && frame.getString("kind") in setOf("records", "ack") && random.nextInt(5) == 0) {
                                    throw EOFException("Synthetic seeded mid-session disconnect")
                                }
                            },
                        )
                        val responderConn = pipe.endpoint("synthetic-$targetReplica", "synthetic-$replica")
                        val responderThread = Thread {
                            controllers.getValue(targetReplica).accept(responderConn)
                        }.apply { isDaemon = true; start() }
                        return pipe.endpoint("synthetic-$replica", "synthetic-$targetReplica") {
                            responderThread.join(5_000)
                        }
                    }
                },
                { node -> replicas.firstOrNull { node == "synthetic-$it" } },
            )
        }
        repeat(10) { round ->
            currentRound = round
            if (round == 3) online = replicas.toSet()
            for (replica in replicas.shuffled(random)) {
                if (replica in online) {
                    controllers.getValue(replica).reconcile()
                    if (random.nextBoolean()) controllers.getValue(replica).reconcile()
                }
            }
        }
        assertTrue(stores.values.all { it.records.keys == stores.getValue(a).records.keys })
        assertEquals(6, stores.getValue(a).records.size)
        for (source in stores.values) for (peer in replicas.filter { it != source.replica }) {
            assertEquals(source.records.keys, source.acknowledged[peer])
            assertNull(states.getValue(source.replica).load(peer))
        }
    }

    private fun record(replica: String, sequence: String, tombstone: Boolean = false): JSONObject = JSONObject()
        .put("body_key", (if (tombstone) "d" else sequence.takeLast(1)).repeat(64))
        .put("operation", JSONObject().put("version", 2).put("id", id(replica, sequence))
            .put("body", if (tombstone) JSONObject().put("kind", "conversation_deleted").put("conversation", id(replica, "1"))
                else JSONObject().put("kind", "conversation_created").put("title", "Synthetic conversation")))

    private fun id(replica: String, sequence: String) = JSONObject().put("replica", replica).put("sequence", sequence)
}

internal class InMemorySessionPipe(
    val session: String,
    private val onFrame: (senderNode: String, frame: JSONObject) -> Unit = { _, _ -> },
    private val transformChunk: (senderNode: String, bytes: ByteArray) -> ByteArray = { _, bytes -> bytes },
) {
    private val lock = Object()
    private val aToBFrames = ArrayDeque<JSONObject>()
    private val bToAFrames = ArrayDeque<JSONObject>()
    private val aToBChunks = ArrayDeque<ByteArray>()
    private val bToAChunks = ArrayDeque<ByteArray>()
    private var closed = false

    fun endpoint(
        localNode: String,
        peerNode: String,
        sessionId: String = session,
        sessionPath: String = ReplicationWire.sessionPath(session),
        onClose: () -> Unit = {},
    ): ConnectedEndpoint {
        val first = localNode < peerNode
        return ConnectedEndpoint(
            nodeId = peerNode,
            path = sessionPath,
            session = sessionId,
            localNode = localNode,
            outboundFrames = if (first) aToBFrames else bToAFrames,
            inboundFrames = if (first) bToAFrames else aToBFrames,
            outboundChunks = if (first) aToBChunks else bToAChunks,
            inboundChunks = if (first) bToAChunks else aToBChunks,
            onClose = onClose,
        )
    }

    fun closePipe() {
        synchronized(lock) {
            closed = true
            lock.notifyAll()
        }
    }

    inner class ConnectedEndpoint(
        override val nodeId: String,
        override val path: String,
        override val session: String,
        private val localNode: String,
        private val outboundFrames: ArrayDeque<JSONObject>,
        private val inboundFrames: ArrayDeque<JSONObject>,
        private val outboundChunks: ArrayDeque<ByteArray>,
        private val inboundChunks: ArrayDeque<ByteArray>,
        private val onClose: () -> Unit,
    ) : ReplicationSessionConnection {
        var closedEndpoint = false
            private set

        override fun readFrame(): JSONObject {
            synchronized(lock) {
                while (inboundFrames.isEmpty() && !closed) {
                    lock.wait(5_000)
                    if (inboundFrames.isEmpty() && !closed) break
                }
                if (inboundFrames.isEmpty()) throw EOFException("Replication session closed")
                return inboundFrames.removeFirst()
            }
        }

        override fun writeFrame(frame: JSONObject) {
            val decoded = ReplicationWire.decode(ReplicationWire.encode(frame))
            synchronized(lock) {
                if (closed) throw EOFException("Replication session closed")
                try {
                    onFrame(localNode, decoded)
                } catch (error: Exception) {
                    closed = true
                    lock.notifyAll()
                    throw error
                }
                outboundFrames.addLast(decoded)
                lock.notifyAll()
            }
        }

        override fun readBytes(length: Int): ByteArray {
            require(length in 1..ReplicationWire.MAX_CHUNK_BYTES)
            synchronized(lock) {
                while (inboundChunks.isEmpty() && !closed) {
                    lock.wait(5_000)
                    if (inboundChunks.isEmpty() && !closed) break
                }
                if (inboundChunks.isEmpty()) throw EOFException("Replication session closed")
                val chunk = inboundChunks.removeFirst()
                require(chunk.size == length)
                return chunk
            }
        }

        override fun writeBytes(bytes: ByteArray, offset: Int, length: Int) {
            require(length in 1..ReplicationWire.MAX_CHUNK_BYTES)
            val slice = bytes.copyOfRange(offset, offset + length)
            synchronized(lock) {
                if (closed) throw EOFException("Replication session closed")
                val transformed = try {
                    transformChunk(localNode, slice)
                } catch (error: Exception) {
                    closed = true
                    lock.notifyAll()
                    throw error
                }
                require(transformed.size in 1..ReplicationWire.MAX_CHUNK_BYTES)
                outboundChunks.addLast(transformed)
                lock.notifyAll()
            }
        }

        override fun close() {
            closedEndpoint = true
            closePipe()
            onClose()
        }
    }
}

internal class MemoryReplicationState : ReplicationState {
    val enrolled = mutableSetOf<String>()
    private val sessions = mutableMapOf<String, String>()
    override fun enrolledReplicas() = enrolled.toSet()
    override fun load(peer: String) = sessions[peer]?.let(::JSONObject)
    override fun save(peer: String, state: JSONObject?) { if (state == null) sessions.remove(peer) else sessions[peer] = state.toString() }
}

internal class FakeReplicationStore(override val replica: String) : ReplicationStore {
    val records = linkedMapOf<String, JSONObject>()
    val imports = mutableListOf<String>()
    val acknowledged = mutableMapOf<String, MutableSet<String>>()
    val quarantined = mutableSetOf<String>()
    var failCommit = false
    var filterPublicationsByQuarantine = true
    var revision = 0
    fun add(record: JSONObject) { records[key(record.getJSONObject("operation").getJSONObject("id"))] = record; revision++ }
    override fun snapshot() = JSONObject().put("version", 2).put("revision", revision.toString())
        .put("records", JSONArray(records.values.toList())).put("retired", JSONArray())
        .put("quarantine", JSONArray(quarantined.map { JSONObject().put("id", parseId(it)) }))
    override fun publications(peer: String, after: JSONObject?, limit: Int) = JSONArray(records.filter {
        (!filterPublicationsByQuarantine || it.key !in quarantined) && it.key !in (acknowledged[peer] ?: emptySet<String>()) && (after == null || it.key > key(after))
    }.values.take(limit))
    override fun importRecords(input: JSONArray) {
        check(!failCommit) { "Synthetic commit failure" }
        for (i in 0 until input.length()) {
            val record = input.getJSONObject(i)
            val operation = record.getJSONObject("operation")
            val key = key(operation.getJSONObject("id"))
            val previous = records[key]
            require(previous == null || previous.toString() == record.toString())
            if (previous == null) { imports.add(operation.getJSONObject("body").getString("kind")); add(record) }
        }
    }
    override fun acknowledge(peer: String, receipts: JSONArray) {
        for (i in 0 until receipts.length()) {
            val receipt = receipts.getJSONObject(i)
            val key = key(receipt.getJSONObject("operation"))
            require(receipt.getString("recipient_replica") == peer && records[key]?.getString("body_key") == receipt.getString("body_key"))
            acknowledged.getOrPut(peer) { mutableSetOf() }.add(key)
        }
    }
    companion object {
        fun key(id: JSONObject) = id.getString("replica") + "/" + id.getString("sequence")
        fun parseId(key: String) = JSONObject().put("replica", key.substringBefore('/')).put("sequence", key.substringAfter('/'))
    }
}

internal class FakeReplicationDomain(private val pageSize: Int) : ReplicationDomain {
    override fun inventory(snapshot: JSONObject, cursor: JSONObject): JSONObject {
        require(cursor.getString("revision") == snapshot.getString("revision"))
        val quarantined = snapshot.getJSONArray("quarantine").objects().map { FakeReplicationStore.key(it.getJSONObject("id")) }.toSet()
        val entries = snapshot.getJSONArray("records").objects().map(::entry).filter { FakeReplicationStore.key(it.getJSONObject("id")) !in quarantined }
            .sortedWith { a, b -> compareId(a.getJSONObject("id"), b.getJSONObject("id")) }
            .filter { cursor.isNull("after") || compareId(it.getJSONObject("id"), cursor.getJSONObject("after")) > 0 }
        val page = entries.take(pageSize)
        return JSONObject().put("version", 2).put("revision", snapshot.getString("revision")).put("entries", JSONArray(page))
            .put("next", if (entries.size > page.size) JSONObject().put("revision", snapshot.getString("revision")).put("after", page.last().getJSONObject("id")) else JSONObject.NULL)
    }
    override fun reconcile(snapshot: JSONObject, page: JSONObject): JSONObject {
        val local = snapshot.getJSONArray("records").objects().associateBy { FakeReplicationStore.key(it.getJSONObject("operation").getJSONObject("id")) }
        val deleted = local.values.filter { it.getJSONObject("operation").getJSONObject("body").getString("kind") == "conversation_deleted" }
            .map { FakeReplicationStore.key(it.getJSONObject("operation").getJSONObject("body").getJSONObject("conversation")) }.toSet()
        val entries = page.getJSONArray("entries").objects()
        entries.forEach { incoming -> local[FakeReplicationStore.key(incoming.getJSONObject("id"))]?.let { require(it.getString("body_key") == incoming.getString("body_key")) } }
        return JSONObject().put("fetch_tombstones", JSONArray(entries.filter { it.getBoolean("tombstone") && FakeReplicationStore.key(it.getJSONObject("id")) !in local }))
            .put("fetch_records", JSONArray(entries.filter { !it.getBoolean("tombstone") && FakeReplicationStore.key(it.getJSONObject("id")) !in local && FakeReplicationStore.key(it.getJSONObject("conversation")) !in deleted }))
            .put("offer_tombstones", JSONArray()).put("missing_ancestors", JSONArray())
    }
    override fun receipts(snapshot: JSONObject, replica: String, ids: JSONArray): JSONArray {
        val records = snapshot.getJSONArray("records").objects().associateBy { FakeReplicationStore.key(it.getJSONObject("operation").getJSONObject("id")) }
        return JSONArray(ids.objects().map { id -> JSONObject().put("version", 2).put("recipient_replica", replica).put("operation", id)
            .put("body_key", records.getValue(FakeReplicationStore.key(id)).getString("body_key")) })
    }
    private fun entry(record: JSONObject): JSONObject {
        val op = record.getJSONObject("operation")
        val body = op.getJSONObject("body")
        return JSONObject().put("id", op.getJSONObject("id")).put("body_key", record.getString("body_key"))
            .put("conversation", if (body.getString("kind") == "conversation_created") op.getJSONObject("id") else body.getJSONObject("conversation"))
            .put("tombstone", body.getString("kind") == "conversation_deleted")
    }
    private fun compareId(a: JSONObject, b: JSONObject): Int {
        val replica = a.getString("replica").compareTo(b.getString("replica"))
        return if (replica != 0) replica else a.getString("sequence").compareTo(b.getString("sequence"))
    }
}

internal class PairHarness(
    private val a: String,
    private val b: String,
    private val pageSize: Int = 63,
    private val leftRole: AssistantPeerRole = AssistantPeerRole.PHONE,
    private val rightRole: AssistantPeerRole = AssistantPeerRole.PHONE,
) {
    data class HintPacket(val senderNode: String, val targetNode: String, val bytes: ByteArray)
    val hints = mutableListOf<HintPacket>()
    val leftState = MemoryReplicationState()
    val rightState = MemoryReplicationState()
    val leftStore = FakeReplicationStore(a)
    val rightStore = FakeReplicationStore(b)
    var online = true
    var openedSessions = 0
    var dropNextFrameKind: String? = null
    var forgeNextBodyChunk = false
    var tamperNextFrame: ((JSONObject) -> Unit)? = null
    val left: ReplicationController = controller(a, leftStore, leftState, leftRole)
    var right: ReplicationController = controller(b, rightStore, rightState, rightRole)
    fun enroll() { leftState.enrolled.add(b); rightState.enrolled.add(a) }
    fun restartRight() { right = controller(b, rightStore, rightState, rightRole) }
    private fun controller(
        replica: String,
        store: FakeReplicationStore,
        state: MemoryReplicationState,
        role: AssistantPeerRole,
    ): ReplicationController = ReplicationController(
        store,
        FakeReplicationDomain(pageSize),
        state,
        object : ReplicationTransport {
            override fun reachable() = if (!online) emptyList() else listOf(PeerNode(if (replica == a) "node-b" else "node-a", "Synthetic peer"))
            override fun publish(path: String, bytes: ByteArray, assets: Map<String, ByteArray>) = Unit
            override fun sendHint(nodeId: String, sender: String, target: String) {
                val senderNode = if (replica == a) "node-a" else "node-b"
                hints.add(HintPacket(senderNode, nodeId, ReplicationWire.encodeHint(sender, target)))
            }
            override fun openSession(nodeId: String, session: String): ReplicationSessionConnection {
                check(online) { "Offline transport cannot open session" }
                openedSessions++
                val senderNode = if (replica == a) "node-a" else "node-b"
                val responderController: ReplicationController = if (nodeId == "node-a") left else right
                val pipe = InMemorySessionPipe(
                    session = session,
                    onFrame = { _, frame ->
                        tamperNextFrame?.invoke(frame)
                        if (dropNextFrameKind != null && frame.getString("kind") == dropNextFrameKind) {
                            val dropped = dropNextFrameKind
                            dropNextFrameKind = null
                            throw EOFException("Synthetic dropped frame: $dropped")
                        }
                    },
                    transformChunk = { _, chunk ->
                        if (forgeNextBodyChunk) {
                            forgeNextBodyChunk = false
                            JSONObject(String(chunk, Charsets.UTF_8))
                                .put("body_key", "f".repeat(64))
                                .toString()
                                .toByteArray(Charsets.UTF_8)
                        } else {
                            chunk
                        }
                    },
                )
                val responderConn = pipe.endpoint(nodeId, senderNode)
                val responderThread = Thread {
                    responderController.accept(responderConn)
                }.apply { isDaemon = true; start() }
                return pipe.endpoint(senderNode, nodeId) {
                    responderThread.join(5_000)
                }
            }
        },
        { node -> when (node) { "node-a" -> a; "node-b" -> b; else -> null } },
        { UUIDs.next() },
        role = role,
    )

    fun drain(duplicate: Boolean = false, reverse: Boolean = false) {
        while (hints.isNotEmpty()) {
            val hint = hints.removeAt(if (reverse) hints.lastIndex else 0)
            val target: ReplicationController = if (hint.targetNode == "node-a") left else right
            target.onHint(hint.senderNode, hint.bytes)
            if (duplicate) target.onHint(hint.senderNode, hint.bytes)
        }
        if (duplicate && online && dropNextFrameKind == null && !forgeNextBodyChunk) {
            if (reverse) right.reconcile() else left.reconcile()
        }
    }

    private object UUIDs {
        private var counter = 100L
        fun next() = java.util.UUID(0, counter++).toString()
    }
}

internal fun JSONArray.objects() = (0 until length()).map(::getJSONObject)
