package dev.gitblame.voxgolem.assistant

import android.content.ContentValues
import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteOpenHelper
import org.json.JSONArray
import org.json.JSONObject
import java.io.Closeable
import java.io.File
import java.math.BigInteger
import java.util.UUID

class AssistantRevisionBusyException : IllegalStateException("Assistant store revision changed repeatedly")
class AssistantCompletionSuppressedException : IllegalStateException("Completion deleted or quarantined; not saved")

/** Blocking IO. The caller must use an existing worker/dispatcher, never the UI thread. */
class AssistantStore(
    context: Context,
    gateway: DomainGateway = DomainGateway(AssistantDomainNative::apply),
) : Closeable {
    private val domain = AssistantDomain(gateway)
    private val helper = Database(context.applicationContext)
    internal var beforeCommit: (() -> Unit)? = null
    internal var beforeCompare: (() -> Unit)? = null

    @Synchronized fun view(): JSONObject = domain.evaluate("view", readSnapshot()) as JSONObject

    /** Trusted committed DB state only; there is deliberately no snapshot-import API. */
    @Synchronized fun snapshot(): JSONObject = JSONObject(readSnapshot())

    @Synchronized fun allocateId(): JSONObject = transaction { db ->
        db.rawQuery("SELECT replica, last_sequence FROM state WHERE singleton = 1", null).use { cursor ->
            check(cursor.moveToFirst())
            val next = BigInteger(cursor.getString(1)).add(BigInteger.ONE)
            check(next <= MAX_SEQUENCE) { "Assistant replica sequence exhausted" }
            db.execSQL("UPDATE state SET last_sequence = ? WHERE singleton = 1", arrayOf(next.toString()))
            JSONObject().put("replica", cursor.getString(0)).put("sequence", next.toString())
        }
    }

    @Synchronized fun createConversation(title: String): JSONObject {
        val record = record(allocateId(), JSONObject().put("kind", "conversation_created").put("title", title))
        commitRecords(JSONArray().put(record), false)
        return record
    }

    /** Peer input consists exclusively of ordinary records, never a snapshot or retired ledger. */
    @Synchronized fun importRecords(records: JSONArray): JSONObject = commitRecords(records, true)

    @Synchronized fun context(conversation: JSONObject, selectedHead: JSONObject?): JSONObject {
        val raw = readSnapshot()
        return domain.evaluate("context", raw, JSONObject().put("request", JSONObject()
            .put("expected_revision", JSONObject(raw).getString("revision"))
            .put("conversation", conversation).put("selected_head", selectedHead ?: JSONObject.NULL)
            .put("max_turns", 20).put("max_text_bytes", 256 * 1024))) as JSONObject
    }

    /** Context and request identity are pinned before generation; imports cannot reparent them. */
    @Synchronized fun completeTurn(
        request: JSONObject,
        context: JSONObject,
        prompt: String,
        answer: JSONObject,
        terminalStage: String,
        provenance: JSONObject,
        commitAdmission: (() -> Boolean) -> Boolean = { it() },
    ): JSONObject {
        val pinnedContext = JSONObject(context.toString())
        val pinnedProvenance = JSONObject(provenance.toString()).put("context", pinnedContext.getJSONArray("ancestor_ids"))
        val body = JSONObject().put("kind", "turn_completed")
            .put("conversation", pinnedContext.getJSONObject("conversation"))
            .put("parent", pinnedContext.get("parent")).put("request", JSONObject(request.toString())).put("prompt", prompt)
            .put("answer", JSONObject(answer.toString())).put("terminal_stage", terminalStage).put("provenance", pinnedProvenance)
        val result = record(allocateId(), body)
        val plan = commitRecords(JSONArray().put(result), false, commitAdmission, result)
        val id = result.getJSONObject("operation").getJSONObject("id")
        if (containsId(plan.getJSONArray("suppressed"), id) || containsId(quarantinedIds(plan), id)) {
            throw AssistantCompletionSuppressedException()
        }
        return result
    }

    @Synchronized fun selectConversation(conversation: JSONObject?, selectedHead: JSONObject?) {
        require(conversation != null || selectedHead == null)
        if (selectedHead != null) idKey(selectedHead)
        transaction { db ->
            if (conversation != null) checkNotDeleted(db, conversation)
            db.execSQL("UPDATE local_state SET conversation = ?, selected_head = ? WHERE singleton = 1",
                arrayOf(conversation?.toString(), selectedHead?.toString()))
        }
    }

    @Synchronized fun localState(): JSONObject = helper.readableDatabase.rawQuery(
        "SELECT conversation, selected_head FROM local_state WHERE singleton = 1", null,
    ).use {
        check(it.moveToFirst())
        JSONObject().put("conversation", if (it.isNull(0)) JSONObject.NULL else JSONObject(it.getString(0)))
            .put("selected_head", if (it.isNull(1)) JSONObject.NULL else JSONObject(it.getString(1)))
    }

    /** Null conversation is a fresh, not-yet-created draft. Interrupted never implies replay. */
    @Synchronized fun saveDraft(conversation: JSONObject?, text: String, interrupted: Boolean = false) {
        require(text.toByteArray(Charsets.UTF_8).size <= 32 * 1024) { "Assistant draft too large" }
        transaction { db ->
            if (conversation != null) checkNotDeleted(db, conversation)
            check(db.insertWithOnConflict("drafts", null, ContentValues().apply {
                put("conversation_key", conversation?.let(::idKey) ?: NEW_DRAFT)
                put("text", text)
                put("interrupted", if (interrupted) 1 else 0)
            }, SQLiteDatabase.CONFLICT_REPLACE) != -1L) { "Assistant draft not saved" }
            checkQuota(db, "drafts", "length(CAST(text AS BLOB))", MAX_LOCAL_BYTES, MAX_LEDGER_ROWS)
        }
    }

    @Synchronized fun draft(conversation: JSONObject?): JSONObject = helper.readableDatabase.rawQuery(
        "SELECT text, interrupted FROM drafts WHERE conversation_key = ?",
        arrayOf(conversation?.let(::idKey) ?: NEW_DRAFT),
    ).use {
        if (it.moveToFirst()) JSONObject().put("text", it.getString(0)).put("interrupted", it.getInt(1) != 0)
        else JSONObject().put("text", "").put("interrupted", false)
    }

    /** Committed publications only. Exact peer receipts filter delivery, not greatest sequence. */
    @Synchronized fun publications(
        peerReplica: String? = null,
        limit: Int = 128,
        maxBytes: Int = AssistantDomain.MAX_SNAPSHOT_BYTES,
        after: JSONObject? = null,
    ): JSONArray {
        require(limit in 1..128 && maxBytes in 1..AssistantDomain.MAX_SNAPSHOT_BYTES)
        val args = mutableListOf(after?.let(::idKey) ?: "")
        val peerFilter = if (peerReplica == null) "" else {
            args.add(peerReplica)
            " AND NOT EXISTS (SELECT 1 FROM peer_receipts r WHERE r.peer = ? AND r.operation_key = o.operation_key AND r.body_key = o.body_key)"
        }
        return boundedRows("outbox", "SELECT o.operation_key, length(CAST(o.payload AS BLOB)) FROM outbox o WHERE o.operation_key > ?$peerFilter ORDER BY o.operation_key LIMIT $limit",
            args.toTypedArray(), maxBytes)
    }

    @Synchronized fun receiptsToPublish(limit: Int = 128, after: JSONObject? = null): JSONArray {
        require(limit in 1..128)
        return boundedRows("receipt_outbox", "SELECT operation_key, length(CAST(payload AS BLOB)) FROM receipt_outbox WHERE operation_key > ? ORDER BY operation_key LIMIT $limit",
            arrayOf(after?.let(::idKey) ?: ""), 32 * 1024)
    }

    /** Authentication/association is the transport caller's responsibility before this call. */
    @Synchronized fun acknowledge(peerReplica: String, receipts: JSONArray) {
        require(receipts.length() <= 128 && receipts.toString().toByteArray(Charsets.UTF_8).size <= 32 * 1024)
        val ids = JSONArray()
        for (i in 0 until receipts.length()) ids.put(receipts.getJSONObject(i).getJSONObject("operation"))
        repeat(MAX_RETRIES) {
            val raw = readSnapshot()
            val revision = JSONObject(raw).getString("revision")
            val expected = domain.evaluate("receipts", raw, JSONObject().put("recipient", peerReplica).put("operation_ids", ids)) as JSONArray
            for (i in 0 until receipts.length()) {
                val receipt = receipts.getJSONObject(i)
                val actual = expected.getJSONObject(i)
                require(receipt.length() == 4 && receipt.get("version") == actual.get("version") &&
                    receipt.getString("recipient_replica") == actual.getString("recipient_replica") &&
                    receipt.getString("body_key") == actual.getString("body_key") &&
                    sameId(receipt.getJSONObject("operation"), actual.getJSONObject("operation"))) { "Invalid exact-body receipt" }
            }
            if (transaction { db ->
                    if (readRevision(db) != revision) return@transaction false
                    for (i in 0 until expected.length()) {
                        val receipt = expected.getJSONObject(i)
                        db.insertWithOnConflict("peer_receipts", null, ContentValues().apply {
                            put("peer", peerReplica)
                            put("operation_key", idKey(receipt.getJSONObject("operation")))
                            put("body_key", receipt.getString("body_key"))
                        }, SQLiteDatabase.CONFLICT_IGNORE)
                    }
                    checkQuota(db, "peer_receipts", "0", 0, MAX_LEDGER_ROWS)
                    beforeCommit?.invoke()
                    true
                }) return
        }
        throw AssistantRevisionBusyException()
    }

    private fun record(id: JSONObject, body: JSONObject): JSONObject = domain.evaluate("record", fields = JSONObject()
        .put("operation", JSONObject().put("version", 1).put("id", id).put("body", body))) as JSONObject

    private fun commitRecords(input: JSONArray, receive: Boolean, commitAdmission: (() -> Boolean) -> Boolean = { it() }, completion: JSONObject? = null): JSONObject {
        require(input.length() <= 128)
        val records = JSONArray(input.toString())
        repeat(MAX_RETRIES) {
            val raw = readSnapshot()
            val plan = domain.evaluate("plan", raw, JSONObject().put("command", JSONObject()
                .put("kind", "import").put("expected_revision", JSONObject(raw).getString("revision"))
                .put("records", records))) as JSONObject
            val snapshot = plan.getJSONObject("snapshot")
            val quarantined = quarantinedIds(plan)
            val next = snapshot.toString()
            check(next.toByteArray(Charsets.UTF_8).size <= AssistantDomain.MAX_SNAPSHOT_BYTES) { "Assistant snapshot too large" }
            val ids = JSONArray()
            for (i in 0 until records.length()) ids.put(records.getJSONObject(i).getJSONObject("operation").getJSONObject("id"))
            val receipts = if (receive) domain.evaluate("receipts", next, JSONObject()
                .put("recipient", replica()).put("operation_ids", ids)) as JSONArray else JSONArray()
            beforeCompare?.invoke()
            // Cancellation can win before admission. Once admitted, the whole SQLite transaction
            // (including endTransaction) wins; a later Stop cannot undo a durable completion.
            val committed = commitAdmission { transaction { db ->
                if (readRevision(db) != plan.getString("base_revision")) return@transaction false
                db.execSQL("UPDATE state SET revision = ?, snapshot = ? WHERE singleton = 1",
                    arrayOf(snapshot.getString("revision"), next))
                replaceLedger(db, snapshot)
                val purges = plan.getJSONArray("purge_conversations")
                for (i in 0 until purges.length()) {
                    val conversation = purges.getJSONObject(i)
                    val key = idKey(conversation)
                    db.delete("outbox", "conversation_key = ? AND tombstone = 0", arrayOf(key))
                    db.delete("drafts", "conversation_key = ?", arrayOf(key))
                    val selected = db.rawQuery("SELECT conversation FROM local_state WHERE singleton = 1", null).use {
                        it.moveToFirst(); if (it.isNull(0)) null else JSONObject(it.getString(0))
                    }
                    if (selected != null && sameId(selected, conversation)) {
                        db.execSQL("UPDATE local_state SET conversation = NULL, selected_head = NULL WHERE singleton = 1")
                    }
                }
                val publications = plan.getJSONArray("publish")
                for (i in 0 until publications.length()) {
                    val publication = publications.getJSONObject(i)
                    val operation = publication.getJSONObject("operation")
                    val body = operation.getJSONObject("body")
                    db.insertWithOnConflict("outbox", null, ContentValues().apply {
                        put("operation_key", idKey(operation.getJSONObject("id")))
                        put("conversation_key", idKey(conversationOf(operation)))
                        put("body_key", publication.getString("body_key"))
                        put("tombstone", if (body.getString("kind") == "conversation_deleted") 1 else 0)
                        put("payload", publication.toString())
                    }, SQLiteDatabase.CONFLICT_IGNORE)
                }
                for (i in 0 until quarantined.length()) {
                    db.delete("outbox", "operation_key = ?", arrayOf(idKey(quarantined.getJSONObject(i))))
                }
                for (i in 0 until receipts.length()) {
                    val receipt = receipts.getJSONObject(i)
                    db.insertWithOnConflict("receipt_outbox", null, ContentValues().apply {
                        put("operation_key", idKey(receipt.getJSONObject("operation")))
                        put("body_key", receipt.getString("body_key"))
                        put("payload", receipt.toString())
                    }, SQLiteDatabase.CONFLICT_IGNORE)
                }
                checkQuota(db, "outbox", "length(CAST(payload AS BLOB))", MAX_OUTBOX_BYTES, MAX_LEDGER_ROWS)
                checkQuota(db, "receipt_outbox", "length(CAST(payload AS BLOB))", MAX_OUTBOX_BYTES, MAX_LEDGER_ROWS)
                completion?.getJSONObject("operation")?.let { operation ->
                    val id = operation.getJSONObject("id")
                    if (!containsId(plan.getJSONArray("suppressed"), id) && !containsId(quarantined, id)) {
                        val body = operation.getJSONObject("body")
                        // Consume only the admitted prompt, atomically with its durable completion.
                        // A failed local selection/read afterwards cannot resurrect the sent draft.
                        db.delete("drafts", "conversation_key = ? AND text = ?", arrayOf(
                            idKey(body.getJSONObject("conversation")), body.getString("prompt")))
                    }
                }
                beforeCommit?.invoke()
                true
            } }
            if (committed) return plan
        }
        throw AssistantRevisionBusyException()
    }

    private fun replaceLedger(db: SQLiteDatabase, snapshot: JSONObject) {
        db.delete("operations", null, null)
        val retained = snapshot.getJSONArray("records")
        for (i in 0 until retained.length()) {
            val record = retained.getJSONObject(i)
            val operation = record.getJSONObject("operation")
            insertLedger(db, operation.getJSONObject("id"), conversationOf(operation), record.getString("body_key"),
                false, operation.getJSONObject("body").getString("kind") == "conversation_deleted")
        }
        val retired = snapshot.getJSONArray("retired")
        for (i in 0 until retired.length()) {
            val record = retired.getJSONObject(i)
            insertLedger(db, record.getJSONObject("id"), record.getJSONObject("conversation"), record.getString("body_key"), true, false)
        }
    }

    private fun insertLedger(db: SQLiteDatabase, id: JSONObject, conversation: JSONObject, bodyKey: String, retired: Boolean, tombstone: Boolean) {
        db.insertOrThrow("operations", null, ContentValues().apply {
            put("operation_key", idKey(id)); put("conversation_key", idKey(conversation)); put("body_key", bodyKey)
            put("retired", if (retired) 1 else 0); put("tombstone", if (tombstone) 1 else 0)
        })
    }

    private fun checkNotDeleted(db: SQLiteDatabase, conversation: JSONObject) {
        db.rawQuery("SELECT 1 FROM operations WHERE conversation_key = ? AND tombstone = 1 LIMIT 1", arrayOf(idKey(conversation))).use {
            check(!it.moveToFirst()) { "Conversation deleted" }
        }
    }

    @Synchronized fun replica(): String = helper.readableDatabase.rawQuery("SELECT replica FROM state WHERE singleton = 1", null).use {
        check(it.moveToFirst()); it.getString(0)
    }

    private fun readRevision(db: SQLiteDatabase): String = db.rawQuery("SELECT revision FROM state WHERE singleton = 1", null).use {
        check(it.moveToFirst()); it.getString(0)
    }

    private fun readSnapshot(): String {
        val db = helper.readableDatabase
        return transaction(db) {
            db.rawQuery("SELECT length(CAST(snapshot AS BLOB)) FROM state WHERE singleton = 1", null).use {
                check(it.moveToFirst() && it.getLong(0) <= AssistantDomain.MAX_SNAPSHOT_BYTES) { "Assistant committed snapshot too large" }
            }
            // A snapshot can exceed Android's usual 2 MiB CursorWindow. Scalar statement IO
            // avoids placing the bounded 4 MiB value into a cursor row.
            val raw = db.compileStatement("SELECT snapshot FROM state WHERE singleton = 1").use { it.simpleQueryForString() }
            check(JSONObject(raw).getString("revision") == readRevision(db)) { "Assistant DB revision mismatch" }
            raw
        }
    }

    private fun boundedRows(table: String, sql: String, args: Array<String>, maxBytes: Int): JSONArray = transaction { db ->
        val result = JSONArray()
        var bytes = 2L
        db.rawQuery(sql, args).use {
            while (it.moveToNext()) {
                val size = it.getLong(1) + if (result.length() == 0) 0 else 1
                if (bytes + size > maxBytes) {
                    check(result.length() != 0) { "Assistant publication exceeds page byte limit" }
                    break
                }
                bytes += size
                val payload = db.compileStatement("SELECT payload FROM $table WHERE operation_key = ?").use { statement ->
                    statement.bindString(1, it.getString(0))
                    statement.simpleQueryForString()
                }
                result.put(JSONObject(payload))
            }
        }
        result
    }

    private fun checkQuota(db: SQLiteDatabase, table: String, bytes: String, maxBytes: Int, maxRows: Int) {
        db.rawQuery("SELECT COUNT(*), COALESCE(SUM($bytes), 0) FROM $table", null).use {
            check(it.moveToFirst() && it.getLong(0) <= maxRows && it.getLong(1) <= maxBytes) { "Assistant storage quota exceeded" }
        }
    }

    private fun <T> transaction(block: (SQLiteDatabase) -> T): T = transaction(helper.writableDatabase, block)

    private fun <T> transaction(db: SQLiteDatabase, block: (SQLiteDatabase) -> T): T {
        db.beginTransaction()
        try {
            val result = block(db)
            db.setTransactionSuccessful()
            return result
        } finally { db.endTransaction() }
    }

    @Synchronized override fun close() { helper.close() }

    private class Database(context: Context) : SQLiteOpenHelper(context, File(context.noBackupFilesDir, "assistant.db").absolutePath, null, 1) {
        override fun onConfigure(db: SQLiteDatabase) {
            db.rawQuery("PRAGMA secure_delete = ON", null).use { check(it.moveToFirst() && it.getInt(0) == 1) }
        }
        override fun onCreate(db: SQLiteDatabase) {
            db.execSQL("CREATE TABLE state (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), replica TEXT NOT NULL, last_sequence TEXT NOT NULL, revision TEXT NOT NULL, snapshot TEXT NOT NULL)")
            db.execSQL("CREATE TABLE operations (operation_key TEXT PRIMARY KEY, conversation_key TEXT NOT NULL, body_key TEXT NOT NULL, retired INTEGER NOT NULL, tombstone INTEGER NOT NULL)")
            db.execSQL("CREATE INDEX operations_conversation ON operations(conversation_key, tombstone)")
            db.execSQL("CREATE TABLE outbox (operation_key TEXT PRIMARY KEY, conversation_key TEXT NOT NULL, body_key TEXT NOT NULL, tombstone INTEGER NOT NULL, payload TEXT NOT NULL)")
            db.execSQL("CREATE INDEX outbox_conversation ON outbox(conversation_key)")
            db.execSQL("CREATE TABLE receipt_outbox (operation_key TEXT PRIMARY KEY, body_key TEXT NOT NULL, payload TEXT NOT NULL)")
            db.execSQL("CREATE TABLE peer_receipts (peer TEXT NOT NULL, operation_key TEXT NOT NULL, body_key TEXT NOT NULL, PRIMARY KEY(peer, operation_key))")
            db.execSQL("CREATE TABLE local_state (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), conversation TEXT, selected_head TEXT)")
            db.execSQL("CREATE TABLE drafts (conversation_key TEXT PRIMARY KEY, text TEXT NOT NULL, interrupted INTEGER NOT NULL)")
            db.execSQL("INSERT INTO state VALUES (1, ?, '0', '0', ?)", arrayOf(UUID.randomUUID().toString(),
                "{\"version\":1,\"revision\":\"0\",\"records\":[],\"retired\":[]}"))
            db.execSQL("INSERT INTO local_state VALUES (1, NULL, NULL)")
        }
        override fun onUpgrade(db: SQLiteDatabase, oldVersion: Int, newVersion: Int) {
            error("Assistant DB migration required")
        }
    }

    companion object {
        private const val MAX_RETRIES = 8
        private const val MAX_LEDGER_ROWS = 65_536
        private const val MAX_OUTBOX_BYTES = 8 * 1024 * 1024
        private const val MAX_LOCAL_BYTES = 4 * 1024 * 1024
        private const val NEW_DRAFT = "new"
        private val MAX_SEQUENCE = BigInteger("18446744073709551615")
        private fun quarantinedIds(plan: JSONObject): JSONArray =
            if (plan.has("quarantined")) plan.getJSONArray("quarantined") else JSONArray()
        private fun containsId(ids: JSONArray, id: JSONObject): Boolean =
            (0 until ids.length()).any { sameId(ids.getJSONObject(it), id) }
        private fun idKey(id: JSONObject): String {
            require(id.length() == 2 && id.get("replica") is String && id.get("sequence") is String)
            val replica = id.getString("replica")
            val sequence = id.getString("sequence")
            require(replica.length <= 36 && sequence.length <= 20 && '/' !in replica && '/' !in sequence)
            return "$replica/$sequence"
        }
        private fun sameId(first: JSONObject, second: JSONObject): Boolean =
            first.length() == 2 && second.length() == 2 && idKey(first) == idKey(second)
        private fun conversationOf(operation: JSONObject): JSONObject = operation.getJSONObject("body").let {
            if (it.getString("kind") == "conversation_created") operation.getJSONObject("id") else it.getJSONObject("conversation")
        }
    }
}
