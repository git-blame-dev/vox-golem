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
import java.io.EOFException
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

                val tombstone = canonical(JSONObject().put("version", 2).put("id", right.allocateId()).put("body",
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

    @Test fun forgedCanonicalBodyAndInjectedCommitFailureNeverProduceAck() {
        assumeTrue("Parent supplies the host JNI library", System.getProperty("assistant.native.library") != null)
        AssistantStore(isolated("synthetic-source"), gateway).use { left ->
            AssistantStore(isolated("synthetic-target"), gateway).use { right ->
                left.createConversation("Synthetic transactional import")
                val harness = NativeHarness(left, right, gateway)
                var attemptedCommits = 0
                harness.begin()
                harness.drain(forgeFirstRecord = true)
                assertEquals(0, attemptedCommits)
                assertEquals(0, right.snapshot().getJSONArray("records").length())
                assertEquals(0, right.receiptsToPublish().length())
                right.beforeCommit = { attemptedCommits++; error("Synthetic durable commit fault") }
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
                    left.importRecords(JSONArray(range.map { sequence -> canonical(JSONObject().put("version", 2).put("id", id(middle, sequence.toString()))
                        .put("body", JSONObject().put("kind", "conversation_created").put("title", "Synthetic bounded ancestor page"))) }))
                }
                fun turn(operation: JSONObject, ancestor: JSONObject?, context: JSONArray) = canonical(JSONObject().put("version", 2).put("id", operation)
                    .put("body", JSONObject().put("kind", "turn_completed").put("sources", JSONArray()).put("conversation", conversation)
                        .put("parent", ancestor ?: JSONObject.NULL).put("request", operation).put("prompt", "Synthetic ancestor prompt")
                        .put("answer", JSONObject().put("kind", "text").put("text", "Synthetic ancestor answer"))
                        .put("terminal_stage", "instant").put("provenance", provenance().put("context", context))))
                left.importRecords(JSONArray().put(turn(id(early, "1"), parent, JSONArray().put(parent))))
                left.importRecords(JSONArray().put(turn(parent, null, JSONArray())).put(canonical(JSONObject().put("version", 2).put("id", conversation)
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
                fun turn(sequence: String, parent: JSONObject?, context: JSONArray) = canonical(JSONObject().put("version", 2).put("id", id(replica, sequence))
                    .put("body", JSONObject().put("kind", "turn_completed").put("sources", JSONArray()).put("conversation", conversation)
                        .put("parent", parent ?: JSONObject.NULL).put("request", id(replica, "1$sequence"))
                        .put("prompt", "Synthetic quarantine prompt").put("answer", JSONObject().put("kind", "text").put("text", "Synthetic quarantine answer"))
                        .put("terminal_stage", "instant").put("provenance", provenance().put("context", context))))
                val create = canonical(JSONObject().put("version", 2).put("id", conversation).put("body",
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
                left.importRecords(JSONArray(listOf("1", "10", "2").map { sequence -> canonical(JSONObject().put("version", 2)
                    .put("id", id(operationReplica, sequence)).put("body", JSONObject().put("kind", "conversation_created").put("title", "Synthetic lexical page"))) }))
                val snapshot = left.snapshot()
                val revision = snapshot.getString("revision")
                val initialCursor = JSONObject().put("revision", revision).put("after", JSONObject.NULL)
                val page1 = JSONObject(gateway.apply(JSONObject().put("kind", "inventory").put("snapshot", snapshot)
                    .put("request", JSONObject().put("cursor", initialCursor).put("max_items", 2)).toString())).getJSONObject("value")
                val cursorAfter10 = page1.getJSONObject("next")
                assertEquals("10", cursorAfter10.getJSONObject("after").getString("sequence"))
                val page2 = JSONObject(gateway.apply(JSONObject().put("kind", "inventory").put("snapshot", snapshot)
                    .put("request", JSONObject().put("cursor", cursorAfter10).put("max_items", 1)).toString())).getJSONObject("value")
                assertEquals("2", page2.getJSONArray("entries").getJSONObject(0).getJSONObject("id").getString("sequence"))

                val source = object : ReplicationStore by NativeReplicationStore(left) {
                    override fun nativeRead(request: JSONObject): Any? {
                        if (request.getString("kind") == "inventory") {
                            return if (request.isNull("cursor") || request.getJSONObject("cursor").isNull("after")) page1 else page2
                        }
                        return NativeReplicationStore(left).nativeRead(request)
                    }
                }
                val target = NativeReplicationStore(right)
                val sourceState = MemoryReplicationState().apply { enrolled.add(target.replica) }
                val targetState = MemoryReplicationState().apply { enrolled.add(source.replica) }
                val requestedSequences = mutableListOf<String>()
                lateinit var sourceController: ReplicationController
                val targetController = ReplicationController(
                    target,
                    NativeReplicationDomain(gateway),
                    targetState,
                    object : ReplicationTransport {
                        override fun reachable() = listOf(PeerNode("synthetic-source-node", "Synthetic lexical source"))
                        override fun openSession(nodeId: String, session: String): ReplicationSessionConnection {
                            val pipe = InMemorySessionPipe(
                                session = session,
                                onFrame = { _, frame ->
                                    if (frame.getString("kind") == "record_request") {
                                        requestedSequences.addAll(
                                            frame.getJSONObject("payload").getJSONArray("entries").objects()
                                                .map { it.getJSONObject("id").getString("sequence") }
                                        )
                                    }
                                },
                            )
                            val responderConn = pipe.endpoint("synthetic-source-node", "synthetic-target-node")
                            val responderThread = Thread {
                                sourceController.accept(responderConn)
                            }.apply { isDaemon = true; start() }
                            return pipe.endpoint("synthetic-target-node", "synthetic-source-node") {
                                responderThread.join(5_000)
                            }
                        }
                    },
                    { source.replica },
                )
                sourceController = ReplicationController(
                    source,
                    NativeReplicationDomain(gateway),
                    sourceState,
                    object : ReplicationTransport {
                        override fun reachable() = listOf(PeerNode("synthetic-target-node", "Synthetic lexical target"))
                    },
                    { target.replica },
                )
                assertTrue(targetController.connect("synthetic-source-node", source.replica))
                assertEquals(listOf("1", "10", "2"), requestedSequences)
                assertEquals(keys(left), keys(right))
            }
        }
    }

    @Test fun multiDigitPublicationHintsUseRustCanonicalLexicalOrderAndConverge() {
        assumeTrue("Parent supplies the current host JNI gateway", System.getProperty("assistant.native.library") != null)
        AssistantStore(isolated("synthetic-hint-source"), gateway).use { left ->
            AssistantStore(isolated("synthetic-hint-target"), gateway).use { right ->
                val operationReplica = "00000000-0000-4000-8000-000000000001"
                left.importRecords(JSONArray(listOf("2", "10").map { sequence -> canonical(JSONObject().put("version", 2)
                    .put("id", id(operationReplica, sequence)).put("body", JSONObject().put("kind", "conversation_created").put("title", "Synthetic lexical hint"))) }))
                val harness = NativeHarness(left, right, gateway, leftRole = AssistantPeerRole.WATCH, rightRole = AssistantPeerRole.PHONE)
                harness.begin()
                assertEquals(1, harness.hints.size)
                assertTrue(harness.hints.single().bytes.size <= ReplicationWire.MAX_HINT_BYTES)
                harness.drain(duplicate = true)
                assertTrue(harness.rejections.toString(), harness.rejections.isEmpty())
                val inventorySequences = harness.inventoryPages.first { it.length() > 0 }
                    .objects().map { it.getJSONObject("id").getString("sequence") }
                assertEquals(listOf("10", "2"), inventorySequences)
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

internal class NativeHarness(
    left: AssistantStore,
    right: AssistantStore,
    private val gateway: DomainGateway,
    private val leftRole: AssistantPeerRole = AssistantPeerRole.PHONE,
    private val rightRole: AssistantPeerRole = AssistantPeerRole.WATCH,
) {
    // Every native protocol scenario must use only bounded stateful reads and coarse
    // single-transaction importVerified receipts (never snapshot() or post-import durableReceipts()).
    private fun snapshotFree(store: AssistantStore): ReplicationStore = object : ReplicationStore by NativeReplicationStore(store) {
        override fun snapshot(): JSONObject = error("Production replication crossed whole history")
        override fun durableReceipts(ids: JSONArray, checkBudget: () -> Unit): JSONArray =
            error("Production importVerified must return receipts atomically without calling durableReceipts")
    }
    private val leftStore = snapshotFree(left)
    private val rightStore = snapshotFree(right)
    val leftReplica = leftStore.replica
    val rightReplica = rightStore.replica
    private val leftState = MemoryReplicationState().apply { enrolled.add(rightReplica) }
    private val rightState = MemoryReplicationState().apply { enrolled.add(leftReplica) }
    val hints = mutableListOf<PairHarness.HintPacket>()
    val wireBodyIds = mutableListOf<String>()
    val inventoryPages = mutableListOf<JSONArray>()
    val rejections = mutableListOf<Exception>()
    private var dropNextAck = false
    private var forgeNextRecord = false
    private var deferSessions = false
    private var pendingPhoneReconcile = false
    private var leftController: ReplicationController = controller(leftStore, leftState, leftRole)
    private var rightController: ReplicationController = controller(rightStore, rightState, rightRole)
    var requestedUnknownAncestor = false
        private set

    fun restartControllers() {
        leftController = controller(leftStore, leftState, leftRole)
        rightController = controller(rightStore, rightState, rightRole)
    }

    fun begin() {
        deferSessions = true
        try {
            leftController.reconcile()
            rightController.reconcile()
        } finally {
            deferSessions = false
        }
    }

    private fun controller(
        store: ReplicationStore,
        state: MemoryReplicationState,
        role: AssistantPeerRole,
    ): ReplicationController = ReplicationController(
        store,
        NativeReplicationDomain(gateway),
        state,
        object : ReplicationTransport {
            override fun reachable() = listOf(
                PeerNode(if (store.replica == leftReplica) "synthetic-right-node" else "synthetic-left-node", "Synthetic native peer")
            )
            override fun sendHint(nodeId: String, sender: String, target: String) {
                val senderNode = if (store.replica == leftReplica) "synthetic-left-node" else "synthetic-right-node"
                hints.add(PairHarness.HintPacket(senderNode, nodeId, ReplicationWire.encodeHint(sender, target)))
            }
            override fun openSession(nodeId: String, session: String): ReplicationSessionConnection {
                if (deferSessions) {
                    pendingPhoneReconcile = true
                    throw EOFException("Deferred to drain")
                }
                val senderNode = if (store.replica == leftReplica) "synthetic-left-node" else "synthetic-right-node"
                val responder: ReplicationController = if (nodeId == "synthetic-left-node") leftController else rightController
                val pipe = InMemorySessionPipe(
                    session = session,
                    onFrame = { _, frame ->
                        when (frame.getString("kind")) {
                            "inventory" -> {
                                val entries = frame.getJSONObject("payload").getJSONObject("page").getJSONArray("entries")
                                if (entries.length() > 0) inventoryPages.add(JSONArray(entries.toString()))
                            }
                            "record_request" -> {
                                if (frame.getJSONObject("payload").getJSONArray("entries").objects().any { it.isNull("body_key") }) {
                                    requestedUnknownAncestor = true
                                }
                            }
                            "records" -> {
                                wireBodyIds.addAll(
                                    frame.getJSONObject("payload").getJSONArray("records").objects()
                                        .map { ReplicationWire.idKey(it.getJSONObject("entry").getJSONObject("id")) }
                                )
                            }
                            "ack" -> {
                                if (dropNextAck) {
                                    dropNextAck = false
                                    throw EOFException("Synthetic lost ack")
                                }
                            }
                        }
                    },
                    transformChunk = { _, chunk ->
                        if (forgeNextRecord) {
                            forgeNextRecord = false
                            val original = JSONObject(String(chunk, Charsets.UTF_8))
                            val altered = JSONObject(original.toString())
                            val originalTitle = original.getJSONObject("operation").getJSONObject("body").getString("title")
                            altered.getJSONObject("operation").getJSONObject("body").put("title", "x".repeat(originalTitle.length))
                            altered.toString().toByteArray(Charsets.UTF_8)
                        } else {
                            chunk
                        }
                    },
                )
                val responderConn = pipe.endpoint(nodeId, senderNode)
                val responderThread = Thread {
                    responder.accept(responderConn)
                }.apply { isDaemon = true; start() }
                return pipe.endpoint(senderNode, nodeId) {
                    responderThread.join(5_000)
                }
            }
        },
        { node -> when (node) { "synthetic-left-node" -> leftReplica; "synthetic-right-node" -> rightReplica; else -> null } },
        onRejected = { error -> if (!deferSessions) rejections.add(error) },
        role = role,
    )

    fun drain(dropFirstAck: Boolean = false, duplicate: Boolean = false, forgeFirstRecord: Boolean = false) {
        dropNextAck = dropFirstAck
        forgeNextRecord = forgeFirstRecord
        val runPhoneReconcile = pendingPhoneReconcile
        pendingPhoneReconcile = false
        val hadHints = hints.isNotEmpty()
        while (hints.isNotEmpty()) {
            val hint = hints.removeAt(0)
            val target: ReplicationController = if (hint.targetNode == "synthetic-left-node") leftController else rightController
            target.onHint(hint.senderNode, hint.bytes)
            if (duplicate && !dropFirstAck && !forgeFirstRecord) {
                target.onHint(hint.senderNode, hint.bytes)
            }
        }
        if (!hadHints && runPhoneReconcile && !dropFirstAck && !forgeFirstRecord) {
            if (leftRole == AssistantPeerRole.PHONE) leftController.reconcile()
            if (rightRole == AssistantPeerRole.PHONE) rightController.reconcile()
        }
    }
}
