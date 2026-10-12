package dev.gitblame.voxgolem.assistant.replication

import android.content.Context
import android.content.ContextWrapper
import dev.gitblame.voxgolem.assistant.AssistantDomainNative
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.DomainGateway
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.File

/** Exercises the production controller, SQLite and actual Rust hash/import/reconcile gateway. */
@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class NativeReplicationControllerTest {
    @get:Rule val directories = TemporaryFolder()
    private val gateway by lazy { DomainGateway(AssistantDomainNative::apply) }

    @Test fun paginatedTwoWayNativeHistoryConvergesWithGapsDuplicatesLostAckAndTombstones() {
        assumeTrue("Parent supplies the host JNI library", System.getProperty("assistant.native.library") != null)
        AssistantStore(isolated("synthetic-left"), gateway).use { left ->
            AssistantStore(isolated("synthetic-right"), gateway).use { right ->
                val first = left.createConversation("Synthetic native history")
                val conversation = first.getJSONObject("operation").getJSONObject("id")
                left.allocateId() // Deliberate sequence gap is never an acknowledgement watermark.
                left.completeTurn(left.allocateId(), left.context(conversation, null), "Synthetic prompt",
                    JSONObject().put("kind", "text").put("text", "Synthetic answer"), "instant", provenance())
                repeat(65) { left.createConversation("Synthetic page $it") }
                right.createConversation("Synthetic independent right history")
                val harness = NativeHarness(left, right, gateway)
                harness.begin()
                harness.drain(dropFirstAck = true, duplicate = true)
                harness.restartControllers()
                harness.begin()
                harness.drain(duplicate = true)
                assertEquals(keys(left), keys(right))
                assertEquals(0, left.publications(harness.rightReplica).length())
                assertEquals(0, right.publications(harness.leftReplica).length())

                val tombstone = canonical(JSONObject().put("version", 1).put("id", right.allocateId()).put("body",
                    JSONObject().put("kind", "conversation_deleted").put("conversation", conversation)))
                right.importRecords(JSONArray().put(tombstone))
                harness.begin()
                harness.drain(duplicate = true)
                harness.begin()
                harness.drain()
                assertEquals(keys(left), keys(right))
                assertFalse(left.snapshot().toString().contains("Synthetic answer"))
                assertFalse(right.snapshot().toString().contains("Synthetic answer"))
                assertTrue(left.publications().objects().any { it.getString("body_key") == tombstone.getString("body_key") })
            }
        }
    }

    @Test fun forgedCanonicalBodyAndFailedActualSqliteCommitNeverProduceAck() {
        assumeTrue("Parent supplies the host JNI library", System.getProperty("assistant.native.library") != null)
        AssistantStore(isolated("synthetic-source"), gateway).use { left ->
            AssistantStore(isolated("synthetic-target"), gateway).use { right ->
                left.createConversation("Synthetic transactional import")
                val harness = NativeHarness(left, right, gateway)
                var attemptedCommits = 0
                right.beforeCommit = { attemptedCommits++; error("Synthetic durable commit fault") }
                harness.begin()
                harness.drain(forgeFirstRecord = true)
                assertEquals(0, attemptedCommits)
                harness.begin()
                harness.drain()
                assertTrue(attemptedCommits > 0)
                assertEquals(0, right.snapshot().getJSONArray("records").length())
                assertEquals(0, right.receiptsToPublish().length())
                assertEquals(1, left.publications(harness.rightReplica).length())
                right.beforeCommit = null
                harness.restartControllers()
                harness.begin()
                harness.drain()
                assertEquals(keys(left), keys(right))
                assertEquals(0, left.publications(harness.rightReplica).length())
            }
        }
    }

    @Test fun missingAncestorsOutsideTheCurrentPageAreRequestedByIdAndValidatedByRust() {
        assumeTrue("Parent supplies the host JNI library", System.getProperty("assistant.native.library") != null)
        AssistantStore(isolated("synthetic-ancestor-source"), gateway).use { left ->
            AssistantStore(isolated("synthetic-ancestor-target"), gateway).use { right ->
                val early = "00000000-0000-4000-8000-000000000001"
                val middle = "00000000-0000-4000-8000-000000000002"
                val late = "ffffffff-ffff-4fff-8fff-ffffffffffff"
                val conversation = id(late, "8")
                val parent = id(late, "9")
                for (range in listOf(1..128, 129..131)) {
                    left.importRecords(JSONArray(range.map { sequence -> canonical(JSONObject().put("version", 1).put("id", id(middle, sequence.toString()))
                        .put("body", JSONObject().put("kind", "conversation_created").put("title", "Synthetic bounded ancestor page"))) }))
                }
                fun turn(operation: JSONObject, ancestor: JSONObject?, context: JSONArray) = canonical(JSONObject().put("version", 1).put("id", operation)
                    .put("body", JSONObject().put("kind", "turn_completed").put("conversation", conversation)
                        .put("parent", ancestor ?: JSONObject.NULL).put("request", operation).put("prompt", "Synthetic ancestor prompt")
                        .put("answer", JSONObject().put("kind", "text").put("text", "Synthetic ancestor answer"))
                        .put("terminal_stage", "instant").put("provenance", provenance().put("context", context))))
                left.importRecords(JSONArray().put(turn(id(early, "1"), parent, JSONArray().put(parent))))
                left.importRecords(JSONArray().put(turn(parent, null, JSONArray())).put(canonical(JSONObject().put("version", 1).put("id", conversation)
                    .put("body", JSONObject().put("kind", "conversation_created").put("title", "Synthetic late conversation")))))
                val harness = NativeHarness(left, right, gateway)
                harness.begin()
                harness.drain(duplicate = true)
                assertTrue(harness.requestedUnknownAncestor)
                assertEquals(keys(left), keys(right))
                assertEquals(2, right.context(conversation, id(early, "1")).getJSONArray("ancestor_ids").length())
            }
        }
    }

    @Test fun authenticParentImportsQuarantinePendingPoisonWithoutReservingAHotAncestorLoop() {
        assumeTrue("Parent supplies the current host JNI gateway", System.getProperty("assistant.native.library") != null)
        AssistantStore(isolated("synthetic-quarantine-source"), gateway).use { left ->
            AssistantStore(isolated("synthetic-quarantine-target"), gateway).use { right ->
                val replica = "00000000-0000-4000-8000-000000000001"
                val other = "00000000-0000-4000-8000-000000000002"
                val conversation = id(replica, "1")
                fun turn(sequence: String, parent: JSONObject?, context: JSONArray) = canonical(JSONObject().put("version", 1).put("id", id(replica, sequence))
                    .put("body", JSONObject().put("kind", "turn_completed").put("conversation", conversation)
                        .put("parent", parent ?: JSONObject.NULL).put("request", id(replica, "1$sequence"))
                        .put("prompt", "Synthetic quarantine prompt").put("answer", JSONObject().put("kind", "text").put("text", "Synthetic quarantine answer"))
                        .put("terminal_stage", "instant").put("provenance", provenance().put("context", context))))
                val create = canonical(JSONObject().put("version", 1).put("id", conversation).put("body",
                    JSONObject().put("kind", "conversation_created").put("title", "Synthetic quarantine conversation")))
                val poison = turn("4", id(replica, "3"), JSONArray().put(id(other, "88")).put(id(replica, "3")))
                left.importRecords(JSONArray().put(create).put(turn("2", null, JSONArray())).put(poison))
                val harness = NativeHarness(left, right, gateway)
                harness.begin(); harness.drain(duplicate = true)
                assertEquals(3, right.snapshot().getJSONArray("records").length())
                harness.wireBodyIds.clear()
                left.importRecords(JSONArray().put(turn("3", null, JSONArray())))
                harness.begin(); harness.drain(duplicate = true)
                assertEquals(1, left.snapshot().getJSONArray("quarantine").length())
                assertEquals(1, right.snapshot().getJSONArray("quarantine").length())
                assertEquals(4, right.snapshot().getJSONArray("records").length())
                assertFalse(harness.wireBodyIds.contains("$replica/4"))
                assertTrue(harness.wireBodyIds.contains("$replica/3"))
                assertTrue(harness.rejections.isEmpty())
                assertEquals(1, right.context(conversation, id(replica, "3")).getJSONArray("ancestor_ids").length())
                assertThrows(Exception::class.java) { right.context(conversation, id(replica, "4")) }
                assertTrue(right.receiptsToPublish().objects().any { it.getString("body_key") == poison.getString("body_key") })
            }
        }
    }

    @Test fun rustLexicalInventoryPageAfterSequence10AcceptsSequence2InProductionController() {
        assumeTrue("Parent supplies the current host JNI gateway", System.getProperty("assistant.native.library") != null)
        AssistantStore(isolated("synthetic-lexical-source"), gateway).use { left ->
            AssistantStore(isolated("synthetic-lexical-target"), gateway).use { right ->
                val operationReplica = "00000000-0000-4000-8000-000000000001"
                left.importRecords(JSONArray(listOf("1", "10", "2").map { sequence -> canonical(JSONObject().put("version", 1)
                    .put("id", id(operationReplica, sequence)).put("body", JSONObject().put("kind", "conversation_created").put("title", "Synthetic lexical page"))) }))
                val snapshot = left.snapshot()
                val cursor = JSONObject().put("revision", snapshot.getString("revision")).put("after", id(operationReplica, "10"))
                val page = JSONObject(gateway.apply(JSONObject().put("kind", "inventory").put("snapshot", snapshot)
                    .put("request", JSONObject().put("cursor", cursor).put("max_items", 1)).toString())).getJSONObject("value")
                assertEquals("2", page.getJSONArray("entries").getJSONObject(0).getJSONObject("id").getString("sequence"))
                val source = NativeReplicationStore(left)
                val target = NativeReplicationStore(right)
                val state = MemoryReplicationState().apply { enrolled.add(source.replica) }
                val session = "00000000-0000-4000-8000-000000000010"
                val token = "00000000-0000-4000-8000-000000000020"
                val request = ReplicationWire.frame(target.replica, source.replica, session, token, "inventory_request",
                    JSONObject().put("phase", "records").put("cursor", cursor))
                state.save(source.replica, JSONObject().put("session", session).put("phase", "records")
                    .put("revision", snapshot.getString("revision")).put("pages", 0).put("pending", request))
                val sent = mutableListOf<JSONObject>()
                val controller = ReplicationController(target, NativeReplicationDomain(gateway), state, object : ReplicationTransport {
                    override fun reachable() = listOf(PeerNode("synthetic-source-node", "Synthetic lexical source"))
                    override fun publish(path: String, bytes: ByteArray, assets: Map<String, ByteArray>) { sent.add(JSONObject(String(bytes))) }
                }, { source.replica })
                val response = ReplicationWire.frame(source.replica, target.replica, session, token, "inventory", JSONObject().put("page", page))
                assertTrue(controller.onData("synthetic-source-node", ReplicationWire.path(source.replica, target.replica, "inventory"),
                    ReplicationWire.encode(response), emptyMap()))
                val fetch = sent.single { it.getString("kind") == "record_request" }
                assertEquals("2", fetch.getJSONObject("payload").getJSONArray("entries").getJSONObject(0).getJSONObject("id").getString("sequence"))
            }
        }
    }

    @Test fun multiDigitPublicationHintsUseRustCanonicalLexicalOrderAndConverge() {
        assumeTrue("Parent supplies the current host JNI gateway", System.getProperty("assistant.native.library") != null)
        AssistantStore(isolated("synthetic-hint-source"), gateway).use { left ->
            AssistantStore(isolated("synthetic-hint-target"), gateway).use { right ->
                val operationReplica = "00000000-0000-4000-8000-000000000001"
                left.importRecords(JSONArray(listOf("2", "10").map { sequence -> canonical(JSONObject().put("version", 1)
                    .put("id", id(operationReplica, sequence)).put("body", JSONObject().put("kind", "conversation_created").put("title", "Synthetic lexical hint"))) }))
                val harness = NativeHarness(left, right, gateway)
                harness.begin(); harness.drain(duplicate = true)
                assertTrue(harness.rejections.toString(), harness.rejections.isEmpty())
                val offers = harness.hintOffers.first { it.length() > 0 }
                assertEquals(listOf("10", "2"), offers.objects().map { it.getJSONObject("id").getString("sequence") })
                assertEquals(keys(left), keys(right))
            }
        }
    }

    private fun isolated(name: String): Context {
        val root = directories.newFolder(name)
        return object : ContextWrapper(RuntimeEnvironment.getApplication()) {
            override fun getApplicationContext(): Context = this
            override fun getNoBackupFilesDir(): File = root
        }
    }
    private fun keys(store: AssistantStore) = store.snapshot().getJSONArray("records").objects()
        .map { ReplicationWire.idKey(it.getJSONObject("operation").getJSONObject("id")) to it.getString("body_key") }.toSet()
    private fun canonical(operation: JSONObject) = JSONObject(gateway.apply(JSONObject().put("kind", "record").put("operation", operation).toString())).getJSONObject("value")
    private fun id(replica: String, sequence: String) = JSONObject().put("replica", replica).put("sequence", sequence)
    private fun provenance() = JSONObject().put("requested_model", "synthetic-model").put("returned_model", JSONObject.NULL)
        .put("policy_id", "synthetic-policy").put("policy_version", "1").put("instruction_profile_id", "synthetic-instructions")
        .put("instruction_profile_version", "1")
}

