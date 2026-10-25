package dev.gitblame.voxgolem.assistant.replication

import android.util.JsonReader
import android.util.JsonToken
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.StringReader
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.UUID

internal data class PeerNode(val nodeId: String, val displayName: String)

internal data class PreparedRecordTransfer(val entry: JSONObject, val bytes: ByteArray)

internal data class AssembledRecordBatch(
    val records: JSONArray,
    val verifiedEntries: JSONArray,
    val ids: JSONArray,
    val unavailable: JSONArray,
)

internal interface ReplicationSessionConnection : AutoCloseable {
    val nodeId: String
    val path: String
    val session: String
    fun readFrame(): JSONObject
    fun writeFrame(frame: JSONObject)
    fun readBytes(length: Int): ByteArray
    fun writeBytes(bytes: ByteArray, offset: Int = 0, length: Int = bytes.size)
    override fun close()
}

internal interface ReplicationTransport {
    fun reachable(): List<PeerNode>
    fun sendHint(nodeId: String, sender: String, target: String): Unit = throw UnsupportedOperationException()
    fun openSession(nodeId: String, session: String = UUID.randomUUID().toString()): ReplicationSessionConnection =
        throw UnsupportedOperationException()
}

internal interface ReplicationState {
    fun enrolledReplicas(): Set<String>
}

internal interface ReplicationStore {
    val replica: String
    fun snapshot(): JSONObject
    fun publications(peer: String, after: JSONObject?, limit: Int): JSONArray
    fun importRecords(input: JSONArray)
    fun importVerified(input: JSONArray, entries: JSONArray): JSONArray? = run { importRecords(input); null }
    fun acknowledge(peer: String, receipts: JSONArray)
    fun durableReceipts(ids: JSONArray, checkBudget: () -> Unit): JSONArray? = null
    /** Stateful native owner; null is the legacy in-memory test adapter only. */
    fun nativeRead(request: JSONObject): Any? = null
}

internal interface ReplicationDomain {
    fun inventory(snapshot: JSONObject, cursor: JSONObject): JSONObject
    fun reconcile(snapshot: JSONObject, page: JSONObject): JSONObject
    fun receipts(snapshot: JSONObject, replica: String, ids: JSONArray): JSONArray
}

internal object ReplicationWire {
    const val PREFIX = "/voxgolem/assistant/v2/"
    // Pairing/credential associations retain their independent metadata protocol.
    const val IDENTITY_PATH = "/voxgolem/assistant/v1/identity"
    const val CAPABILITY = "voxgolem_assistant_v1"
    const val HINT_PATH = "/voxgolem/assistant/v2/reconcile-hint"
    const val SESSION_PREFIX = "/voxgolem/assistant/v2/session/"
    const val MAX_CONTROL_BYTES = 32 * 1024
    const val MAX_HINT_BYTES = 256
    const val MAX_CHUNK_BYTES = 64 * 1024
    const val MAX_RECORD_BYTES = 2 * 1024 * 1024
    const val MAX_BATCH_BYTES = 4 * 1024 * 1024
    const val MAX_ITEMS = 128
    const val MAX_BATCH_RECORDS = 8
    const val MAX_SESSION_PAGES = 4096
    const val MAX_PEERS = 8
    val kinds = setOf("hint", "inventory_request", "inventory", "record_request", "records", "ack", "ack_confirmed", "restart")
    val sessionKinds = setOf(
        "inventory_request",
        "inventory",
        "record_request",
        "records",
        "body_chunk",
        "batch_end",
        "ack",
        "ack_confirmed",
        "restart",
        "done",
    )

    fun uuid(value: String): String {
        require(value.length == 36 && UUID.fromString(value).toString() == value)
        return value
    }

    fun idKey(id: JSONObject): String {
        require(id.length() == 2 && id.get("replica") is String && id.get("sequence") is String)
        val sequence = id.getString("sequence")
        require(sequence.matches(Regex("[1-9][0-9]{0,19}")) && java.math.BigInteger(sequence) <= java.math.BigInteger("18446744073709551615"))
        return "${uuid(id.getString("replica"))}/$sequence"
    }

