package dev.gitblame.voxgolem.assistant

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.io.Closeable
import java.io.File

class AssistantRevisionBusyException : IllegalStateException("Assistant store revision changed repeatedly")
class AssistantCompletionSuppressedException : IllegalStateException("Completion deleted or quarantined; not saved")
internal data class CommittedSnapshot(val rawJson: String, val revision: String)

/** No canonical Kotlin cache or SQLite implementation. All calls are blocking background IO. */
class AssistantStore(
    context: Context,
    gateway: DomainGateway = DomainGateway(AssistantDomainNative::apply),
) : Closeable {
    private val domain = AssistantDomain(gateway)
    // Lazy open: constructing the shell on UI does not perform SQLite IO.
    private val path = File(context.noBackupFilesDir, "assistant.db").absolutePath
    private var handle = 0L
    private var closed = false
    internal var beforeCommit: (() -> Unit)? = null
    internal var beforeCompare: (() -> Unit)? = null
    internal var beforeSelection: (() -> Unit)? = null

    private fun call(kind: String, fields: JSONObject = JSONObject()): Any {
        val raw = executeRaw(JSONObject(fields.toString()).put("kind", kind).toString())
        val response = JSONObject(raw)
        if (!response.getBoolean("ok")) throw AssistantDomainException(response.getJSONObject("error"))
        return response.get("value")
    }

    private fun executeRaw(request: String): String {
        check(!closed) { "Assistant store closed" }
        if (handle == 0L) handle = AssistantStoreNative.open(path)
        require(request.toByteArray(Charsets.UTF_8).size <= AssistantDomain.MAX_GATEWAY_BYTES)
        val raw = AssistantStoreNative.execute(handle, request)
        check(raw.toByteArray(Charsets.UTF_8).size <= AssistantDomain.MAX_GATEWAY_BYTES)
        return raw
    }

    @Synchronized internal fun evaluateOwnedRead(request: String): String =
        executeRaw(JSONObject().put("kind", "read").put("request", JSONObject(request)).toString())

    @Synchronized internal fun read(kind: String, fields: JSONObject = JSONObject()): Any =
        call("read", JSONObject().put("request", JSONObject(fields.toString()).put("kind", kind)))

    @Synchronized fun view(): JSONObject = read("view") as JSONObject
    /** Explicit trusted export for replication/diagnostics; never used by ordinary projections. */
    @Synchronized fun snapshot(): JSONObject = call("snapshot") as JSONObject
    @Synchronized internal fun committedSnapshot(): CommittedSnapshot = snapshot().let { CommittedSnapshot(it.toString(), it.getString("revision")) }
    @Synchronized fun allocateId(): JSONObject = call("allocate_id") as JSONObject
    @Synchronized fun replica(): String = call("replica") as String
    @Synchronized fun conversationPage(
        offset: Int = 0,
        limit: Int = 128,
        expectedRevision: String? = null,
    ): JSONObject =
        call(
            "conversations",
            JSONObject()
                .put("offset", offset)
                .put("limit", limit)
                .apply {
                    if (expectedRevision != null) {
                        put("revision", expectedRevision)
                    }
                },
        ) as JSONObject

    @Synchronized fun headPage(
        conversation: JSONObject,
        offset: Int = 0,
        limit: Int = 128,
        expectedRevision: String? = null,
        selectedHead: JSONObject? = null,
    ): JSONObject =
        call(
            "heads",
            JSONObject()
                .put("conversation", conversation)
                .put("offset", offset)
                .put("limit", limit)
                .apply {
                    if (expectedRevision != null || selectedHead != null) {
                        put("revision", expectedRevision ?: JSONObject.NULL)
                        put("selected_head", selectedHead ?: JSONObject.NULL)
                    }
                },
        ) as JSONObject

    @Synchronized fun createConversation(title: String): JSONObject {
        val record = record(allocateId(), JSONObject().put("kind", "conversation_created").put("title", title))
        commitRecords(JSONArray().put(record), false)
        return record
    }

    /** Authenticated ordinary records only: no snapshot/retired-ledger import entry point. */
    @Synchronized fun importRecords(records: JSONArray): JSONObject = commitRecords(records, true)

    @Synchronized fun context(conversation: JSONObject, selectedHead: JSONObject?): JSONObject =
        call("context", JSONObject().put("conversation", conversation).put("selected_head", selectedHead ?: JSONObject.NULL)) as JSONObject

    @Synchronized internal fun project(conversation: JSONObject?, head: JSONObject?, busy: Boolean,
        read: AssistantProjectionRead? = null): AssistantProjection {
        val value = this.read("projection", JSONObject().put("request", JSONObject()
            .put("conversation", conversation ?: JSONObject.NULL).put("selected_head", head ?: JSONObject.NULL)
            .put("local_work_in_flight", busy).put("read", read?.request() ?: JSONObject.NULL))) as JSONObject
        return AssistantProjection(value.getJSONObject("view"), value.optJSONObject("continuation"), value.optJSONObject("read"))
    }

    /** Pinned identity/ancestry survive imports. Admission lock encloses native SQLite COMMIT. */
    @Synchronized fun completeTurn(
        request: JSONObject,
        context: JSONObject,
        prompt: String,
        answer: JSONObject,
        terminalStage: String,
        provenance: JSONObject,
        sources: JSONArray = JSONArray(),
        commitAdmission: (() -> Boolean) -> Boolean = { it() },
    ): JSONObject {
        val pinned = JSONObject(context.toString())
        val body = JSONObject().put("kind", "turn_completed")
            .put("conversation", pinned.getJSONObject("conversation"))
            .put("parent", pinned.get("parent")).put("request", JSONObject(request.toString()))
            .put("prompt", prompt).put("answer", JSONObject(answer.toString())).put("sources", JSONArray(sources.toString()))
            .put("terminal_stage", terminalStage)
            .put("provenance", JSONObject(provenance.toString()).put("context", pinned.getJSONArray("ancestor_ids")))
        beforeCompare?.invoke()
        var result: JSONObject? = null
        val admitted = commitAdmission {
            beforeCommit?.invoke()
            result = call("complete", JSONObject().put("completion", body)) as JSONObject
            true
        }
        if (!admitted) throw AssistantRevisionBusyException()
        val completed = checkNotNull(result)
        if (!completed.getBoolean("saved")) throw AssistantCompletionSuppressedException()
        return completed.getJSONObject("record")
    }

    @Synchronized fun selectConversation(conversation: JSONObject?, selectedHead: JSONObject?) {
        beforeSelection?.invoke()
        call("select", JSONObject().put("selection", selection(conversation, selectedHead)))
    }
    @Synchronized fun localState(): JSONObject = call("local_state") as JSONObject
    @Synchronized internal fun projectionStillCurrent(revision: String, conversation: String?, head: String?): Boolean =
        call("projection_still_current", JSONObject().put("revision", revision)
            .put("selection", selection(conversation?.let(::JSONObject), head?.let(::JSONObject)))) as Boolean

    @Synchronized fun saveDraft(conversation: JSONObject?, text: String, interrupted: Boolean = false) {
        require(AssistantDraftBudget.accepts(text)) { "Assistant draft too large" }
        call("save_draft", JSONObject().put("conversation", conversation ?: JSONObject.NULL).put("text", text).put("interrupted", interrupted))
    }
    @Synchronized fun draft(conversation: JSONObject?): JSONObject =
        call("draft", JSONObject().put("conversation", conversation ?: JSONObject.NULL)) as JSONObject

    @Synchronized fun publications(peerReplica: String? = null, limit: Int = 128,
        maxBytes: Int = AssistantDomain.MAX_SNAPSHOT_BYTES, after: JSONObject? = null): JSONArray =
        call("publications", JSONObject().put("peer", peerReplica ?: JSONObject.NULL).put("limit", limit)
            .put("max_bytes", maxBytes).put("after", after ?: JSONObject.NULL)) as JSONArray
    @Synchronized fun receiptsToPublish(limit: Int = 128, after: JSONObject? = null): JSONArray =
        call("receipts_to_publish", JSONObject().put("limit", limit).put("after", after ?: JSONObject.NULL)) as JSONArray
    @Synchronized fun acknowledge(peerReplica: String, receipts: JSONArray) {
        beforeCommit?.invoke()
        call("acknowledge", JSONObject().put("peer", peerReplica).put("receipts", receipts))
    }

    private fun record(id: JSONObject, body: JSONObject): JSONObject = domain.evaluate("record", fields = JSONObject()
        .put("operation", JSONObject().put("version", 2).put("id", id).put("body", body))) as JSONObject

    private fun commitRecords(records: JSONArray, receive: Boolean,
        commitAdmission: (() -> Boolean) -> Boolean = { it() }, completion: JSONObject? = null): JSONObject {
        require(records.length() <= 128)
        val fields = JSONObject().put("records", JSONArray(records.toString())).put("receive", receive)
            .put("completion", completion ?: JSONObject.NULL)
        beforeCompare?.invoke()
        var result: JSONObject? = null
        val admitted = commitAdmission {
            beforeCommit?.invoke()
            result = call("commit", fields) as JSONObject
            true
        }
        if (!admitted) throw AssistantRevisionBusyException()
        return checkNotNull(result)
    }

    @Synchronized override fun close() {
        if (handle != 0L) AssistantStoreNative.close(handle)
        handle = 0L
        closed = true
    }
    companion object {
        private fun selection(conversation: JSONObject?, head: JSONObject?) = JSONObject()
            .put("conversation", conversation ?: JSONObject.NULL).put("selected_head", head ?: JSONObject.NULL)
    }
}

internal object AssistantStoreNative {
    init { System.loadLibrary("wear_assistant_native") }
    external fun open(path: String): Long
    external fun execute(handle: Long, request: String): String
    external fun close(handle: Long)
}
