package dev.gitblame.voxgolem.assistant.speech

import java.util.Locale
import org.json.JSONArray
import org.json.JSONObject

internal object SpeechSelectionNative {
    init { System.loadLibrary("wear_assistant_native") }
    external fun select(input: String): Int
}

/** Portable eligibility/ranking only; SDK discovery, synthesis and retirement stay native. */
class RustObservedVoiceSelector : ObservedVoiceSelector {
    override fun select(requested: Locale, networkAllowed: Boolean, explicitVoice: String,
        defaultVoice: String?, voices: List<LocalVoice>): Int {
        val selection = JSONObject().put("language", requested.language).put("script", requested.script)
            .put("region", requested.country).put("networkAllowed", networkAllowed)
            .put("explicitVoice", explicitVoice).put("defaultVoice", defaultVoice.orEmpty())
        val observed = JSONArray(voices.map { voice -> JSONObject().put("name", voice.name)
            .put("language", voice.locale.language).put("script", voice.locale.script).put("region", voice.locale.country)
            .put("networkRequired", voice.networkRequired).put("installed", voice.installed) })
        return SpeechSelectionNative.select(JSONObject().put("selection", selection).put("voices", observed).toString())
    }
}
