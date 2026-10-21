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
    private val collecting = ThreadLocal<MutableMap<String, AssistantDiagnosticJournal.Metric>?>()

    private inline fun <T> measure(phase: String, action: () -> T): T {
        val metrics = collecting.get() ?: return action()
        val start = android.os.SystemClock.elapsedRealtimeNanos()
        try { return action() } finally {
            val ns = (android.os.SystemClock.elapsedRealtimeNanos() - start).coerceAtLeast(0)
            val old = metrics[phase]
            metrics[phase] = AssistantDiagnosticJournal.Metric(((old?.count ?: 0) + 1).coerceAtMost(1_000_000),
                (old?.totalNs ?: 0).let { if (Long.MAX_VALUE - it < ns) Long.MAX_VALUE else it + ns }, maxOf(old?.maxNs ?: 0, ns))
        }
    }

    private inline fun <T> locked(call: AssistantDiagnosticJournal.StorageCall, action: () -> T): T {
        if (!AssistantTiming.storageEnabled() || collecting.get() != null) return synchronized(this) { action() }
        val metrics = linkedMapOf<String, AssistantDiagnosticJournal.Metric>()
        collecting.set(metrics)
        var outcome = AssistantDiagnosticJournal.StorageOutcome.ERROR
        try {
            return measure("store_total") {
                val start = android.os.SystemClock.elapsedRealtimeNanos()
                synchronized(this) {
                    val wait = (android.os.SystemClock.elapsedRealtimeNanos() - start).coerceAtLeast(0)
                    metrics["store_lock_wait"] = AssistantDiagnosticJournal.Metric(1, wait, wait)
                    action().also { outcome = AssistantDiagnosticJournal.StorageOutcome.OK }
                }
            }
        } finally {
            collecting.remove()
            AssistantTiming.storage(AssistantDiagnosticJournal.Storage(call, outcome, metrics.toMap()))
        }
    }

    private fun call(kind: String, fields: JSONObject = JSONObject()): Any {
        val request = measure("kotlin_request_encode") { JSONObject(fields.toString()).put("kind", kind).toString() }
        val raw = executeRaw(request)
        val response = measure("kotlin_response_decode") { JSONObject(raw) }
        if (!response.getBoolean("ok")) throw AssistantDomainException(response.getJSONObject("error"))
        return response.get("value")
    }

    private fun executeRaw(request: String): String {
        check(!closed) { "Assistant store closed" }
        if (handle == 0L) handle = measure("native_open") {
            if (collecting.get() == null) AssistantStoreNative.open(path)
            else {
                val opened = AssistantStoreNative.openProfiled(path)
                collectNative(opened)
                val response = measure("kotlin_response_decode") { JSONObject(opened) }
                check(response.getBoolean("ok")) { "conversation_storage_open" }
                response.getLong("value")
            }
        }
        require(request.toByteArray(Charsets.UTF_8).size <= AssistantDomain.MAX_GATEWAY_BYTES)
        val raw = measure("jni_execute") {
            if (collecting.get() == null) AssistantStoreNative.execute(handle, request)
            else AssistantStoreNative.executeProfiled(handle, request)
        }
        check(raw.toByteArray(Charsets.UTF_8).size <= AssistantDomain.MAX_GATEWAY_BYTES)
        collectNative(raw)
        return raw
    }

    private fun collectNative(raw: String) {
        if (collecting.get() != null) {
            // Optional fixed numeric diagnostics never enter the domain payload.
            runCatching {
                val summary = measure("kotlin_response_decode") { JSONObject(raw).optJSONObject("storageTiming") }
                if (summary?.optInt("schema") == 1) {
                    val metrics = summary.getJSONObject("metrics")
                    AssistantDiagnosticJournal.STORAGE_PHASES.forEach { key ->
                        metrics.optJSONObject(key)?.let {
                            val count = it.getLong("count"); val total = it.getLong("total_ns"); val max = it.getLong("max_ns")
                            if (count in 0..1_000_000L && total >= 0 && max in 0..total &&
                                (count > 0 || (total == 0L && max == 0L))) {
                                val old = collecting.get()!![key]
                                collecting.get()!![key] = AssistantDiagnosticJournal.Metric(
                                    ((old?.count ?: 0) + count).coerceAtMost(1_000_000),
                                    (old?.totalNs ?: 0).let { n -> if (Long.MAX_VALUE - n < total) Long.MAX_VALUE else n + total },
                                    maxOf(old?.maxNs ?: 0, max))
                            }
                        }
                    }
                }
            }
        }
    }

    internal fun evaluateOwnedRead(request: String): String = locked(AssistantDiagnosticJournal.StorageCall.PROJECTION) {
        executeRaw(measure("kotlin_request_encode") { JSONObject().put("kind", "read").put("request", JSONObject(request)).toString() })
    }

    internal fun read(kind: String, fields: JSONObject = JSONObject()): Any = locked(AssistantDiagnosticJournal.StorageCall.READ) {
        call("read", JSONObject().put("request", JSONObject(fields.toString()).put("kind", kind)))
    }

    fun view(): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.PROJECTION) { read("view") as JSONObject }
    /** Explicit trusted export for replication/diagnostics; never used by ordinary projections. */
    fun snapshot(): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.SNAPSHOT) { call("snapshot") as JSONObject }
    internal fun committedSnapshot(): CommittedSnapshot = locked(AssistantDiagnosticJournal.StorageCall.SNAPSHOT) { snapshot().let { CommittedSnapshot(it.toString(), it.getString("revision")) } }
    fun allocateId(): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.ALLOCATE_ID) { call("allocate_id") as JSONObject }
    fun replica(): String = locked(AssistantDiagnosticJournal.StorageCall.REPLICA) { call("replica") as String }
    internal fun replication(request: JSONObject): Any = locked(AssistantDiagnosticJournal.StorageCall.READ) {
        if (request.getString("kind") == "import") {
            beforeCompare?.invoke()
            beforeCommit?.invoke()
        }
        call("replication", JSONObject().put("request", request))
    }
    fun conversationPage(
        offset: Int = 0,
        limit: Int = 128,
        expectedRevision: String? = null,
    ): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.CONVERSATIONS) {
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
    }

    fun headPage(
        conversation: JSONObject,
        offset: Int = 0,
        limit: Int = 128,
        expectedRevision: String? = null,
        selectedHead: JSONObject? = null,
    ): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.HEADS) {
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
    }

    fun createConversation(title: String): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.CREATE_CONVERSATION) {
        val record = record(allocateId(), JSONObject().put("kind", "conversation_created").put("title", title))
        commitRecords(JSONArray().put(record), false)
        record
    }

    /** Authenticated ordinary records only: no snapshot/retired-ledger import entry point. */
    fun importRecords(records: JSONArray): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.COMMIT) { commitRecords(records, true) }

    fun context(conversation: JSONObject, selectedHead: JSONObject?): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.CONTEXT) {
        call("context", JSONObject().put("conversation", conversation).put("selected_head", selectedHead ?: JSONObject.NULL)) as JSONObject
    }

    internal fun prepareMobile(conversation: JSONObject?, selectedHead: JSONObject?, prompt: String, strictParent: Boolean): JSONObject =
        locked(AssistantDiagnosticJournal.StorageCall.CONTEXT) {
            beforeCompare?.invoke()
            beforeCommit?.invoke()
            call("prepare_mobile", JSONObject().put("request", JSONObject()
                .put("selection", selection(conversation, selectedHead)).put("prompt", prompt)
                .put("total_text_bytes", 256 * 1024).put("strict_parent", strictParent))) as JSONObject
        }

    internal fun project(conversation: JSONObject?, head: JSONObject?, busy: Boolean,
        read: AssistantProjectionRead? = null): AssistantProjection = locked(AssistantDiagnosticJournal.StorageCall.PROJECTION) {
        val value = this.read("projection", JSONObject().put("request", JSONObject()
            .put("conversation", conversation ?: JSONObject.NULL).put("selected_head", head ?: JSONObject.NULL)
            .put("local_work_in_flight", busy).put("read", read?.request() ?: JSONObject.NULL))) as JSONObject
        AssistantProjection(value.getJSONObject("view"), value.optJSONObject("continuation"), value.optJSONObject("read"))
    }

    /** Pinned identity/ancestry survive imports. Admission lock encloses native SQLite COMMIT. */
    fun completeTurn(
        request: JSONObject,
        context: JSONObject,
        prompt: String,
        answer: JSONObject,
        terminalStage: String,
        provenance: JSONObject,
        sources: JSONArray = JSONArray(),
        commitAdmission: (() -> Boolean) -> Boolean = { it() },
    ): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.COMPLETE) {
        val body = measure("kotlin_request_encode") {
            val pinned = JSONObject(context.toString())
            JSONObject().put("kind", "turn_completed")
                .put("conversation", pinned.getJSONObject("conversation"))
                .put("parent", pinned.get("parent")).put("request", JSONObject(request.toString()))
                .put("prompt", prompt).put("answer", JSONObject(answer.toString())).put("sources", JSONArray(sources.toString()))
                .put("terminal_stage", terminalStage)
                .put("provenance", JSONObject(provenance.toString()).put("context", pinned.getJSONArray("ancestor_ids")))
        }
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
        completed.getJSONObject("record")
    }

    fun selectConversation(conversation: JSONObject?, selectedHead: JSONObject?) = locked(AssistantDiagnosticJournal.StorageCall.SELECT) {
        beforeSelection?.invoke()
        call("select", JSONObject().put("selection", selection(conversation, selectedHead)))
        Unit
    }
    fun localState(): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.LOCAL_STATE) { call("local_state") as JSONObject }
    internal fun projectionStillCurrent(revision: String, conversation: String?, head: String?): Boolean = locked(AssistantDiagnosticJournal.StorageCall.PROJECTION_STILL_CURRENT) {
        call("projection_still_current", JSONObject().put("revision", revision)
            .put("selection", selection(conversation?.let(::JSONObject), head?.let(::JSONObject)))) as Boolean
    }

    fun saveDraft(conversation: JSONObject?, text: String, interrupted: Boolean = false) = locked(AssistantDiagnosticJournal.StorageCall.SAVE_DRAFT) {
        require(AssistantDraftBudget.accepts(text)) { "Assistant draft too large" }
        call("save_draft", JSONObject().put("conversation", conversation ?: JSONObject.NULL).put("text", text).put("interrupted", interrupted))
        Unit
    }
    fun draft(conversation: JSONObject?): JSONObject = locked(AssistantDiagnosticJournal.StorageCall.DRAFT) {
        call("draft", JSONObject().put("conversation", conversation ?: JSONObject.NULL)) as JSONObject
    }

    fun publications(peerReplica: String? = null, limit: Int = 128,
        maxBytes: Int = AssistantDomain.MAX_SNAPSHOT_BYTES, after: JSONObject? = null): JSONArray = locked(AssistantDiagnosticJournal.StorageCall.PUBLICATIONS) {
        call("publications", JSONObject().put("peer", peerReplica ?: JSONObject.NULL).put("limit", limit)
            .put("max_bytes", maxBytes).put("after", after ?: JSONObject.NULL)) as JSONArray
    }
    fun receiptsToPublish(limit: Int = 128, after: JSONObject? = null): JSONArray = locked(AssistantDiagnosticJournal.StorageCall.RECEIPTS_TO_PUBLISH) {
        call("receipts_to_publish", JSONObject().put("limit", limit).put("after", after ?: JSONObject.NULL)) as JSONArray
    }
    fun acknowledge(peerReplica: String, receipts: JSONArray) = locked(AssistantDiagnosticJournal.StorageCall.ACKNOWLEDGE) {
        beforeCommit?.invoke()
        call("acknowledge", JSONObject().put("peer", peerReplica).put("receipts", receipts))
        Unit
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

    override fun close() = locked(AssistantDiagnosticJournal.StorageCall.CLOSE) {
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
    external fun openProfiled(path: String): String
    external fun execute(handle: Long, request: String): String
    external fun executeProfiled(handle: Long, request: String): String
    external fun close(handle: Long)
}
