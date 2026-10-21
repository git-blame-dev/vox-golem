package dev.gitblame.voxgolem.assistant.speech

import org.json.JSONObject

internal object AssistantSpeechNative {
    init { System.loadLibrary("wear_assistant_native") }
    external fun open(enabled: Boolean): Long
    external fun apply(handle: Long, operation: Int, text: String?): String?
    external fun firstParagraph(text: String): String?
}

internal data class SpeechChunk(val generation: Long, val id: Long, val text: String)
internal data class SpeechDecision(val cancel: Boolean, val closed: Boolean, val chunk: SpeechChunk?)

/** Each admission owns one bounded Rust policy; settlement releases it before playback ends. */
internal class SpeechPolicyUnavailable : RuntimeException("speech_policy_unavailable")

internal class RustReplySpeech(enabled: Boolean = true) : AutoCloseable {
    private var handle = AssistantSpeechNative.open(enabled).also { if (it == 0L) throw SpeechPolicyUnavailable() }
    fun apply(operation: Int, text: String? = null): SpeechDecision {
        if (handle == 0L) throw SpeechPolicyUnavailable()
        try {
            val raw = AssistantSpeechNative.apply(handle, operation, text) ?: throw SpeechPolicyUnavailable()
            if (operation >= 2) handle = 0
            val json = JSONObject(raw)
            val chunk = json.optJSONObject("speak")?.let { SpeechChunk(it.getLong("generation"), it.getLong("chunk"), it.getString("text")) }
            return SpeechDecision(json.getBoolean("cancel"), json.getBoolean("closed"), chunk)
        } catch (_: Exception) {
            close()
            throw SpeechPolicyUnavailable()
        }
    }
    override fun close() {
        val retired = handle; handle = 0
        if (retired != 0L) AssistantSpeechNative.apply(retired, 3, null)
    }
}

object FirstParagraphSpeech {
    fun extract(text: String): String = AssistantSpeechNative.firstParagraph(text).orEmpty()
}