    fun bodyKey(value: String): String {
        require(value.matches(Regex("[0-9a-f]{64}")))
        return value
    }

    fun sessionPath(session: String): String = "$SESSION_PREFIX${uuid(session)}"

    fun parseSessionPath(path: String): String? =
        path.takeIf { it.startsWith(SESSION_PREFIX) }
            ?.removePrefix(SESSION_PREFIX)
            ?.takeIf { runCatching { uuid(it) }.isSuccess }

    fun validSessionPath(path: String): Boolean = parseSessionPath(path) != null

    fun encodeHint(sender: String, target: String): ByteArray =
        JSONObject().put("version", 3).put("kind", "reconcile_hint")
            .put("sender", uuid(sender)).put("target", uuid(target))
            .toString().toByteArray(Charsets.UTF_8)
            .also { require(it.size <= MAX_HINT_BYTES) }

    fun validateHint(bytes: ByteArray, local: String, authenticatedPeer: String) {
        val hint = decode(bytes, MAX_HINT_BYTES)
        require(hint.length() == 4 && hint.get("version") == 3 && hint.getString("kind") == "reconcile_hint")
        require(hint.getString("sender") == uuid(authenticatedPeer) && hint.getString("target") == uuid(local))
    }

    fun frame(sender: String, target: String, session: String, token: String, kind: String, payload: JSONObject): JSONObject =
        JSONObject().put("version", 2).put("sender", uuid(sender)).put("target", uuid(target))
            .put("session", uuid(session)).put("token", uuid(token)).put("kind", kind).put("payload", payload)

    fun sessionFrame(
        sender: String,
        target: String,
        session: String,
        token: String,
        kind: String,
        payload: JSONObject = JSONObject(),
    ): JSONObject {
        require(kind in sessionKinds)
        return frame(sender, target, session, token, kind, payload)
    }

    fun encode(frame: JSONObject): ByteArray = frame.toString().toByteArray(Charsets.UTF_8).also {
        require(it.size <= MAX_CONTROL_BYTES)
    }

    fun decode(bytes: ByteArray, maxBytes: Int = MAX_CONTROL_BYTES): JSONObject {
        require(bytes.size in 2..maxBytes)
        val raw = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString()
        // JSONObject otherwise silently accepts duplicate fields. Preserve the Rust boundary.
        JsonReader(StringReader(raw)).use { reader ->
            reader.isLenient = false
            validateJson(reader, 0)
            require(reader.peek() == JsonToken.END_DOCUMENT)
        }
        return JSONObject(raw)
    }

    private fun validateJson(reader: JsonReader, depth: Int) {
        require(depth <= 32)
        when (reader.peek()) {
            JsonToken.BEGIN_OBJECT -> {
                reader.beginObject()
                val names = HashSet<String>()
                while (reader.hasNext()) { require(names.add(reader.nextName())); validateJson(reader, depth + 1) }
                reader.endObject()
            }
            JsonToken.BEGIN_ARRAY -> {
                reader.beginArray()
                var count = 0
                while (reader.hasNext()) { require(count++ < 65_536); validateJson(reader, depth + 1) }
                reader.endArray()
            }
            JsonToken.STRING, JsonToken.NUMBER -> reader.nextString()
            JsonToken.BOOLEAN -> reader.nextBoolean()
            JsonToken.NULL -> reader.nextNull()
            else -> error("Invalid replication JSON")
        }
    }

    fun validateSessionFrame(frame: JSONObject, session: String, local: String, authenticatedPeer: String): JSONObject {
        require(frame.length() == 7 && frame.get("version") == 2)
        require(frame.getString("sender") == uuid(authenticatedPeer) && frame.getString("target") == uuid(local))
        require(frame.getString("session") == uuid(session))
        uuid(frame.getString("token"))
        require(frame.getString("kind") in sessionKinds)
        return frame.getJSONObject("payload")
    }

