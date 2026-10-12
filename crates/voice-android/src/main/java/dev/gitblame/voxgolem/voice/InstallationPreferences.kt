package dev.gitblame.voxgolem.voice

import android.content.Context
import android.content.SharedPreferences
import dev.gitblame.voxgolem.assistant.speech.SpeechAdmission
import dev.gitblame.voxgolem.assistant.speech.SpeechSelection

data class InstallationSettings(val webEnabled: Boolean = true, val speechNetworkFallback: Boolean = true,
    val speechLanguage: String = "", val speechTtsProvider: String = "", val speechTtsVoice: String = "") {
    fun selection() = SpeechSelection(speechLanguage, speechTtsProvider, speechTtsVoice)
    fun speechAdmission() = SpeechAdmission(selection(), speechNetworkFallback)
}

/** Installation-local settings only. Readers never open chat storage or initialize its runtime. */
class InstallationPreferences internal constructor(private val preferences: SharedPreferences) {
    constructor(context: Context) : this(context.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE))

    // Android returns one SharedPreferences instance per file in a process. Its monitor also
    // serializes different adapters, including a failed commit and its full-record rollback.
    fun read(): InstallationSettings = synchronized(preferences) {
        InstallationSettings(preferences.getBoolean("web_enabled", true),
            preferences.getBoolean("speech_network_fallback", true), preferences.getString("speech_language", "") ?: "",
            preferences.getString("speech_tts_provider", "") ?: "", preferences.getString("speech_tts_voice", "") ?: "")
    }

    /** Caller retains actor/context validation and publishes only after durable success. */
    fun write(value: InstallationSettings) = synchronized(preferences) {
        value.selection().validateShape()
        val before = read()
        if (!commit(value)) {
            commit(before)
            throw IllegalStateException("Assistant policy was not saved")
        }
    }

    private fun commit(value: InstallationSettings) = preferences.edit().putBoolean("web_enabled", value.webEnabled)
        .putBoolean("speech_network_fallback", value.speechNetworkFallback)
        .putString("speech_language", value.speechLanguage).putString("speech_tts_provider", value.speechTtsProvider)
        .putString("speech_tts_voice", value.speechTtsVoice).commit()

    companion object {
        const val FILE = "assistant-policy"
        fun ownsKey(key: String): Boolean = when (key) {
            "web_enabled", "speech_network_fallback", "speech_language", "speech_tts_provider", "speech_tts_voice" -> true
            else -> false
        }
    }
}
