package dev.gitblame.voxgolem.assistant.phone

import android.content.Context
import dev.gitblame.voxgolem.voice.InstallationPreferences
import org.json.JSONObject

class PhonePreferences internal constructor(private val storage: android.content.SharedPreferences) {
    constructor(context: Context) : this(context.applicationContext.getSharedPreferences("assistant-phone-preferences", Context.MODE_PRIVATE))
    internal data class Presentation(val instant: String, val deep: String, val review: String,
        val deepEnabled: Boolean, val reviewEnabled: Boolean, val prefetch: Boolean, val completion: Boolean,
        val responseProfile: String, val ttsEnabled: Boolean, val theme: String, val textSize: String) {
        fun json(): JSONObject = defaults().put("instant", instant).put("deep", deep).put("review", review)
            .put("deep_enabled", deepEnabled).put("review_enabled", reviewEnabled).put("prefetch", prefetch)
            .put("completion", completion).put("response_profile", responseProfile).put("tts_enabled", ttsEnabled)
            .put("ui_theme", theme).put("ui_text_size", textSize)
        companion object {
            fun from(value: JSONObject) = Presentation(value.getString("instant"), value.getString("deep"), value.getString("review"),
                value.getBoolean("deep_enabled"), value.getBoolean("review_enabled"), value.getBoolean("prefetch"),
                value.getBoolean("completion"), value.getString("response_profile"), value.getBoolean("tts_enabled"),
                value.getString("ui_theme"), value.getString("ui_text_size"))
        }
    }
    @Volatile private var published: Presentation? = null
    private val initialization = Any()

    // The first reader still loads persisted values synchronously; moving that bootstrap
    // to the engine actor requires its owner. Hot reads never share a lock with persistence.
    private fun current(): Presentation = published ?: synchronized(initialization) {
        published ?: Presentation.from(readStored()).also { published = it }
    }
    fun read(model: String): JSONObject = current().json().put("instant", model)

    private fun readStored(): JSONObject {
        val defaults = defaults()
        val value = JSONObject()
        for (key in defaults.keys()) if (!InstallationPreferences.ownsKey(key)) {
            value.put(key, when (val fallback = defaults.get(key)) {
                is Boolean -> storage.getBoolean(key, fallback)
                else -> storage.getString(key, fallback as String)
            })
        }
        return value
    }

    /** Invoke on the shared actor alongside model selection; disk commit precedes publication. */
    fun write(input: JSONObject) { publish(persist(input)) }

    internal fun persist(input: JSONObject): Presentation {
        val value = copyInput(input)
        validate(value)
        val before = current()
        val candidate = Presentation.from(value)
        if (!commit(candidate.json())) {
            // SharedPreferences may update its memory cache even when disk commit fails.
            commit(before.json())
            throw AssistantPhoneFailure("internal", "Assistant preferences were not saved")
        }
        return candidate
    }

    internal fun publish(value: Presentation) { published = value }

    private fun commit(value: JSONObject): Boolean {
        val editor = storage.edit()
        // Actor policy belongs to InstallationPreferences; only presentation fields are persisted here.
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
            .put("web_enabled", true).put("speech_network_fallback", true).put("fast_mode", false)
            .put("speech_language", "").put("speech_tts_provider", "").put("speech_tts_voice", "")

        internal fun copyInput(value: JSONObject) = JSONObject(value.toString())
        fun validate(value: JSONObject) {
            val defaults = defaults()
            if (value.keys().asSequence().toSet() != defaults.keys().asSequence().toSet()) invalid()
            val enums = mapOf(
                "instant" to dev.gitblame.voxgolem.assistant.runtime.AssistantModels.aliases.toSet(),
                "deep" to dev.gitblame.voxgolem.assistant.runtime.AssistantModels.agentAliases,
                "review" to dev.gitblame.voxgolem.assistant.runtime.AssistantModels.agentAliases,
                "response_profile" to setOf("fast", "quality"), "ui_theme" to setOf("light", "dark", "slate"),
                "ui_text_size" to setOf("small", "medium", "large", "extra_large"),
            )
            for ((key, options) in enums) {
                val selected = value.opt(key) as? String ?: invalid()
                if (selected !in options) invalid()
            }
            val strings = listOf("speech_language", "speech_tts_provider", "speech_tts_voice").map { value.opt(it) as? String ?: invalid() }
            try { dev.gitblame.voxgolem.assistant.speech.SpeechSelection(strings[0], strings[1], strings[2]).validateShape() }
            catch (_: IllegalArgumentException) { invalid() }
            for (key in listOf("deep_enabled", "review_enabled", "prefetch", "completion", "tts_enabled", "web_enabled", "speech_network_fallback", "fast_mode")) {
                if (value.opt(key) !is Boolean) invalid()
                if (key in setOf("deep_enabled", "review_enabled", "prefetch", "completion") && value.getBoolean(key)) unavailable()
            }
            if (value.getString("response_profile") != "fast") unavailable()
        }
        private fun invalid(): Nothing = throw AssistantPhoneFailure("invalid_input", "Invalid Assistant preferences")
        private fun unavailable(): Nothing = throw AssistantPhoneFailure("unavailable", "This Android capability is unavailable")
    }
}