    fun entries(array: JSONArray, descriptors: Boolean = true) {
        require(array.length() <= MAX_ITEMS)
        val seen = HashSet<String>()
        for (i in 0 until array.length()) {
            val item = array.getJSONObject(i)
            require(seen.add(idKey(if (descriptors) item.getJSONObject("id") else item)))
            if (descriptors) {
                require(item.length() == 4 || (item.length() == 2 && item.isNull("body_key")))
                if (!item.isNull("body_key")) {
                    bodyKey(item.getString("body_key")); idKey(item.getJSONObject("conversation"))
                    require(item.get("tombstone") is Boolean)
                }
            }
        }
    }

    fun descriptor(record: JSONObject): JSONObject {
        val operation = record.getJSONObject("operation")
        val body = operation.getJSONObject("body")
        val kind = body.getString("kind")
        return JSONObject().put("id", operation.getJSONObject("id"))
            .put("body_key", bodyKey(record.getString("body_key")))
            .put("conversation", if (kind == "conversation_created") operation.getJSONObject("id") else body.getJSONObject("conversation"))
            .put("tombstone", kind == "conversation_deleted")
    }

    fun sameEntry(a: JSONObject, b: JSONObject): Boolean =
        idKey(a.getJSONObject("id")) == idKey(b.getJSONObject("id")) &&
            idKey(a.getJSONObject("conversation")) == idKey(b.getJSONObject("conversation")) &&
            a.getString("body_key") == b.getString("body_key") &&
            a.getBoolean("tombstone") == b.getBoolean("tombstone")

    fun writeRecordBatch(
        connection: ReplicationSessionConnection,
        sender: String,
        target: String,
        session: String,
        token: String,
        transfer: String,
        bodies: List<PreparedRecordTransfer>,
        unavailable: JSONArray,
        checkBudget: () -> Unit = {},
    ) {
        uuid(transfer)
        require(bodies.size <= MAX_BATCH_RECORDS)
        entries(unavailable, descriptors = false)
        require(bodies.size + unavailable.length() in 1..MAX_BATCH_RECORDS)
        val seenIds = arrayObjects(unavailable).map(::idKey).toHashSet()
        val descriptors = JSONArray()
        var totalBytes = 0L
        for (item in bodies) {
            require(item.entry.length() == 4 && !item.entry.isNull("body_key"))
            entries(JSONArray().put(item.entry))
            require(seenIds.add(idKey(item.entry.getJSONObject("id"))))
            require(item.bytes.size in 2..MAX_RECORD_BYTES)
            totalBytes += item.bytes.size.toLong()
            require(totalBytes <= MAX_BATCH_BYTES)
            descriptors.put(JSONObject().put("entry", item.entry).put("bytes", item.bytes.size))
        }
        checkBudget()
        connection.writeFrame(
            sessionFrame(
                sender,
                target,
                session,
                token,
                "records",
                JSONObject().put("transfer", transfer).put("records", descriptors).put("unavailable", unavailable),
            )
        )
        for ((index, item) in bodies.withIndex()) {
            var offset = 0
            while (offset < item.bytes.size) {
                checkBudget()
                val chunkLen = minOf(MAX_CHUNK_BYTES, item.bytes.size - offset)
                connection.writeFrame(
                    sessionFrame(
                        sender,
                        target,
                        session,
                        token,
                        "body_chunk",
                        JSONObject().put("transfer", transfer).put("index", index).put("bytes", chunkLen),
                    )
                )
                connection.writeBytes(item.bytes, offset, chunkLen)
                offset += chunkLen
            }
        }
        checkBudget()
        connection.writeFrame(
            sessionFrame(
                sender,
                target,
                session,
                token,
                "batch_end",
                JSONObject().put("transfer", transfer).put("records", bodies.size).put("bytes", totalBytes.toInt()),
            )
        )
    }

