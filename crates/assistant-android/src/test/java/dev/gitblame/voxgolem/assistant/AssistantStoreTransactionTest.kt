package dev.gitblame.voxgolem.assistant

import android.content.ContextWrapper
import android.database.sqlite.SQLiteDatabase
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.File

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class AssistantStoreTransactionTest {
    private val context = RuntimeEnvironment.getApplication()
    private val stores = mutableListOf<AssistantStore>()
    private fun open() = AssistantStore(context).also(stores::add)
    @After fun closeStores() { stores.forEach(AssistantStore::close) }

    @Test fun constructionAndClosingUnopenedWrapperNeverCreateDatabaseAndHonorIsolatedNoBackupDir() {
        val syntheticRoot = File(context.cacheDir, "synthetic-isolated-history").apply { mkdirs() }
        val isolated = object : ContextWrapper(context) { override fun getNoBackupFilesDir() = syntheticRoot }
        val unopened = AssistantStore(isolated)
        assertFalse(File(syntheticRoot, "assistant.db").exists())
        unopened.close()
        assertFalse(File(syntheticRoot, "assistant.db").exists())
        val store = AssistantStore(isolated).also(stores::add)
        store.createConversation("Synthetic isolated")
        assertTrue(File(syntheticRoot, "assistant.db").exists())
        assertFalse(File(context.noBackupFilesDir, "assistant.db").exists())
    }

    @Test fun ordinaryReadsDraftsAndProjectionsNeverCrossTheSnapshotGateway() {
        var codecCalls = 0
        val store = AssistantStore(context, DomainGateway { raw ->
            codecCalls++
            assertEquals("record", JSONObject(raw).getString("kind"))
            assertFalse(JSONObject(raw).has("snapshot"))
            AssistantDomainNative.apply(raw)
        }).also(stores::add)
        val id = store.createConversation("Synthetic native owner").getJSONObject("operation").getJSONObject("id")
        assertEquals(1, codecCalls)
        store.selectConversation(id, null)
        store.saveDraft(id, "Synthetic draft", true)
        assertEquals("Synthetic draft", store.draft(id).getString("text"))
        val projection = store.project(id, null, false, AssistantProjectionRead.Context())
        assertFalse(projection.view.has("records"))
        assertEquals(1, projection.view.getInt("conversation_count"))
        assertEquals(0, projection.read()!!.getJSONArray("turns").length())
        assertEquals(1, store.view().getJSONArray("conversations").length())
        assertTrue(store.projectionStillCurrent(projection.view.getString("revision"), id.toString(), null))
        assertEquals(1, codecCalls)
    }

    @Test fun allocatedSequenceIsDurableAcrossConcurrentHandlesAndReopenThroughRealJni() {
        val first = open(); val second = open()
        assertEquals("1", first.allocateId().getString("sequence"))
        assertEquals("2", second.allocateId().getString("sequence"))
        first.close(); second.close()
        assertEquals("3", open().allocateId().getString("sequence"))
    }

    @Test fun alphaHistoryResetPreservesAdministrativeIdentityAndUnownedTablesThroughJni() {
        val path = File(context.noBackupFilesDir, "assistant.db")
        SQLiteDatabase.openOrCreateDatabase(path, null).use { db ->
            db.execSQL("CREATE TABLE state(singleton INTEGER PRIMARY KEY,replica TEXT,last_sequence TEXT,snapshot TEXT)")
            db.execSQL("INSERT INTO state VALUES(1,'00000000-0000-4000-8000-000000000001','77','invalid legacy JSON')")
            db.execSQL("CREATE TABLE drafts(text TEXT)")
            db.execSQL("INSERT INTO drafts VALUES('Synthetic old draft')")
            db.execSQL("CREATE TABLE synthetic_settings(value TEXT)")
            db.execSQL("INSERT INTO synthetic_settings VALUES('Synthetic preserve')")
            db.version = 1
        }
        val store = open()
        assertEquals(0, store.snapshot().getJSONArray("records").length())
        assertEquals("78", store.allocateId().getString("sequence"))
        SQLiteDatabase.openDatabase(path.path, null, SQLiteDatabase.OPEN_READONLY).use { db ->
            db.rawQuery("SELECT value FROM synthetic_settings", null).use { assertTrue(it.moveToFirst()); assertEquals("Synthetic preserve", it.getString(0)) }
        }
    }
}
