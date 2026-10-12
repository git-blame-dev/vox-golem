package dev.gitblame.voxgolem.assistant.replication

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class ReplicationControllerTest {
    private val a = "00000000-0000-4000-8000-000000000001"
    private val b = "00000000-0000-4000-8000-000000000002"

    @Test fun discoveryNeverEnrollsAndWrongNodeVersionOrTargetCannotImport() {
        val pair = PairHarness(a, b)
        pair.left.reconcile()
        assertTrue(pair.queue.isEmpty())
        pair.enroll()
        pair.left.reconcile()
        val request = pair.queue.removeAt(0)
        assertFalse(pair.right.onData("wrong-node", request.path, request.bytes, request.assets))
        assertFalse(pair.right.onData("node-a", request.path, JSONObject(String(request.bytes))
            .put("version", 2).toString().toByteArray(), request.assets))
        assertFalse(pair.right.onData("node-a", request.path, JSONObject(String(request.bytes))
            .put("target", a).toString().toByteArray(), request.assets))
        assertTrue(pair.rightStore.imports.isEmpty())
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
    }

    @Test fun crashAfterCommitBeforeAckAndThirtyMinuteDisconnectRecoverFromCommittedTruth() {
        val pair = PairHarness(a, b)
        pair.enroll()
        pair.leftStore.add(record(a, "1"))
        pair.right.reconcile()
        while (pair.rightStore.records.isEmpty()) pair.deliverOne()
        pair.queue.clear() // Synthetic process death between durable import and ACK delivery.
        pair.restartRight()
        pair.right.reconcile()
        pair.drain()
        assertEquals(setOf("$a/1"), pair.leftStore.acknowledged[b])
        pair.online = false
        pair.leftStore.add(record(a, "3"))
        pair.rightStore.add(record(b, "7"))
        pair.left.reconcile()
        pair.right.reconcile()
        assertTrue(pair.queue.isEmpty())
        // No wall-clock TTL: the next reachable/resume trigger works after any interval.
        pair.online = true
        pair.left.reconcile()
        pair.right.reconcile()
        pair.drain(duplicate = true)
        assertEquals(pair.leftStore.records.keys, pair.rightStore.records.keys)
    }

    @Test fun forgedBodyAssetOversizeAndQuarantinedServingAreRejected() {
        val pair = PairHarness(a, b)
        pair.enroll()
        val original = record(a, "1")
        pair.leftStore.add(original)
        pair.right.reconcile()
        while (pair.queue.none { JSONObject(String(it.bytes)).getString("kind") == "records" }) pair.deliverOne()
        val transfer = pair.queue.removeAt(pair.queue.indexOfFirst { JSONObject(String(it.bytes)).getString("kind") == "records" })
        val bad = JSONObject(original.toString()).put("body_key", "f".repeat(64)).toString().toByteArray()
        assertFalse(pair.right.onData("node-a", transfer.path, transfer.bytes, mapOf("body_0" to bad)))
        assertTrue(pair.rightStore.records.isEmpty())
        assertFalse(pair.right.onData("node-a", transfer.path, ByteArray(ReplicationWire.MAX_CONTROL_BYTES + 1), emptyMap()))
        pair.leftStore.quarantined.add("$a/1")
        pair.leftStore.filterPublicationsByQuarantine = false // Old Store index still has the staged body.
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
        val queue = mutableListOf<PairHarness.Packet>()
        var online = replicas.take(2).toSet()
        val controllers = replicas.associateWith { replica -> ReplicationController(stores.getValue(replica), FakeReplicationDomain(2), states.getValue(replica),
            object : ReplicationTransport {
                override fun reachable() = if (replica !in online) emptyList() else online.filter { it != replica }.map { PeerNode("synthetic-$it", "Synthetic peer") }
                override fun publish(path: String, bytes: ByteArray, assets: Map<String, ByteArray>) { queue.add(PairHarness.Packet(replica, path, bytes, assets)) }
            }, { node -> replicas.firstOrNull { node == "synthetic-$it" } }) }
        val random = kotlin.random.Random(7123)
        repeat(12) { round ->
            if (round == 3) online = replicas.toSet()
            controllers.values.forEach { it.reconcile() }
            var count = 0
            while (queue.isNotEmpty()) {
                check(count++ < 5000)
                val packet = queue.removeAt(random.nextInt(queue.size))
                val frame = JSONObject(String(packet.bytes))
                val target = frame.getString("target")
                if (target !in online || packet.sender !in online || (round < 8 && random.nextInt(10) == 0)) continue
                controllers.getValue(target).onData("synthetic-${packet.sender}", packet.path, packet.bytes, packet.assets)
                if (random.nextBoolean()) controllers.getValue(target).onData("synthetic-${packet.sender}", packet.path, packet.bytes, packet.assets)
            }
        }
        assertTrue(stores.values.all { it.records.keys == stores.getValue(a).records.keys })
        assertEquals(6, stores.getValue(a).records.size)
        for (source in stores.values) for (peer in replicas.filter { it != source.replica }) {
            assertEquals(source.records.keys, source.acknowledged[peer])
        }
    }

    private fun record(replica: String, sequence: String, tombstone: Boolean = false): JSONObject = JSONObject()
        .put("body_key", (if (tombstone) "d" else sequence.takeLast(1)).repeat(64))
        .put("operation", JSONObject().put("version", 1).put("id", id(replica, sequence))
            .put("body", if (tombstone) JSONObject().put("kind", "conversation_deleted").put("conversation", id(replica, "1"))
                else JSONObject().put("kind", "conversation_created").put("title", "Synthetic conversation")))

    private fun id(replica: String, sequence: String) = JSONObject().put("replica", replica).put("sequence", sequence)
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
    override fun snapshot() = JSONObject().put("version", 1).put("revision", revision.toString())
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
        return JSONObject().put("version", 1).put("revision", snapshot.getString("revision")).put("entries", JSONArray(page))
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
        return JSONArray(ids.objects().map { id -> JSONObject().put("version", 1).put("recipient_replica", replica).put("operation", id)
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

internal class PairHarness(private val a: String, private val b: String, private val pageSize: Int = 63) {
    data class Packet(val sender: String, val path: String, val bytes: ByteArray, val assets: Map<String, ByteArray>)
    val queue = mutableListOf<Packet>()
    val leftState = MemoryReplicationState()
    val rightState = MemoryReplicationState()
    val leftStore = FakeReplicationStore(a)
    val rightStore = FakeReplicationStore(b)
    var online = true
    val left = controller(a, leftStore, leftState)
    var right = controller(b, rightStore, rightState)
    fun enroll() { leftState.enrolled.add(b); rightState.enrolled.add(a) }
    fun restartRight() { right = controller(b, rightStore, rightState) }
    private fun controller(replica: String, store: FakeReplicationStore, state: MemoryReplicationState) = ReplicationController(
        store, FakeReplicationDomain(pageSize), state, object : ReplicationTransport {
            override fun reachable() = if (!online) emptyList() else listOf(PeerNode(if (replica == a) "node-b" else "node-a", "Synthetic peer"))
            override fun publish(path: String, bytes: ByteArray, assets: Map<String, ByteArray>) { queue.add(Packet(replica, path, bytes, assets)) }
        }, { node -> when (node) { "node-a" -> a; "node-b" -> b; else -> null } }, { UUIDs.next() },
    )
    fun deliverOne(reverse: Boolean = false, duplicate: Boolean = false) {
        if (queue.isEmpty()) return
        val packet = queue.removeAt(if (reverse) queue.lastIndex else 0)
        val target = if (packet.sender == a) right else left
        val node = if (packet.sender == a) "node-a" else "node-b"
        target.onData(node, packet.path, packet.bytes, packet.assets)
        if (duplicate) target.onData(node, packet.path, packet.bytes, packet.assets)
    }
    fun drain(duplicate: Boolean = false, reverse: Boolean = false) {
        var count = 0
        while (queue.isNotEmpty()) { check(count++ < 1000) { "Replication did not settle" }; deliverOne(reverse, duplicate) }
    }
    private object UUIDs {
        private var counter = 100L
        fun next() = java.util.UUID(0, counter++).toString()
    }
}

internal fun JSONArray.objects() = (0 until length()).map(::getJSONObject)
