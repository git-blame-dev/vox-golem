package dev.gitblame.voxgolem.assistant

import android.database.sqlite.SQLiteDatabase
import org.json.JSONArray
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.junit.runners.model.FrameworkMethod
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.internal.bytecode.InstrumentationConfiguration
import java.io.File

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class AssistantStoreTest {
    private val stores = mutableListOf<AssistantStore>()
    private val context = RuntimeEnvironment.getApplication()
    private val peer = "00000000-0000-4000-8000-000000000002"
    private fun nativeStore(): AssistantStore {
        val library = System.getProperty("assistant.native.library")
        assumeTrue("Set -PassistantNativeLibrary to an existing host JNI library", library != null)
        return open(DomainGateway(AssistantDomainNative::apply))
    }
    private fun open(gateway: DomainGateway) = AssistantStore(context, gateway).also(stores::add)
    @After fun closeStores() { stores.forEach(AssistantStore::close) }

    @Test fun coldRestoreKeepsImmutableRecordsReplicaSequenceAndDurablePublications() {
        val store = nativeStore()
        val record = store.createConversation("Synthetic conversation")
        val reserved = store.allocateId()
        val view = store.view().toString()
        store.close()
        val restored = nativeStore()
        assertEquals(view, restored.view().toString())
        assertEquals(record.toString(), restored.publications().getJSONObject(0).toString())
        val next = restored.allocateId()
        assertEquals(reserved.getString("replica"), next.getString("replica"))
        assertEquals(reserved.getString("sequence").toLong() + 1, next.getString("sequence").toLong())
        assertTrue(File(context.noBackupFilesDir, "assistant.db").exists())
    }

    @Test fun failedCommitRollsBackSnapshotLedgerOutboxAndReceiptsTogether() {
        val store = nativeStore()
        val record = canonical(remoteCreate("1", "Synthetic rollback"))
        store.beforeCommit = { error("Synthetic disk failure") }
        assertThrows(IllegalStateException::class.java) { store.importRecords(JSONArray().put(record)) }
        assertEquals("0", store.snapshot().getString("revision"))
        assertEquals(0, store.publications().length())
        assertEquals(0, store.receiptsToPublish().length())
        store.beforeCommit = null
        store.importRecords(JSONArray().put(record))
        assertEquals(1, store.publications().length())
        assertEquals(1, store.receiptsToPublish().length())
    }

    @Test fun revisionConflictReplansSameRecordsWithoutLosingConcurrentCommit() {
        val store = nativeStore()
        val other = nativeStore()
        var once = false
        store.beforeCompare = {
            if (!once) { once = true; other.createConversation("Synthetic concurrent") }
        }
        val record = store.createConversation("Synthetic candidate")
        assertEquals(2, store.snapshot().getJSONArray("records").length())
        assertEquals("2", store.snapshot().getString("revision"))
        assertTrue(store.publications().toString().contains(record.getString("body_key")))
    }

    @Test fun duplicateReceiveAfterCommitBeforeAckIsIdempotentAndReacksExactBody() {
        val store = nativeStore()
        val record = canonical(remoteCreate("1", "Synthetic import"))
        store.importRecords(JSONArray().put(record))
        val saved = store.snapshot().toString()
        store.close()
        val restarted = nativeStore()
        restarted.importRecords(JSONArray().put(record))
        assertEquals(saved, restarted.snapshot().toString())
        assertEquals(1, restarted.receiptsToPublish().length())
        assertEquals(record.getString("body_key"), restarted.receiptsToPublish().getJSONObject(0).getString("body_key"))
        val changed = canonical(remoteCreate("1", "Synthetic conflicting"))
        assertThrows(AssistantDomainException::class.java) { restarted.importRecords(JSONArray().put(changed)) }
        assertEquals(saved, restarted.snapshot().toString())
    }

    @Test fun exactPeerReceiptsNeverAcknowledgeGapsOrAnotherPeer() {
        val store = nativeStore()
        val first = store.createConversation("Synthetic first")
        val second = store.createConversation("Synthetic second")
        val receipt = JSONObject().put("version", 1).put("recipient_replica", peer)
            .put("operation", second.getJSONObject("operation").getJSONObject("id"))
            .put("body_key", second.getString("body_key"))
        store.acknowledge(peer, JSONArray().put(receipt))
        assertEquals(first.toString(), store.publications(peer).getJSONObject(0).toString())
        assertEquals(2, store.publications("00000000-0000-4000-8000-000000000003").length())
        val forged = JSONObject(receipt.toString()).put("body_key", "0".repeat(64))
        assertThrows(IllegalArgumentException::class.java) { store.acknowledge(peer, JSONArray().put(forged)) }
        assertEquals(1, store.publications(peer).length())
        store.close()
        assertEquals(1, nativeStore().publications(peer).length())
    }

    @Test fun deletionAtomicallyDropsContentDraftsSelectionAndPreviouslyStagedPayloads() {
        val store = nativeStore()
        val create = store.createConversation("Synthetic private title")
        val conversation = create.getJSONObject("operation").getJSONObject("id")
        val selection = store.context(conversation, null)
        store.completeTurn(store.allocateId(), selection, "Synthetic private prompt",
            JSONObject().put("kind", "text").put("text", "Synthetic private answer"), "instant", provenance())
        store.selectConversation(conversation, null)
        store.saveDraft(conversation, "Synthetic private draft", true)
        val deletion = JSONObject().put("version", 1).put("id", store.allocateId()).put("body",
            JSONObject().put("kind", "conversation_deleted").put("conversation", conversation))
        store.importRecords(JSONArray().put(canonical(deletion)))
        assertEquals(1, store.publications().length())
        assertTrue(store.publications().getJSONObject(0).getJSONObject("operation").getJSONObject("body")
            .getString("kind") == "conversation_deleted")
        assertFalse(store.snapshot().toString().contains("Synthetic private"))
        assertEquals(2, store.snapshot().getJSONArray("retired").length())
        assertTrue(store.localState().isNull("conversation"))
        assertEquals("", store.draft(conversation).getString("text"))
        store.close()
        assertFalse(nativeStore().snapshot().toString().contains("Synthetic private"))
    }

    @Test fun pinnedContextAndLocalDraftSurviveInterveningImportAndColdRestore() {
        val store = nativeStore()
        val create = store.createConversation("Synthetic context")
        val id = create.getJSONObject("operation").getJSONObject("id")
        val selection = store.context(id, null)
        store.importRecords(JSONArray().put(canonical(remoteCreate("4", "Synthetic other"))))
        val turn = store.completeTurn(store.allocateId(), selection, "Synthetic prompt",
            JSONObject().put("kind", "refusal").put("reason", "Synthetic refusal"), "instant", provenance())
        assertTrue(turn.getJSONObject("operation").getJSONObject("body").isNull("parent"))
        store.selectConversation(id, turn.getJSONObject("operation").getJSONObject("id"))
        store.saveDraft(id, "Synthetic interrupted draft", true)
        store.close()
        val restored = nativeStore()
        assertEquals("Synthetic interrupted draft", restored.draft(id).getString("text"))
        assertTrue(restored.draft(id).getBoolean("interrupted"))
        assertEquals(id.toString(), restored.localState().getJSONObject("conversation").toString())
    }
    @Test fun completionConsumesOnlyItsOwnDraftAtomicallyAndRollbackRetainsIt() {
        val store = nativeStore()
        val conversation = store.createConversation("Synthetic draft ownership").getJSONObject("operation").getJSONObject("id")
        val context = store.context(conversation, null)
        val answer = JSONObject().put("kind", "text").put("text", "Synthetic durable answer")
        store.saveDraft(conversation, "Synthetic admitted prompt", true)
        store.beforeCommit = { error("Synthetic rollback") }
        assertThrows(IllegalStateException::class.java) {
            store.completeTurn(store.allocateId(), context, "Synthetic admitted prompt", answer, "instant", provenance())
        }
        assertEquals("Synthetic admitted prompt", store.draft(conversation).getString("text"))
        assertEquals(1, store.snapshot().getJSONArray("records").length())
        store.beforeCommit = null
        val completed = store.completeTurn(store.allocateId(), context, "Synthetic admitted prompt", answer, "instant", provenance())
        assertEquals("", store.draft(conversation).getString("text"))
        store.saveDraft(conversation, "Synthetic independently newer draft")
        store.completeTurn(store.allocateId(), store.context(conversation, completed.getJSONObject("operation").getJSONObject("id")),
            "Synthetic other prompt", answer, "instant", provenance())
        store.close()
        assertEquals("Synthetic independently newer draft", nativeStore().draft(conversation).getString("text"))
    }

    @Test fun incomingRecordsCannotInstallTrustedRetiredSnapshotOrForgeBodyKeys() {
        val store = nativeStore()
        val forged = JSONObject().put("version", 1).put("revision", "99").put("records", JSONArray())
            .put("retired", JSONArray())
        assertThrows(Exception::class.java) { store.importRecords(JSONArray().put(forged)) }
        val record = canonical(remoteCreate("1", "Synthetic digest")).put("body_key", "0".repeat(64))
        assertThrows(AssistantDomainException::class.java) { store.importRecords(JSONArray().put(record)) }
        assertEquals("0", store.snapshot().getString("revision"))
    }

    @Test fun rawCommittedSnapshotIsBoundedBeforeJsonParsingOrGatewayInvocation() {
        var called = false
        val store = open(DomainGateway { called = true; error("must not call gateway") })
        store.allocateId()
        SQLiteDatabase.openDatabase(File(context.noBackupFilesDir, "assistant.db").path, null, 0).use {
            it.execSQL("UPDATE state SET snapshot = ?", arrayOf(" ".repeat(4 * 1024 * 1024 + 1)))
        }
        assertThrows(IllegalStateException::class.java) { store.view() }
        assertFalse(called)
    }

    @Test fun snapshotAboveCursorWindowSizeRestoresRawBytesThroughActualJni() {
        val store = nativeStore()
        store.allocateId()
        val raw = """{"version":1,"revision":"0","records":[],"retired":[]}""" + " ".repeat(2 * 1024 * 1024)
        SQLiteDatabase.openDatabase(File(context.noBackupFilesDir, "assistant.db").path, null, 0).use {
            it.execSQL("UPDATE state SET snapshot = ?", arrayOf(raw))
        }
        assertEquals("0", store.view().getString("revision"))
        assertEquals(0, store.snapshot().getJSONArray("records").length())
    }

    @Test fun lateCompletionAfterDeletionStaysRetiredAndDoesNotPurgeTheTombstonePublication() {
        val store = nativeStore()
        val created = store.createConversation("Synthetic late")
        val conversation = created.getJSONObject("operation").getJSONObject("id")
        val selection = store.context(conversation, null)
        val request = store.allocateId()
        val deletion = canonical(JSONObject().put("version", 1).put("id", store.allocateId()).put("body",
            JSONObject().put("kind", "conversation_deleted").put("conversation", conversation)))
        store.importRecords(JSONArray().put(deletion))
        assertThrows(AssistantCompletionSuppressedException::class.java) {
            store.completeTurn(request, selection, "Synthetic late prompt", JSONObject().put("kind", "text")
                .put("text", "Synthetic late answer"), "instant", provenance())
        }
        assertEquals(1, store.publications().length())
        assertEquals(deletion.toString(), store.publications().getJSONObject(0).toString())
        assertEquals(2, store.snapshot().getJSONArray("retired").length())
        assertFalse(store.snapshot().toString().contains("Synthetic late answer"))
        store.importRecords(JSONArray().put(deletion))
        assertEquals(deletion.toString(), store.publications().getJSONObject(0).toString())
        assertThrows(IllegalStateException::class.java) { store.saveDraft(conversation, "Synthetic blocked") }
    }

    @Test fun failedDeletionCommitPreservesDraftSelectionAndExistingPublications() {
        val store = nativeStore()
        val created = store.createConversation("Synthetic atomic purge")
        val conversation = created.getJSONObject("operation").getJSONObject("id")
        store.selectConversation(conversation, null)
        store.saveDraft(conversation, "Synthetic retain on failure")
        val before = store.snapshot().toString()
        val deletion = canonical(JSONObject().put("version", 1).put("id", store.allocateId()).put("body",
            JSONObject().put("kind", "conversation_deleted").put("conversation", conversation)))
        store.beforeCommit = { error("Synthetic interrupted deletion") }
        assertThrows(IllegalStateException::class.java) { store.importRecords(JSONArray().put(deletion)) }
        assertEquals(before, store.snapshot().toString())
        assertEquals(created.toString(), store.publications().getJSONObject(0).toString())
        assertEquals(conversation.toString(), store.localState().getJSONObject("conversation").toString())
        assertEquals("Synthetic retain on failure", store.draft(conversation).getString("text"))
        assertEquals(0, store.receiptsToPublish().length())
    }

    @Test fun boundedPublicationAndReceiptPagesResumeByExactIdWithoutDroppingWork() {
        val store = nativeStore()
        for (i in 1..4) store.importRecords(JSONArray().put(canonical(remoteCreate(i.toString(), "Synthetic page $i"))))
        val first = store.publications(limit = 2)
        val lastId = first.getJSONObject(1).getJSONObject("operation").getJSONObject("id")
        assertEquals(2, first.length())
        assertEquals(2, store.publications(limit = 2, after = lastId).length())
        assertEquals(4, store.publications().length())
        assertEquals(2, store.receiptsToPublish(limit = 2).length())
        assertEquals(2, store.receiptsToPublish(limit = 2, after = lastId).length())
        assertThrows(IllegalStateException::class.java) { store.publications(maxBytes = 2) }
    }

    @Test fun postImportNativeSnapshotQuotaFailureNeverEvictsPreviouslyCommittedHistoryOrOutbox() {
        val store = nativeStore()
        store.createConversation("Synthetic preserved")
        val before = store.snapshot().toString()
        val publications = store.publications().toString()
        val conversation = JSONObject().put("replica", peer).put("sequence", "1")
        val records = JSONArray().put(canonical(remoteCreate("1", "Synthetic oversized batch")))
        for (sequence in 2..20) {
            val id = JSONObject().put("replica", peer).put("sequence", sequence.toString())
            val body = JSONObject().put("kind", "turn_completed").put("conversation", conversation)
                .put("parent", JSONObject.NULL).put("request", id).put("prompt", "Synthetic quota prompt")
                .put("answer", JSONObject().put("kind", "text").put("text", "s".repeat(230 * 1024)))
                .put("terminal_stage", "instant").put("provenance", provenance().put("context", JSONArray()))
            records.put(canonical(JSONObject().put("version", 1).put("id", id).put("body", body)))
        }
        val failure = assertThrows(AssistantDomainException::class.java) { store.importRecords(records) }
        assertEquals("native_snapshot_bytes", failure.error.getString("limit"))
        assertEquals(before, store.snapshot().toString())
        assertEquals(publications, store.publications().toString())
        assertEquals(0, store.receiptsToPublish().length())
    }

    @Test fun validCompletionQuarantinesOldPendingChildDropsRetryPayloadAndReacksDuplicate() {
        val store = nativeStore()
        val created = store.createConversation("Synthetic valid parent")
        val conversation = created.getJSONObject("operation").getJSONObject("id")
        val pinned = store.context(conversation, null)
        val request = store.allocateId()
        val parent = JSONObject().put("replica", request.getString("replica")).put("sequence", "3")
        val childId = JSONObject().put("replica", peer).put("sequence", "3")
        val child = canonical(completed(childId, conversation, parent, JSONObject().put("replica", peer).put("sequence", "100"),
            JSONArray().put(JSONObject().put("replica", peer).put("sequence", "88")).put(parent)))
        store.importRecords(JSONArray().put(child))
        assertTrue(hasRecord(store.publications(), childId))
        val saved = store.completeTurn(request, pinned, "Synthetic authentic parent", JSONObject().put("kind", "text")
            .put("text", "Synthetic valid answer"), "instant", provenance())
        assertEquals(parent.toString(), saved.getJSONObject("operation").getJSONObject("id").toString())
        assertEquals(1, store.snapshot().getJSONArray("quarantine").length())
        assertTrue(hasRecord(store.snapshot().getJSONArray("records"), childId))
        assertFalse("Quarantine must remove already staged retry payloads", hasRecord(store.publications(), childId))
        assertTrue(hasRecord(store.publications(), parent))
        val before = store.snapshot().toString()
        val duplicate = store.importRecords(JSONArray().put(child))
        assertEquals(before, store.snapshot().toString())
        assertEquals(childId.toString(), duplicate.getJSONArray("quarantined").getJSONObject(0).toString())
        assertFalse(hasRecord(store.publications(), childId))
        assertEquals(child.getString("body_key"), store.receiptsToPublish().getJSONObject(0).getString("body_key"))
        val receipt = JSONObject().put("version", 1).put("recipient_replica", peer).put("operation", childId)
            .put("body_key", child.getString("body_key"))
        store.acknowledge(peer, JSONArray().put(receipt))
        store.close()
        val restored = nativeStore()
        assertTrue(hasRecord(restored.snapshot().getJSONArray("records"), childId))
        assertFalse(hasRecord(restored.publications(), childId))
        assertEquals(1, restored.view().getJSONArray("quarantine").length())
    }

    @Test fun quarantineTransactionFailureKeepsOldStagingAndCannotPublishTheParentAck() {
        val store = nativeStore()
        val conversation = store.createConversation("Synthetic quarantine rollback").getJSONObject("operation").getJSONObject("id")
        val parent = store.allocateId()
        val childId = JSONObject().put("replica", peer).put("sequence", "3")
        val child = canonical(completed(childId, conversation, parent, JSONObject().put("replica", peer).put("sequence", "100"),
            JSONArray().put(JSONObject().put("replica", peer).put("sequence", "88")).put(parent)))
        store.importRecords(JSONArray().put(child))
        val authentic = canonical(completed(parent, conversation, null, store.allocateId(), JSONArray()))
        val before = store.snapshot().toString()
        val publications = store.publications().toString()
        val receipts = store.receiptsToPublish().toString()
        store.beforeCommit = { error("Synthetic quarantine commit failure") }
        assertThrows(IllegalStateException::class.java) { store.importRecords(JSONArray().put(authentic)) }
        assertEquals(before, store.snapshot().toString())
        assertEquals(publications, store.publications().toString())
        assertEquals(receipts, store.receiptsToPublish().toString())
        store.beforeCommit = null
        store.importRecords(JSONArray().put(authentic))
        assertFalse(hasRecord(store.publications(), childId))
        assertTrue(hasRecord(store.snapshot().getJSONArray("records"), childId))
        assertEquals(2, store.receiptsToPublish().length())
    }

    @Test fun quarantinedDuplicateCompletionIsNotReportedSavedAndRemainsExactAckable() {
        val store = nativeStore()
        val conversation = store.createConversation("Synthetic duplicate disposition").getJSONObject("operation").getJSONObject("id")
        val request = store.allocateId()
        val duplicateId = JSONObject().put("replica", request.getString("replica")).put("sequence", "3")
        val parent = JSONObject().put("replica", peer).put("sequence", "2")
        val ancestors = JSONArray().put(parent)
        val child = canonical(completed(duplicateId, conversation, parent, request, ancestors))
        store.importRecords(JSONArray().put(child))
        store.importRecords(JSONArray().put(canonical(remoteCreate("2", "Synthetic wrong parent type"))))
        val revision = store.snapshot().getString("revision")
        val context = JSONObject().put("conversation", conversation).put("parent", parent).put("ancestor_ids", ancestors)
        assertThrows(AssistantCompletionSuppressedException::class.java) {
            store.completeTurn(request, context, "Synthetic pending prompt", JSONObject().put("kind", "text")
                .put("text", "Synthetic pending answer"), "instant", provenance())
        }
        assertEquals(revision, store.snapshot().getString("revision"))
        assertTrue(hasRecord(store.snapshot().getJSONArray("records"), duplicateId))
        assertFalse(hasRecord(store.publications(), duplicateId))
        val duplicate = store.importRecords(JSONArray().put(child))
        assertEquals(duplicateId.toString(), duplicate.getJSONArray("duplicates").getJSONObject(0).toString())
        assertEquals(duplicateId.toString(), duplicate.getJSONArray("quarantined").getJSONObject(0).toString())
        assertTrue((0 until store.receiptsToPublish().length()).any {
            val receipt = store.receiptsToPublish().getJSONObject(it)
            receipt.getJSONObject("operation").toString() == duplicateId.toString() && receipt.getString("body_key") == child.getString("body_key")
        })
    }

    @Test fun deletingQuarantineWitnessInAnotherConversationColdRestoresThroughActualJni() {
        val store = nativeStore()
        val c = store.createConversation("Synthetic C").getJSONObject("operation").getJSONObject("id")
        val a2 = store.allocateId()
        val a3 = store.allocateId()
        val d = JSONObject().put("replica", peer).put("sequence", "1")
        val b2 = JSONObject().put("replica", peer).put("sequence", "2")
        val b3 = JSONObject().put("replica", peer).put("sequence", "3")
        store.importRecords(JSONArray().put(canonical(remoteCreate("1", "Synthetic D"))))
        val t = canonical(completed(b3, d, b2, JSONObject().put("replica", peer).put("sequence", "100"), JSONArray().put(a3).put(b2)))
        store.importRecords(JSONArray().put(t))
        val q = canonical(completed(a3, c, a2, store.allocateId(), JSONArray()
            .put(JSONObject().put("replica", c.getString("replica")).put("sequence", "88")).put(a2)))
        store.importRecords(JSONArray().put(q))
        store.importRecords(JSONArray().put(canonical(completed(a2, c, null, store.allocateId(), JSONArray()))))
        assertEquals(2, store.view().getJSONArray("quarantine").length())
        val deletion = canonical(JSONObject().put("version", 1).put("id", store.allocateId()).put("body",
            JSONObject().put("kind", "conversation_deleted").put("conversation", c)))
        store.importRecords(JSONArray().put(deletion))
        assertTrue(hasRecord(store.snapshot().getJSONArray("records"), b3))
        assertFalse(hasRecord(store.snapshot().getJSONArray("records"), a3))
        assertTrue((0 until store.snapshot().getJSONArray("retired").length()).any {
            val retired = store.snapshot().getJSONArray("retired").getJSONObject(it)
            retired.getJSONObject("id").toString() == a3.toString() && retired.getString("body_key") == q.getString("body_key")
        })
        store.close()
        val restored = nativeStore()
        assertEquals(1, restored.view().getJSONArray("quarantine").length())
        assertEquals(b3.toString(), restored.view().getJSONArray("quarantine").getJSONObject(0).getJSONObject("id").toString())
        val failure = assertThrows(AssistantDomainException::class.java) { restored.context(d, b3) }
        assertEquals("quarantined", failure.error.getString("kind"))
        assertFalse(hasRecord(restored.publications(), b3))
        assertFalse(hasRecord(restored.publications(), a3))
        val before = restored.snapshot().toString()
        restored.importRecords(JSONArray().put(t))
        assertEquals(before, restored.snapshot().toString())
        assertTrue(hasRecord(restored.snapshot().getJSONArray("records"), b3))
        assertFalse(hasRecord(restored.publications(), b3))
    }

    @Test fun malformedQuarantinedDispositionCannotCommitOrProduceAnAck() {
        assumeTrue("Host JNI library required", System.getProperty("assistant.native.library") != null)
        val store = open(DomainGateway { raw ->
            val response = JSONObject(AssistantDomainNative.apply(raw))
            if (JSONObject(raw).getString("kind") == "plan" && response.getBoolean("ok")) {
                response.getJSONObject("value").put("quarantined", false)
            }
            response.toString()
        })
        val incoming = canonical(remoteCreate("1", "Synthetic malformed disposition"))
        assertThrows(org.json.JSONException::class.java) { store.importRecords(JSONArray().put(incoming)) }
        assertEquals("0", store.snapshot().getString("revision"))
        assertEquals(0, store.snapshot().getJSONArray("records").length())
        assertEquals(0, store.publications().length())
        assertEquals(0, store.receiptsToPublish().length())
    }

    private fun hasRecord(records: JSONArray, id: JSONObject): Boolean = (0 until records.length()).any {
        val candidate = records.getJSONObject(it).getJSONObject("operation").getJSONObject("id")
        candidate.getString("replica") == id.getString("replica") && candidate.getString("sequence") == id.getString("sequence")
    }

    private fun completed(id: JSONObject, conversation: JSONObject, parent: JSONObject?, request: JSONObject, ancestors: JSONArray) =
        JSONObject().put("version", 1).put("id", id).put("body", JSONObject().put("kind", "turn_completed")
            .put("conversation", conversation).put("parent", parent ?: JSONObject.NULL).put("request", request)
            .put("prompt", "Synthetic pending prompt").put("answer", JSONObject().put("kind", "text").put("text", "Synthetic pending answer"))
            .put("terminal_stage", "instant").put("provenance", provenance().put("context", ancestors)))

    private fun canonical(operation: JSONObject): JSONObject {
        val result = JSONObject(AssistantDomainNative.apply(JSONObject().put("kind", "record").put("operation", operation).toString()))
        assertTrue(result.toString(), result.getBoolean("ok"))
        return result.getJSONObject("value")
    }
    private fun remoteCreate(sequence: String, title: String) = JSONObject().put("version", 1)
        .put("id", JSONObject().put("replica", peer).put("sequence", sequence))
        .put("body", JSONObject().put("kind", "conversation_created").put("title", title))
    private fun provenance() = JSONObject().put("requested_model", "synthetic-model")
        .put("returned_model", JSONObject.NULL).put("policy_id", "synthetic-policy").put("policy_version", "1")
        .put("instruction_profile_id", "synthetic-instructions").put("instruction_profile_version", "1")
}

class AssistantStoreRunner(testClass: Class<*>) : RobolectricTestRunner(testClass) {
    override fun createClassLoaderConfig(method: FrameworkMethod): InstrumentationConfiguration =
        InstrumentationConfiguration.Builder(super.createClassLoaderConfig(method))
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.AssistantDomainNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.runtime.AssistantProviderNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.runtime.ProviderNativeCalls")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.runtime.ProviderDeltaCallback").build()
}
