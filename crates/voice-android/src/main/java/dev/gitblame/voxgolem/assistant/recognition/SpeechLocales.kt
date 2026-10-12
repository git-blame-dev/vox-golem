package dev.gitblame.voxgolem.assistant.recognition

import java.util.Locale
import java.util.IllformedLocaleException

object SpeechLocales {
    fun installedMatch(requested: String, installed: List<String>): String? {
        val locale = parse(requested) ?: return null
        return installed.firstOrNull { parse(it) == locale }
            ?: installed.firstOrNull {
                val candidate = parse(it)
                candidate != null && candidate.language == locale.language &&
                    (locale.script.isBlank() || candidate.script.isBlank() || candidate.script == locale.script)
            }
    }
    fun parse(value: String): Locale? = try {
        Locale.Builder().setLanguageTag(value.trim().replace('_', '-')).build()
            .takeIf { it.language.isNotBlank() && it.language != "und" }
    } catch (_: IllformedLocaleException) { null }
}