    fun readRecordBatch(
        connection: ReplicationSessionConnection,
        recordsFrame: JSONObject,
        session: String,
        local: String,
        peer: String,
        expectedEntries: JSONArray,
        phase: String,
        checkBudget: () -> Unit = {},
    ): AssembledRecordBatch {
        require(phase in setOf("tombstones", "records"))
        entries(expectedEntries)
        require(expectedEntries.length() in 1..MAX_BATCH_RECORDS)
        val payload = validateSessionFrame(recordsFrame, session, local, peer)
        require(recordsFrame.getString("kind") == "records")
        val token = recordsFrame.getString("token")
        require(payload.length() == 3)
        val transfer = uuid(payload.getString("transfer"))
        val transfers = payload.getJSONArray("records")
        val unavailable = payload.getJSONArray("unavailable")
        require(transfers.length() <= MAX_BATCH_RECORDS)
        entries(unavailable, descriptors = false)
        require(transfers.length() + unavailable.length() in 1..MAX_BATCH_RECORDS)
        val expected = arrayObjects(expectedEntries).associateBy { idKey(it.getJSONObject("id")) }
        val answered = HashSet<String>()
        var totalBytes = 0L
        val records = JSONArray()
        val verifiedEntries = JSONArray()
        val ids = JSONArray()
        val transferObjects = arrayObjects(transfers)
        for (item in transferObjects) {
            require(item.length() == 2 && item.get("bytes") is Int)
            val entry = item.getJSONObject("entry")
            require(entry.length() == 4 && !entry.isNull("body_key"))
            entries(JSONArray().put(entry))
            val key = idKey(entry.getJSONObject("id"))
            require(answered.add(key))
            val requested = expected[key] ?: error("Unrequested record")
            if (!requested.isNull("body_key")) require(sameEntry(requested, entry))
            if (phase == "tombstones") require(entry.getBoolean("tombstone"))
            val declaredSize = item.getInt("bytes")
            require(declaredSize in 2..MAX_RECORD_BYTES)
            totalBytes += declaredSize.toLong()
            require(totalBytes <= MAX_BATCH_BYTES)
        }
        for (id in arrayObjects(unavailable)) {
            val key = idKey(id)
            require(answered.add(key) && key in expected)
        }
        require(answered == expected.keys)
        for ((expectedIndex, item) in transferObjects.withIndex()) {
            val entry = item.getJSONObject("entry")
            val declaredSize = item.getInt("bytes")
            val assembled = ByteArrayOutputStream(minOf(declaredSize, MAX_CHUNK_BYTES))
            while (assembled.size() < declaredSize) {
                checkBudget()
                val chunkFrame = connection.readFrame()
                val chunkPayload = validateSessionFrame(chunkFrame, session, local, peer)
                require(chunkFrame.getString("token") == token && chunkFrame.getString("kind") == "body_chunk")
                require(chunkPayload.length() == 3 && chunkPayload.getString("transfer") == transfer)
                require(chunkPayload.get("index") is Int && chunkPayload.getInt("index") == expectedIndex)
                require(chunkPayload.get("bytes") is Int)
                val chunkLen = chunkPayload.getInt("bytes")
                require(chunkLen in 1..minOf(MAX_CHUNK_BYTES, declaredSize - assembled.size()))
                val chunk = connection.readBytes(chunkLen)
                require(chunk.size == chunkLen)
                assembled.write(chunk)
            }
            val record = decode(assembled.toByteArray(), MAX_RECORD_BYTES)
            require(record.length() == 2 && sameEntry(descriptor(record), entry))
            records.put(record)
            verifiedEntries.put(entry)
            ids.put(entry.getJSONObject("id"))
        }
        checkBudget()
        val endFrame = connection.readFrame()
        val endPayload = validateSessionFrame(endFrame, session, local, peer)
        require(endFrame.getString("token") == token && endFrame.getString("kind") == "batch_end")
        require(endPayload.length() == 3 && endPayload.getString("transfer") == transfer)
        require(endPayload.get("records") is Int && endPayload.getInt("records") == transfers.length())
        require(endPayload.get("bytes") is Int && endPayload.getInt("bytes").toLong() == totalBytes)
        return AssembledRecordBatch(records, verifiedEntries, ids, unavailable)
    }

    fun arrayObjects(array: JSONArray): List<JSONObject> = (0 until array.length()).map(array::getJSONObject)
}
