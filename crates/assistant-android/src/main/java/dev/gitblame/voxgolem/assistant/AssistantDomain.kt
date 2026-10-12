package dev.gitblame.voxgolem.assistant

import org.json.JSONObject

fun interface DomainGateway {
    fun apply(request: String): String
}

object AssistantDomainNative {
    init { System.loadLibrary("wear_assistant_native") }
    external fun apply(request: String): String
}

class AssistantDomainException(val error: JSONObject) :
    IllegalArgumentException("Assistant domain: ${error.optString("kind", "invalid")}")

internal sealed class AssistantProjectionRead {
    object Context : AssistantProjectionRead()
    data class History(val before: String?) : AssistantProjectionRead()

    fun request(): JSONObject = when (this) {
        Context -> JSONObject().put("kind", "context").put("max_turns", 20).put("max_text_bytes", 256 * 1024)
        is History -> JSONObject().put("kind", "history").put("before", before ?: JSONObject.NULL)
            .put("max_turns", 32).put("max_text_bytes", 2 * 1024 * 1024)
    }
}

internal data class AssistantProjection(
    val view: JSONObject, val continuation: JSONObject?, private val readOutcome: JSONObject?,
) {
    fun read(): JSONObject? {
        val result = readOutcome ?: return null
        if (!result.getBoolean("ok")) throw AssistantDomainException(result.getJSONObject("error"))
        return result.getJSONObject("value")
    }
}

internal class AssistantDomain(private val gateway: DomainGateway) {
    fun project(snapshot: String, conversation: JSONObject?, head: JSONObject?, busy: Boolean,
        read: AssistantProjectionRead? = null): AssistantProjection {
        val value = evaluate("projection", snapshot, JSONObject().put("request", JSONObject()
            .put("conversation", conversation ?: JSONObject.NULL).put("selected_head", head ?: JSONObject.NULL)
            .put("local_work_in_flight", busy).put("read", read?.request() ?: JSONObject.NULL))) as JSONObject
        return AssistantProjection(value.getJSONObject("view"), value.optJSONObject("continuation"), value.optJSONObject("read"))
    }

    fun evaluate(kind: String, snapshot: String? = null, fields: JSONObject = JSONObject()): Any {
        val suffix = fields.toString().removePrefix("{").removeSuffix("}")
        val request = buildString {
            append("{\"kind\":").append(JSONObject.quote(kind))
            if (snapshot != null) append(",\"snapshot\":").append(snapshot)
            if (suffix.isNotEmpty()) append(',').append(suffix)
            append('}')
        }
        require(request.toByteArray(Charsets.UTF_8).size <= MAX_GATEWAY_BYTES) { "Assistant request too large" }
        val raw = gateway.apply(request)
        check(raw.toByteArray(Charsets.UTF_8).size <= MAX_GATEWAY_BYTES) { "Assistant response too large" }
        val response = JSONObject(raw)
        if (!response.getBoolean("ok")) throw AssistantDomainException(response.getJSONObject("error"))
        return response.get("value")
    }

    companion object {
        const val MAX_GATEWAY_BYTES = 16 * 1024 * 1024
        const val MAX_SNAPSHOT_BYTES = 4 * 1024 * 1024
    }
}
