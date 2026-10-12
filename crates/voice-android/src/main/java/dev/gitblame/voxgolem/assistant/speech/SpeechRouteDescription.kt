package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.recognition.RecognitionRoute

private fun speechProvider(provider: String?): String = when {
    provider == null || provider.startsWith("android.") -> "System speech"
    provider.substringBefore('/') == "com.google.android.as" -> "Google System Intelligence"
    provider.startsWith("com.google.") -> "Google"
    provider.startsWith("com.samsung.") -> "Samsung"
    else -> provider.substringBefore('/').take(80)
}

fun RecognitionRoute.description(): String {
    val selected = selectedLanguage?.let { " · selected $it" }.orEmpty()
    val reported = reportedLanguage?.let { " · reported $it" }.orEmpty()
    val mode = when {
        onDeviceOnly -> "processing: on-device"
        localPreferred -> "local preferred · provider-controlled · actual local/network processing: not reported"
        else -> "provider-controlled · actual local/network processing: not reported"
    }
    val variantFallback = selectedLanguage?.let { !it.equals(requestedLanguage, true) } == true
    val fallback = if (fallbackReason != null || variantFallback) " · fallback" else ""
    return "${speechProvider(provider)} · requested $requestedLanguage$selected$reported · $mode$fallback"
}

fun TtsRoute.description(): String {
    val mode = if (networkRequired) "network voice selected · network required" else "installed local voice selected · network not required"
    val fallback = if (fallbackReason != null) " · fallback" else ""
    val engine = speechProvider(provider) + if (providerIsPreference) " preference" else ""
    return "$engine · requested $requestedLanguage · selected $selectedLanguage · $mode$fallback"
}
