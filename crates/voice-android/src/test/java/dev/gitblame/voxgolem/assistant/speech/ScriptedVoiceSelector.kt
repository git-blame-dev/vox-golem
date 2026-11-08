package dev.gitblame.voxgolem.assistant.speech

import java.util.Locale

/** Lifecycle tests script policy outcomes; portable eligibility/ranking is tested in Rust/Assistant. */
internal class ScriptedVoiceSelector : ObservedVoiceSelector {
    var answer: (Locale, Boolean, String, String?, List<LocalVoice>) -> Int =
        { _, _, _, _, voices -> if (voices.isEmpty()) -1 else 0 }
    override fun select(requested: Locale, networkAllowed: Boolean, explicitVoice: String,
        defaultVoice: String?, voices: List<LocalVoice>): Int =
        answer(requested, networkAllowed, explicitVoice, defaultVoice, voices)
}
