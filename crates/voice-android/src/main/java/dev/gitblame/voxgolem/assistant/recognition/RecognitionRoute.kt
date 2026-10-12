package dev.gitblame.voxgolem.assistant.recognition

/** Local preference is not an offline guarantee; the provider may control processing. */
data class RecognitionRoute(
    val provider: String,
    val requestedLanguage: String,
    val selectedLanguage: String? = null,
    val reportedLanguage: String? = null,
    val localPreferred: Boolean = true,
    val fallbackReason: String? = null,
    val onDeviceOnly: Boolean = false,
    val installedLanguages: List<String>? = null,
    val supportedLanguages: List<String>? = null,
)
