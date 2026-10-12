package dev.gitblame.voxgolem.assistant.phone

import android.content.Context
import dev.gitblame.voxgolem.voice.InstallationPreferences
import org.json.JSONObject

class PhonePreferences(context: Context) {
    private val storage = context.applicationContext.getSharedPreferences("assistant-phone-preferences", Context.MODE_PRIVATE)
    private var published: JSONObject? = null

    @Synchronized fun read(model: String): JSONObject {
        val value = published ?: readStored().also { published = it }
        return JSONObject(value.toString()).put("instant", model)
    }

    private fun readStored(): JSONObject {
        val defaults = defaults()
        val value = JSONObject()
        for (key in defaults.keys()) {
            value.put(key, when (val fallback = defaults.get(key)) {
                is Boolean -> storage.getBoolean(key, fallback)
                else -> storage.getString(key, fallback as String)
            })
        }
        return value
    }

    /** Invoke on the shared actor alongside model selection; disk commit precedes publication. */
    @Synchronized fun write(input: JSONObject) {
        val value = normalize(input)
        validate(value)
        val before = published ?: readStored()
        if (!commit(value)) {
            // SharedPreferences may update its memory cache even when disk commit fails.
            commit(before)
            published = before
            throw AssistantPhoneFailure("internal", "Assistant preferences were not saved")
        }
        published = JSONObject(value.toString())
    }

    private fun commit(value: JSONObject): Boolean {
        val editor = storage.edit()
        // Actor policy is persisted once in InstallationPreferences and overlaid by EnginePhoneBackend.
        // Leave old duplicate keys untouched for compatibility, but never write a second policy record.
        for (key in value.keys()) if (!InstallationPreferences.ownsKey(key)) when (val field = value.get(key)) {
            is Boolean -> editor.putBoolean(key, field)
            is String -> editor.putString(key, field)
        }
        return editor.commit()
    }

    companion object {
        fun defaults(): JSONObject = JSONObject()
            .put("instant", "custom-sol-high").put("deep", "custom-sol-high").put("review", "custom-sol-high")
            .put("deep_enabled", false).put("review_enabled", false).put("prefetch", false).put("completion", false)
            .put("response_profile", "fast").put("tts_enabled", false).put("ui_theme", "dark").put("ui_text_size", "medium")
            .put("web_enabled", true).put("speech_network_fallback", true)
            .put("speech_language", "").put("speech_tts_provider", "").put("speech_tts_voice", "")

        fun normalize(value: JSONObject) = JSONObject(value.toString()).apply {
            for (key in listOf("web_enabled", "speech_network_fallback")) if (!has(key)) put(key, true)
            for (key in listOf("speech_language", "speech_tts_provider", "speech_tts_voice")) if (!has(key)) put(key, "")
        }
        fun validate(input: JSONObject) {
            val value = normalize(input)
            val defaults = defaults()
            if (value.keys().asSequence().toSet() != defaults.keys().asSequence().toSet()) invalid()
            val enums = mapOf(
                "instant" to setOf("custom-sol-high", "custom-sol-none", "custom-luna-none", "custom-luna-low"),
                "deep" to setOf("custom-sol-high", "custom-luna-low"),
                "review" to setOf("custom-sol-high", "custom-luna-low"),
                "response_profile" to setOf("fast", "quality"), "ui_theme" to setOf("light", "dark"),
                "ui_text_size" to setOf("small", "medium", "large", "extra_large"),
            )
            for ((key, options) in enums) {
                val selected = value.opt(key) as? String ?: invalid()
                if (selected !in options) invalid()
            }
            val strings = listOf("speech_language", "speech_tts_provider", "speech_tts_voice").map { value.opt(it) as? String ?: invalid() }
            try { dev.gitblame.voxgolem.assistant.speech.SpeechSelection(strings[0], strings[1], strings[2]).validateShape() }
            catch (_: IllegalArgumentException) { invalid() }
            for (key in listOf("deep_enabled", "review_enabled", "prefetch", "completion", "tts_enabled", "web_enabled", "speech_network_fallback")) {
                if (value.opt(key) !is Boolean) invalid()
                if (key in setOf("deep_enabled", "review_enabled", "prefetch", "completion") && value.getBoolean(key)) unavailable()
            }
            if (value.getString("response_profile") != "fast") unavailable()
        }
        private fun invalid(): Nothing = throw AssistantPhoneFailure("invalid_input", "Invalid Assistant preferences")
        private fun unavailable(): Nothing = throw AssistantPhoneFailure("unavailable", "This Android capability is unavailable")
    }
}