internal class NativeHarness(left: AssistantStore, right: AssistantStore, private val gateway: DomainGateway) {
    private val leftStore = NativeReplicationStore(left)
    private val rightStore = NativeReplicationStore(right)
    val leftReplica = leftStore.replica
    val rightReplica = rightStore.replica
    private val leftState = MemoryReplicationState().apply { enrolled.add(rightReplica) }
    private val rightState = MemoryReplicationState().apply { enrolled.add(leftReplica) }
    private val packets = mutableListOf<PairHarness.Packet>()
    val wireBodyIds = mutableListOf<String>()
    val hintOffers = mutableListOf<JSONArray>()
    val rejections = mutableListOf<Exception>()
    private var leftController = controller(leftStore, leftState)
    private var rightController = controller(rightStore, rightState)
    var requestedUnknownAncestor = false
        private set
    fun restartControllers() { leftController = controller(leftStore, leftState); rightController = controller(rightStore, rightState) }
    fun begin() { leftController.reconcile(); rightController.reconcile() }
    private fun controller(store: ReplicationStore, state: MemoryReplicationState) = ReplicationController(store, NativeReplicationDomain(gateway), state,
        object : ReplicationTransport {
            override fun reachable() = listOf(PeerNode(if (store.replica == leftReplica) "synthetic-right-node" else "synthetic-left-node", "Synthetic native peer"))
            override fun publish(path: String, bytes: ByteArray, assets: Map<String, ByteArray>) {
                val frame = JSONObject(String(bytes))
                if (frame.getString("kind") == "hint" && frame.getJSONObject("payload").has("offers"))
                    hintOffers.add(JSONArray(frame.getJSONObject("payload").getJSONArray("offers").toString()))
                if (frame.getString("kind") == "record_request" && frame.getJSONObject("payload").getJSONArray("entries").objects().any { it.isNull("body_key") }) requestedUnknownAncestor = true
                if (frame.getString("kind") == "records") wireBodyIds.addAll(frame.getJSONObject("payload").getJSONArray("records").objects()
                    .map { ReplicationWire.idKey(it.getJSONObject("entry").getJSONObject("id")) })
                packets.add(PairHarness.Packet(store.replica, path, bytes, assets))
            }
        }, { node -> when (node) { "synthetic-left-node" -> leftReplica; "synthetic-right-node" -> rightReplica; else -> null } }, onRejected = rejections::add)
    fun drain(dropFirstAck: Boolean = false, duplicate: Boolean = false, forgeFirstRecord: Boolean = false) {
        var dropped = !dropFirstAck
        var forged = !forgeFirstRecord
        var count = 0
        while (packets.isNotEmpty()) {
            check(count++ < 10_000) { "Native replication did not settle" }
            val packet = packets.removeAt(0)
            if (!dropped && JSONObject(String(packet.bytes)).getString("kind") == "ack") { dropped = true; continue }
            val controller = if (packet.sender == leftReplica) rightController else leftController
            val node = if (packet.sender == leftReplica) "synthetic-left-node" else "synthetic-right-node"
            val assets = if (!forged && JSONObject(String(packet.bytes)).getString("kind") == "records" && packet.assets.isNotEmpty()) {
                forged = true
                val altered = JSONObject(String(packet.assets.getValue("body_0")))
                // Keep encoded byte length consistent so only Rust's canonical hash rejects it.
                val originalTitle = JSONObject(String(packet.assets.getValue("body_0"))).getJSONObject("operation").getJSONObject("body").getString("title")
                altered.getJSONObject("operation").getJSONObject("body").put("title", "x".repeat(originalTitle.length))
                packet.assets + ("body_0" to altered.toString().toByteArray())
            } else packet.assets
            controller.onData(node, packet.path, packet.bytes, assets)
            if (duplicate) controller.onData(node, packet.path, packet.bytes, assets)
        }
    }
}
