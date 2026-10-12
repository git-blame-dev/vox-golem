package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import org.json.JSONArray
import org.json.JSONObject

/** Wire projection only. selectedContext must come from the Rust domain, never a graph flattening. */
object PhoneSnapshotEncoder {
    fun encode(state: AssistantState, selectedContext: JSONObject?, preferences: JSONObject,
        clientRequestIds: Map<String, String>, activeRequest: JSONObject? = null,
        providerAvailable: Boolean = false, providerReason: String = "Access credential status is unavailable",
    ): JSONObject {
        val view = JSONObject(state.viewJson)
        val turns = selectedContext?.getJSONArray("turns") ?: JSONArray()
        require(turns.length() <= 20)
        val transcript = JSONArray()
        var bytes = 0
        for (index in 0 until turns.length()) {
            val turn = turn(turns.getJSONObject(index), clientRequestIds)
            bytes += turn.getJSONObject("user").getJSONObject("content").getString("text").toByteArray(Charsets.UTF_8).size
            bytes += turn.getJSONObject("answer").getJSONObject("content").getString("text").toByteArray(Charsets.UTF_8).size
            transcript.put(turn)
        }
        require(bytes <= 256 * 1024)
        val conversations = JSONArray()
        val canonical = view.getJSONArray("conversations")
        var conversationCount = 0
        var selectedConversation: JSONObject? = null
        var nextConversationOffset = 128
        for (index in 0 until canonical.length()) {
            val item = canonical.getJSONObject(index)
            if (!item.getBoolean("deleted") && !item.isNull("title")) {
                conversationCount++
                val projected = JSONObject().put("id", wireId(item.getJSONObject("id"))).put("title", item.getString("title"))
                if (state.conversation?.let { wireId(JSONObject(it)) } == projected.getString("id")) selectedConversation = projected
                if (conversations.length() < 128) conversations.put(projected)
            }
        }
        selectedConversation?.let { selected ->
            if (!(0 until conversations.length()).any { conversations.getJSONObject(it).getString("id") == selected.getString("id") }) {
                conversations.put(127, selected)
                nextConversationOffset = 127
            }
        }
        val capabilities = JSONArray()
        for ((id, reason) in mapOf(
            "custom_provider" to providerReason, "opencode" to "OpenCode is unavailable on Android",
            "local_fast" to "Local models are unavailable on Android", "local_quality" to "Local models are unavailable on Android",
            "qwen_prediction" to "Prediction is unavailable on Android", "wake_word" to "Wake word is unavailable on Android",
            "vad" to "Voice activity detection is unavailable on Android", "parakeet" to "Desktop transcription is unavailable on Android",
            "tts" to "Text to speech is unavailable on Android", "deep" to "Deep execution is disabled on Android", "review" to "Review execution is disabled on Android",
        )) {
            val available = id == "custom_provider" && providerAvailable
            capabilities.put(JSONObject().put("id", id).put("state", if (available) "available" else "unavailable")
                .put("reason", reason).put("actual_provider", if (available) "custom_openai" else JSONObject.NULL))
        }
        val options = JSONArray()
        for ((id, label) in mapOf("custom-sol-high" to "Sol High", "custom-sol-none" to "Sol None", "custom-luna-none" to "Luna None", "custom-luna-low" to "Luna Low")) {
            options.put(JSONObject().put("id", id).put("label", label).put("available", providerAvailable)
                .put("unavailable_reason", if (providerAvailable) JSONObject.NULL else providerReason))
        }
        val active = activeRequest ?: if (state.busy) JSONObject()
            .put("request_id", requireNotNull(state.clientRequestToken)).put("prompt", state.draft)
            .put("execution_id", JSONObject().put("session_id", wireId(JSONObject(requireNotNull(state.conversation))))
                .put("generation", requireNotNull(state.providerRequestId))) else null
        return JSONObject().put("session_id", state.conversation?.let { wireId(JSONObject(it)) } ?: "0")
            .put("runtime_phase", if (state.busy) "executing" else "sleeping")
            .put("transcript", transcript).put("active_request", active ?: JSONObject.NULL)
            .put("capabilities", capabilities).put("model_options", options).put("preferences", preferences)
            .put("assistant", JSONObject().put("revision", view.getString("revision")).put("conversations", conversations)
                .put("conversation_count", conversationCount)
                .put("conversation_offset", if (conversationCount > conversations.length()) nextConversationOffset else conversationCount)
                .put("selected_head", state.selectedHead?.let { wireId(JSONObject(it)) } ?: JSONObject.NULL)
                .put("branch_choices", JSONArray(state.branchChoices.take(128).map { wireId(JSONObject(it)) }))
                .put("branch_count", state.branchChoices.size).put("branch_offset", state.branchChoices.size.coerceAtMost(128))
                .put("draft", state.draft).put("interrupted", state.interrupted).put("error", state.error ?: JSONObject.NULL))
    }

    fun turn(record: JSONObject, clientRequestIds: Map<String, String>): JSONObject {
        val operation = record.getJSONObject("operation")
        val body = operation.getJSONObject("body")
        require(body.getString("kind") == "turn_completed")
        val turnId = wireId(operation.getJSONObject("id"))
        val requestId = wireId(body.getJSONObject("request"))
        val answer = body.getJSONObject("answer")
        val kind = answer.getString("kind")
        require(kind == "text" || kind == "refusal")
        return JSONObject().put("request_id", clientRequestIds[requestId] ?: requestId)
            .put("execution_id", JSONObject().put("session_id", wireId(body.getJSONObject("conversation"))).put("generation", requestId))
            .put("user", JSONObject().put("id", turnId).put("role", "user").put("content", JSONObject().put("kind", "text").put("text", body.getString("prompt"))))
            .put("answer", JSONObject().put("id", turnId).put("role", "assistant").put("content", JSONObject().put("kind", kind).put("text", answer.getString(if (kind == "refusal") "reason" else "text"))))
            .put("final_stage", body.getString("terminal_stage"))
    }

    fun wireId(id: JSONObject): String = "${id.getString("replica")}:${id.getString("sequence")}"
}
