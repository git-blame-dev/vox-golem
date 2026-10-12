package dev.gitblame.voxgolem.assistant.replication

import android.util.JsonReader
import android.util.JsonToken
import org.json.JSONArray
import org.json.JSONObject
import java.io.StringReader
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.UUID

internal data class PeerNode(val nodeId: String, val displayName: String)

internal interface ReplicationTransport {
    fun reachable(): List<PeerNode>
    fun publish(path: String, bytes: ByteArray, assets: Map<String, ByteArray> = emptyMap())
}

internal interface ReplicationState {
    fun enrolledReplicas(): Set<String>
    fun load(peer: String): JSONObject?
    fun save(peer: String, state: JSONObject?)
    fun cleanupReplicas(): Set<String> = emptySet()
    fun cleanupComplete(peer: String) {}
}

internal interface ReplicationStore {
    val replica: String
    fun snapshot(): JSONObject
    fun publications(peer: String, after: JSONObject?, limit: Int): JSONArray
    fun importRecords(input: JSONArray)
    fun acknowledge(peer: String, receipts: JSONArray)
    fun durableReceipts(ids: JSONArray, checkBudget: () -> Unit): JSONArray? = null
}

internal interface ReplicationDomain {
    fun inventory(snapshot: JSONObject, cursor: JSONObject): JSONObject
    fun reconcile(snapshot: JSONObject, page: JSONObject): JSONObject
    fun receipts(snapshot: JSONObject, replica: String, ids: JSONArray): JSONArray
}

internal object ReplicationWire {
    const val PREFIX = "/voxgolem/assistant/v1/"
    const val IDENTITY_PATH = "${PREFIX}identity"
    const val CAPABILITY = "voxgolem_assistant_v1"
    const val MAX_CONTROL_BYTES = 32 * 1024
    const val MAX_RECORD_BYTES = 2 * 1024 * 1024
    const val MAX_ASSET_BYTES = 4 * 1024 * 1024
    const val MAX_ITEMS = 128
    const val MAX_ASSETS = 8
    const val MAX_PEERS = 8
    val kinds = setOf("hint", "inventory_request", "inventory", "record_request", "records", "ack", "ack_confirmed", "restart")

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

    fun path(sender: String, target: String, kind: String): String {
        require(kind in kinds)
        return "$PREFIX${uuid(sender)}/${uuid(target)}/$kind"
    }

    fun frame(sender: String, target: String, session: String, token: String, kind: String, payload: JSONObject): JSONObject =
        JSONObject().put("version", 1).put("sender", uuid(sender)).put("target", uuid(target))
            .put("session", uuid(session)).put("token", uuid(token)).put("kind", kind).put("payload", payload)

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

    fun validateFrame(frame: JSONObject, path: String, local: String, authenticatedPeer: String) {
        require(frame.length() == 7 && frame.get("version") == 1)
        require(frame.getString("sender") == authenticatedPeer && frame.getString("target") == local)
        uuid(frame.getString("session")); uuid(frame.getString("token"))
        require(path == path(authenticatedPeer, local, frame.getString("kind")))
        frame.getJSONObject("payload")
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

    fun arrayObjects(array: JSONArray): List<JSONObject> = (0 until array.length()).map(array::getJSONObject)
}
