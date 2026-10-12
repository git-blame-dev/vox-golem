package dev.gitblame.voxgolem.assistant.runtime

import android.content.Context
import dev.gitblame.voxgolem.voice.InstallationPreferences
import dev.gitblame.voxgolem.voice.InstallationSettings

data class AssistantPolicy(val webEnabled: Boolean = true, val speechNetworkFallback: Boolean = true,
    val speechLanguage: String = "", val speechTtsProvider: String = "", val speechTtsVoice: String = "") {
    fun selection() = dev.gitblame.voxgolem.assistant.speech.SpeechSelection(speechLanguage, speechTtsProvider, speechTtsVoice)
}

fun AssistantState.speechAdmission() = dev.gitblame.voxgolem.assistant.speech.SpeechAdmission(
    dev.gitblame.voxgolem.assistant.speech.SpeechSelection(speechLanguage, speechTtsProvider, speechTtsVoice), speechNetworkFallback)

interface AssistantPolicyStorage {
    fun read(): AssistantPolicy
    fun write(value: AssistantPolicy)
}

/** Installation-local settings, never conversation records or replication payloads. */
class NativeAssistantPolicyStorage(context: Context) : AssistantPolicyStorage {
    private val preferences = InstallationPreferences(context)
    override fun read() = preferences.read().let { AssistantPolicy(it.webEnabled, it.speechNetworkFallback,
        it.speechLanguage, it.speechTtsProvider, it.speechTtsVoice) }
    override fun write(value: AssistantPolicy) = preferences.write(InstallationSettings(value.webEnabled,
        value.speechNetworkFallback, value.speechLanguage, value.speechTtsProvider, value.speechTtsVoice))
}
