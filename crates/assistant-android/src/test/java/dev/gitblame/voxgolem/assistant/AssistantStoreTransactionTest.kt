package dev.gitblame.voxgolem.assistant

import android.database.sqlite.SQLiteDatabase
import org.json.JSONArray
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.File

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class AssistantStoreTransactionTest {
    private val context = RuntimeEnvironment.getApplication()
    private val stores = mutableListOf<AssistantStore>()
    private val replica = "00000000-0000-4000-8000-000000000001"
    private val id = JSONObject().put("replica", replica).put("sequence", "1")
    private val record = JSONObject("""{"body_key":"63fb5f4ae23359b8632ff27a178d958d60a82d57be7f6fdf59e3a86276974b5b","operation":{"version":1,"id":{"replica":"$replica","sequence":"1"},"body":{"kind":"conversation_created","title":"Synthetic 🧪"}}}""")
    private val plannedSnapshot = JSONObject().put("version", 1).put("revision", "1")
        .put("records", JSONArray().put(record)).put("retired", JSONArray())

    private fun open() = AssistantStore(context, DomainGateway { raw ->
        val request = JSONObject(raw)
        val value: Any = when (request.getString("kind")) {
            "plan" -> {
                assertEquals("0", request.getJSONObject("snapshot").getString("revision"))
                assertEquals(record.toString(), request.getJSONObject("command").getJSONArray("records").getJSONObject(0).toString())
                JSONObject().put("base_revision", "0").put("snapshot", plannedSnapshot)
                    .put("accepted", JSONArray().put(id)).put("duplicates", JSONArray()).put("suppressed", JSONArray())
                    .put("purge_conversations", JSONArray()).put("publish", JSONArray().put(record))
            }
            "receipts" -> JSONArray().put(JSONObject().put("version", 1).put("recipient_replica", request.getString("recipient"))
                .put("operation", id).put("body_key", record.getString("body_key")))
            else -> error("Unexpected gateway call in transaction fixture")
        }
        JSONObject().put("ok", true).put("value", value).toString()
    }).also(stores::add)

    @After fun closeStores() { stores.forEach(AssistantStore::close) }

    @Test fun committedSnapshotRejectsAmbiguousOrMalformedRevisionDocuments() {
        val store = open()
        store.snapshot() // Create the database before injecting synthetic corrupt committed state.
        val invalid = listOf(
            "{\"revision\":\"0\",\"revision\":\"0\"}",
            "{\"revision\":0}", "{}", "{\"revision\":\"0\"} {}",
            "{\"revision\":\"0\",\"records\":[}",
            "{revision:'0'}",
        )
        for (raw in invalid) {
            SQLiteDatabase.openDatabase(File(context.noBackupFilesDir, "assistant.db").path, null, 0).use {
                it.execSQL("UPDATE state SET snapshot = ?", arrayOf(raw))
            }
            assertThrows("Accepted synthetic corrupt snapshot: $raw", Exception::class.java) { store.snapshot() }
        }
    }

    @Test fun injectedPreCommitFailureRollsBackEveryPublicationAndReceiptWithoutNativeLibrary() {
        val store = open()
        store.beforeCommit = { error("Synthetic transaction failure") }
        assertThrows(IllegalStateException::class.java) { store.importRecords(JSONArray().put(record)) }
        assertEquals("0", store.snapshot().getString("revision"))
        assertEquals(0, store.publications().length())
        assertEquals(0, store.receiptsToPublish().length())
        store.beforeCommit = null
        store.importRecords(JSONArray().put(record))
        assertEquals(plannedSnapshot.toString(), store.snapshot().toString())
        store.close()
        val cold = open()
        assertEquals(record.toString(), cold.publications().getJSONObject(0).toString())
        assertEquals(record.getString("body_key"), cold.receiptsToPublish().getJSONObject(0).getString("body_key"))
    }

    @Test fun allocatedSequenceIsUnsignedDecimalDurableAndNeverWraps() {
        val first = open()
        val initial = first.allocateId()
        val second = open()
        val next = second.allocateId()
        assertEquals(initial.getString("replica"), next.getString("replica"))
        assertEquals("1", initial.getString("sequence"))
        assertEquals("2", next.getString("sequence"))
        SQLiteDatabase.openDatabase(File(context.noBackupFilesDir, "assistant.db").path, null, 0).use {
            it.execSQL("UPDATE state SET last_sequence = '18446744073709551614'")
        }
        assertEquals("18446744073709551615", first.allocateId().getString("sequence"))
        assertThrows(IllegalStateException::class.java) { second.allocateId() }
        assertThrows(IllegalStateException::class.java) { first.allocateId() }
    }

    @Test fun freshDraftAndSelectionPersistLocallyAndNeverEnterPublicationWork() {
        val store = open()
        store.selectConversation(id, null)
        store.saveDraft(null, "Synthetic fresh draft", true)
        store.saveDraft(id, "Synthetic conversation draft")
        store.close()
        val restored = open()
        assertEquals("Synthetic fresh draft", restored.draft(null).getString("text"))
        assertTrue(restored.draft(null).getBoolean("interrupted"))
        assertEquals("Synthetic conversation draft", restored.draft(id).getString("text"))
        assertEquals(id.toString(), restored.localState().getJSONObject("conversation").toString())
        assertEquals(0, restored.publications().length())
        assertEquals(0, restored.receiptsToPublish().length())
        assertThrows(IllegalArgumentException::class.java) { restored.saveDraft(id, "🧪".repeat(8193)) }
        assertEquals("Synthetic conversation draft", restored.draft(id).getString("text"))
    }
}
