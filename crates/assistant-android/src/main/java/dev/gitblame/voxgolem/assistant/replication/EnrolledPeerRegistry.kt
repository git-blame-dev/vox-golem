package dev.gitblame.voxgolem.assistant.replication

import android.content.ContentValues
import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteOpenHelper
import org.json.JSONObject
import java.io.Closeable
import java.io.File
import kotlin.math.min
import kotlin.random.Random

/** Native association lookup, injected by each Application. Node IDs are ephemeral routes. */
fun interface PeerAssociationProvider {
    fun replicaForNode(nodeId: String): String?
}

/** Implemented by the shell Application so cold framework listeners/jobs can resolve peers. */
interface AssistantPeerAssociationOwner {
    val assistantPeerAssociationProvider: PeerAssociationProvider
}

/** App-private replication bookkeeping; conversation records are stored separately. */
class EnrolledPeerRegistry(context: Context) : Closeable {
    private val database = ReplicationDatabase(context.applicationContext)

    fun replicas(): Set<String> = database.readableDatabase.rawQuery("SELECT replica FROM peers ORDER BY replica", null).use { cursor ->
        buildSet { while (cursor.moveToNext()) add(cursor.getString(0)) }
    }

    fun enroll(replica: String) {
        ReplicationWire.uuid(replica)
        val db = database.writableDatabase
        db.beginTransaction()
        try {
            val existing = replicas()
            require(replica in existing || existing.size < ReplicationWire.MAX_PEERS) { "Assistant peer limit reached" }
            val backlog = db.rawQuery("SELECT replica FROM cleanup_peers", null).use { cursor ->
                buildSet { while (cursor.moveToNext()) add(cursor.getString(0)) }
            }
            require(replica in existing || replica in backlog || backlog.size < ReplicationWire.MAX_PEERS) { "Assistant peer cleanup pending" }
            db.insertWithOnConflict("peers", null, ContentValues().apply { put("replica", replica) }, SQLiteDatabase.CONFLICT_IGNORE)
            db.delete("cleanup_peers", "replica = ?", arrayOf(replica))
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
    }

    fun revoke(replica: String) {
        ReplicationWire.uuid(replica)
        val db = database.writableDatabase
        db.beginTransaction()
        try {
            if (db.delete("peers", "replica = ?", arrayOf(replica)) != 0) {
                db.insertWithOnConflict("cleanup_peers", null, ContentValues().apply { put("replica", replica) }, SQLiteDatabase.CONFLICT_IGNORE)
            }
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
    }

    internal fun setupValue(key: String): JSONObject? = database.readableDatabase.rawQuery("SELECT value FROM setup WHERE key = ?", arrayOf(key)).use {
        if (it.moveToFirst()) JSONObject(it.getString(0)) else null
    }

    internal fun saveSetup(key: String, value: JSONObject?) {
        require(key in setOf("incoming", "outgoing", "status"))
        val raw = value?.toString()
        require(raw == null || raw.toByteArray(Charsets.UTF_8).size <= 4096)
        if (raw == null) database.writableDatabase.delete("setup", "key = ?", arrayOf(key))
        else check(database.writableDatabase.insertWithOnConflict("setup", null, ContentValues().apply { put("key", key); put("value", raw) }, SQLiteDatabase.CONFLICT_REPLACE) != -1L)
    }

    internal fun clearLegacySetup() {
        database.writableDatabase.delete("setup", "key IN ('incoming', 'outgoing')", null)
    }

    internal fun bindingFor(installation: String, replica: String): ApprovedPeerBinding? = database.readableDatabase.rawQuery(
        "SELECT association FROM native_bindings WHERE replica = ? AND replica = ?", arrayOf(replica, installation),
    ).use { if (it.moveToFirst()) ApprovedPeerBinding(replica, it.getString(0)) else null }

    internal fun rememberPeer(binding: ApprovedPeerBinding) {
        listOf(binding.replica, binding.association).forEach(ReplicationWire::uuid)
        val db = database.writableDatabase
        db.beginTransaction()
        try {
            replicas().filter { it != binding.replica }.forEach { revoke(it) }
            db.delete("native_bindings", "replica != ?", arrayOf(binding.replica))
            enroll(binding.replica)
            check(db.insertWithOnConflict("native_bindings", null, ContentValues().apply {
                put("replica", binding.replica); put("association", binding.association)
            }, SQLiteDatabase.CONFLICT_REPLACE) != -1L)
            db.setTransactionSuccessful()
        } finally { db.endTransaction() }
    }

    override fun close() = database.close()
}

internal class DurableReplicationState(context: Context) : ReplicationState, Closeable {
    private val database = ReplicationDatabase(context.applicationContext)
    override fun enrolledReplicas(): Set<String> = database.readableDatabase.rawQuery("SELECT replica FROM peers", null).use { cursor ->
        buildSet { while (cursor.moveToNext()) add(cursor.getString(0)) }
    }
    override fun load(peer: String): JSONObject? = database.readableDatabase.rawQuery("SELECT session FROM peers WHERE replica = ?", arrayOf(peer)).use {
        if (it.moveToFirst() && !it.isNull(0)) JSONObject(it.getString(0)) else null
    }
    override fun save(peer: String, state: JSONObject?) {
        ReplicationWire.uuid(peer)
        val raw = state?.toString()
        require(raw == null || raw.toByteArray(Charsets.UTF_8).size <= 64 * 1024)
        require(database.writableDatabase.update("peers", ContentValues().apply { put("session", raw) }, "replica = ?", arrayOf(peer)) == 1)
    }
    override fun cleanupReplicas(): Set<String> = database.readableDatabase.rawQuery("SELECT replica FROM cleanup_peers", null).use { cursor ->
        buildSet { while (cursor.moveToNext()) add(cursor.getString(0)) }
    }
    override fun cleanupComplete(peer: String) { database.writableDatabase.delete("cleanup_peers", "replica = ?", arrayOf(peer)) }
    fun retryDelay(now: Long): Long = database.readableDatabase.rawQuery("SELECT due FROM retry WHERE singleton = 1", null).use {
        check(it.moveToFirst()); (it.getLong(0) - now).coerceIn(0, MAX_BACKOFF)
    }
    fun failed(now: Long, random: Random = Random.Default): Long {
        val db = database.writableDatabase
        db.beginTransaction()
        try {
            val previous = db.rawQuery("SELECT attempts FROM retry WHERE singleton = 1", null).use { check(it.moveToFirst()); it.getInt(0) }
            val attempts = min(previous + 1, 10)
            val base = min(30_000L shl min(attempts - 1, 6), MAX_BACKOFF)
            val delay = (base + random.nextLong(-base / 10, base / 10 + 1)).coerceIn(30_000, MAX_BACKOFF)
            db.execSQL("UPDATE retry SET attempts = ?, due = ? WHERE singleton = 1", arrayOf<Any>(attempts, now + delay))
            db.setTransactionSuccessful()
            return delay
        } finally { db.endTransaction() }
    }
    fun succeeded() { database.writableDatabase.execSQL("UPDATE retry SET attempts = 0, due = 0 WHERE singleton = 1") }
    override fun close() = database.close()
    companion object { const val MAX_BACKOFF = 30 * 60 * 1000L }
}

private class ReplicationDatabase(context: Context) : SQLiteOpenHelper(context,
    File(context.noBackupFilesDir, "assistant-replication.db").absolutePath, null, 3) {
    override fun onConfigure(db: SQLiteDatabase) {
        db.execSQL("PRAGMA foreign_keys = ON")
        db.rawQuery("PRAGMA secure_delete = ON", null).use { check(it.moveToFirst()) }
    }
    override fun onCreate(db: SQLiteDatabase) {
        db.execSQL("CREATE TABLE peers (replica TEXT PRIMARY KEY, session TEXT)")
        db.execSQL("CREATE TABLE cleanup_peers (replica TEXT PRIMARY KEY)")
        db.execSQL("CREATE TABLE setup (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        db.execSQL("CREATE TABLE native_bindings (replica TEXT PRIMARY KEY, association TEXT NOT NULL)")
        db.execSQL("CREATE TABLE retry (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), attempts INTEGER NOT NULL, due INTEGER NOT NULL)")
        db.execSQL("INSERT INTO retry VALUES (1, 0, 0)")
    }
    override fun onUpgrade(db: SQLiteDatabase, oldVersion: Int, newVersion: Int) {
        check(oldVersion in 1..2 && newVersion == 3)
        db.delete("setup", null, null)
        db.execSQL("DROP TABLE IF EXISTS native_bindings")
        db.execSQL("CREATE TABLE native_bindings (replica TEXT PRIMARY KEY, association TEXT NOT NULL)")
    }
}

internal data class ApprovedPeerBinding(val replica: String, val association: String)
