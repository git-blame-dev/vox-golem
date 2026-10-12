package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.runtime.speechAdmission
import dev.gitblame.voxgolem.assistant.speech.SpeechSelection

internal enum class AssistantSpeechField(val title: String) {
    LANGUAGE("Speech language"), ENGINE("Reply voice engine"), VOICE("Speech voice"),
}

internal data class AssistantSpeechChoice(val value: String, val label: String, val enabled: Boolean)
internal data class AssistantSpeechCatalog(
    val savedValue: String, val savedLabel: String, val savedReported: Boolean, val savedAvailable: Boolean,
    val evidence: String, val choices: List<AssistantSpeechChoice>,
)

internal fun assistantSpeechCandidate(saved: SpeechSelection, field: AssistantSpeechField, value: String): SpeechSelection = when (field) {
    AssistantSpeechField.LANGUAGE -> saved.copy(language = value)
    AssistantSpeechField.ENGINE -> if (value == saved.ttsProvider) saved else saved.copy(ttsProvider = value, ttsVoice = "")
    AssistantSpeechField.VOICE -> saved.copy(ttsVoice = value)
}

internal fun assistantSpeechCatalog(state: AssistantState, voice: AssistantVoiceState, field: AssistantSpeechField): AssistantSpeechCatalog {
    val options = voice.speechOptions.bounded()
    val saved = state.speechAdmission().selection
    val voiceEntries = options.ttsVoices.filter { it.provider == saved.ttsProvider }.groupBy { it.name }
    val value = when (field) {
        AssistantSpeechField.LANGUAGE -> saved.language
        AssistantSpeechField.ENGINE -> saved.ttsProvider
        AssistantSpeechField.VOICE -> saved.ttsVoice
    }
    val reported = when (field) {
        AssistantSpeechField.LANGUAGE -> if (state.speechNetworkFallback) options.recognitionSupportedLanguages else options.recognitionInstalledLanguages
        AssistantSpeechField.ENGINE -> options.ttsProviders
        AssistantSpeechField.VOICE -> voiceEntries.keys.toList()
    }
    val evidence = when (field) {
        AssistantSpeechField.LANGUAGE -> when {
            reported == null -> "Language inventory: not reported"
            reported.isEmpty() -> "No ${if (state.speechNetworkFallback) "supported" else "installed"} languages reported"
            else -> "Reported ${if (state.speechNetworkFallback) "supported" else "installed"} languages · not processing-route proof"
        }
        AssistantSpeechField.ENGINE -> if (reported.isNullOrEmpty()) "Engine inventory: not reported"
            else "Reported engines" + if ("" in reported) " · System engine observed" else ""
        AssistantSpeechField.VOICE -> if (reported.isNullOrEmpty()) "Voices for the requested engine: not reported"
            else "Reported voices for the requested engine · not hidden model identifiers"
    }
    val choices = listOf(AssistantSpeechChoice("", "Automatic", true)) + reported.orEmpty().filter(String::isNotEmpty).distinct().map { candidate ->
        val selection = assistantSpeechCandidate(saved, field, candidate)
        val entries = voiceEntries[candidate].orEmpty()
        val enabled = runCatching { options.validate(selection, state.speechNetworkFallback) }.isSuccess &&
            (field != AssistantSpeechField.VOICE || entries.size == 1)
        val label = if (field == AssistantSpeechField.VOICE) {
            val entry = entries.first()
            if (entries.size != 1) "$candidate · conflicting reported metadata · unavailable"
            else "$candidate · ${entry.language} · ${if (entry.networkRequired) "network required" else "network not required"}"
        } else candidate
        AssistantSpeechChoice(candidate, label, enabled)
    }
    val available = runCatching { options.validate(saved, state.speechNetworkFallback) }.isSuccess &&
        (field != AssistantSpeechField.VOICE || value.isEmpty() || choices.singleOrNull { it.value == value }?.enabled == true)
    val savedReported = value.isEmpty() || reported.orEmpty().any { candidate ->
        candidate.equals(value, ignoreCase = field == AssistantSpeechField.LANGUAGE)
    }
    return AssistantSpeechCatalog(value, value.ifEmpty { "Automatic" }, savedReported, available, evidence, choices)
}

internal fun assistantCanSelectSpeech(state: AssistantState, voice: AssistantVoiceState, pending: Boolean = false): Boolean =
    !pending && state.foreground && !state.busy && !voice.active

internal fun assistantCanChangeSpeechFallback(state: AssistantState, voice: AssistantVoiceState): Boolean =
    state.foreground && (!state.speechNetworkFallback || voice.localOnlyRecognitionAvailable || voice.speechOptions.localOnlyRecognitionAvailable)

internal fun assistantSpeechFallbackDescription(state: AssistantState, voice: AssistantVoiceState): String = when {
    !state.foreground -> "Keep Chat foreground to change speech policy"
    !voice.admissionReady -> "Speech resources are not ready. Dedicated on-device capability is not an active recognition route."
    !state.speechNetworkFallback -> "Dedicated on-device recognition required; network speech disabled"
    voice.localOnlyRecognitionAvailable || voice.speechOptions.localOnlyRecognitionAvailable -> "Future speech may use network"
    else -> "Dedicated on-device API unavailable or not reported. Ordinary recognition may still work offline; strict fallback-off is unavailable."
}

internal fun assistantDictationAvailabilityDescription(state: AssistantState, voice: AssistantVoiceState): String = when {
    !state.foreground -> "Keep Chat foreground to check dictation readiness. Keyboard remains available."
    !voice.admissionReady -> "Speech resources are not ready. This does not establish whether offline recognition is available. Keyboard remains available."
    !voice.recognitionServiceAvailable -> "Native dictation is unavailable under the current speech policy. Keyboard remains available."
    else -> "Dictation requests the configured language. An offline preference alone is not a local-processing guarantee."
}

internal fun assistantSpeechSelectionErrorText(code: String): String = when (code) {
    "speech_selection_rejected" -> "Speech choice was not accepted. Recheck reported options or reset to Automatic."
    "speech_selection_pending" -> "Waiting for native speech-settings settlement. Make the next choice after it finishes."
    "speech_selection_not_saved" -> "Speech choice was not saved. Recheck reported options and retry deliberately."
    "speech_selection_unconfirmed" -> "Speech choice has not been confirmed here. Recheck native settings before retrying."
    else -> assistantVoiceErrorText(code)
}
