package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.recognition.SpeechLocales
import org.json.JSONArray
import org.json.JSONObject
import java.util.Locale

/** Opaque SDK IDs must round-trip through the public UTF-8 IPC, never truncate into a different ID. */
internal fun selectableSpeechId(value: String): Boolean {
    if (value.length > 512 || value.any(Char::isISOControl)) return false
    val bytes = value.toByteArray(Charsets.UTF_8)
    return bytes.size <= 512 && bytes.toString(Charsets.UTF_8) == value
}

data class SpeechSelection(val language: String = "", val ttsProvider: String = "", val ttsVoice: String = "") {
    fun validateShape() {
        require(language.length <= 64 && (language.isEmpty() || SpeechLocales.parse(language) != null)) { "unsupported_speech_language" }
        require(selectableSpeechId(ttsProvider) && selectableSpeechId(ttsVoice)) { "invalid_speech_selection" }
    }
}
data class SpeechAdmission(val selection: SpeechSelection = SpeechSelection(), val networkFallback: Boolean = true)
data class SpeechVoiceOption(val provider: String, val name: String, val language: String, val networkRequired: Boolean)

/** Observed SDK choices, not downloadable models or a guarantee about today's processing route. */
data class SpeechOptions(
    val ttsProviders: List<String> = emptyList(),
    val ttsVoices: List<SpeechVoiceOption> = emptyList(),
    val recognitionInstalledLanguages: List<String>? = null,
    val recognitionSupportedLanguages: List<String>? = null,
    val localOnlyRecognitionAvailable: Boolean = false,
) {
    fun bounded() = copy(ttsProviders = ttsProviders.filter(::selectableSpeechId).distinct().take(8),
        ttsVoices = ttsVoices.filter { selectableSpeechId(it.name) && selectableSpeechId(it.provider) &&
            it.language.length <= 64 && SpeechLocales.parse(it.language) != null }.distinct().take(1024),
        recognitionInstalledLanguages = recognitionInstalledLanguages?.filter { it.length <= 64 }?.mapNotNull { SpeechLocales.parse(it)?.toLanguageTag() }?.distinct()?.take(128),
        recognitionSupportedLanguages = recognitionSupportedLanguages?.filter { it.length <= 64 }?.mapNotNull { SpeechLocales.parse(it)?.toLanguageTag() }?.distinct()?.take(128))

    fun validate(selection: SpeechSelection, networkFallback: Boolean) {
        selection.validateShape()
        if (selection.language.isNotEmpty()) {
            val languages = if (networkFallback) recognitionSupportedLanguages else recognitionInstalledLanguages
            require(languages != null && SpeechLocales.installedMatch(selection.language, languages) != null &&
                (networkFallback || localOnlyRecognitionAvailable)) { "unsupported_speech_language" }
        }
        require(selection.ttsProvider.isEmpty() || selection.ttsProvider in ttsProviders) { "unsupported_tts_provider" }
        if (selection.ttsVoice.isNotEmpty()) {
            val requested = SpeechLocales.parse(selection.language.ifEmpty { Locale.getDefault().toLanguageTag() })
                ?: throw IllegalArgumentException("unsupported_speech_language")
            require(ttsVoices.any { it.provider == selection.ttsProvider && it.name == selection.ttsVoice &&
                (networkFallback || !it.networkRequired) && SpeechLocales.parse(it.language)?.let { locale ->
                    locale.language == requested.language && (requested.script.isEmpty() || locale.script.isEmpty() || locale.script == requested.script)
                } == true }) { "unsupported_tts_voice" }
        }
    }

    fun json(): JSONObject {
        val value = bounded()
        return JSONObject().put("ttsProviders", JSONArray(value.ttsProviders))
            .put("ttsVoices", JSONArray(value.ttsVoices.map { JSONObject().put("provider", it.provider).put("name", it.name)
                .put("language", it.language).put("networkRequired", it.networkRequired) }))
            .put("recognitionInstalledLanguages", value.recognitionInstalledLanguages?.let(::JSONArray) ?: JSONObject.NULL)
            .put("recognitionSupportedLanguages", value.recognitionSupportedLanguages?.let(::JSONArray) ?: JSONObject.NULL)
            .put("localOnlyRecognitionAvailable", value.localOnlyRecognitionAvailable)
            .put("modelSelectionAvailable", false).put("modelIdentifiers", JSONObject.NULL)
    }
}
