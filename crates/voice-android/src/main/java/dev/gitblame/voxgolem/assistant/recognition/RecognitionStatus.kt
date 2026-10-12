package dev.gitblame.voxgolem.assistant.recognition

/** The raw code remains in DictationState for diagnostics. No-match is not a successful final. */
fun recognitionUiError(code: String?): String? = when (code) {
    "recognition_error_7" -> null
    "recorded_input_provider_unproven" -> "Recorded input is unavailable until the supplied-audio provider is validated"
    "recording_failed", "recording_save_failed" -> "Recording failed; available audio is retained for recovery. Nothing was sent"
    "recording_metadata_failed" -> "Recording status could not be saved. Nothing was sent"
    else -> code
}
