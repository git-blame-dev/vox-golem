package dev.gitblame.voxgolem.assistant.recognition

import org.junit.Assert.*
import org.junit.Test

class SpeechLocalesTest {
    @Test fun exactMatchIsNormalizedWithoutChangingProviderTag() {
        assertEquals("en_au", SpeechLocales.installedMatch("en-AU", listOf("en-US", "en_au")))
    }
    @Test fun fallbackUsesProviderOrderAndDoesNotImposeBritishEnglish() {
        assertEquals("en-US", SpeechLocales.installedMatch("en-AU", listOf("fr-FR", "en-US", "en-GB")))
        assertEquals("en-GB", SpeechLocales.installedMatch("en-AU", listOf("en-GB", "en-US")))
    }
    @Test fun unsupportedLanguageMalformedTagsAndConflictingScriptsAreNotInstalledMatches() {
        assertNull(SpeechLocales.installedMatch("en-AU", listOf("fr-FR", "", "invalid_tag_!")))
        assertNull(SpeechLocales.installedMatch("zh-Hant-TW", listOf("zh-Hans-CN")))
        assertNull(SpeechLocales.installedMatch("", listOf("en-AU")))
        assertNull(SpeechLocales.parse("und"))
    }
}
