package dev.gitblame.voxgolem.assistant.phone

import org.junit.Assert.*
import org.junit.Test

class PhonePreferencesTest {
    @Test fun retainsAllLegacyKeysAndEnumSpellings() {
        val value = PhonePreferences.defaults().put("deep", "custom-luna-low").put("review", "custom-luna-low").put("ui_text_size", "extra_large")
        PhonePreferences.validate(value)
        assertEquals(16, value.length())
        assertTrue(value.getBoolean("web_enabled"))
        assertTrue(value.getBoolean("speech_network_fallback"))
        for (key in listOf("speech_language", "speech_tts_provider", "speech_tts_voice")) assertEquals("", value.getString(key))
    }
    @Test fun unavailableFlagsAreNotSilentlyDowngraded() {
        for (key in listOf("deep_enabled", "review_enabled", "prefetch", "completion")) {
            try { PhonePreferences.validate(PhonePreferences.defaults().put(key, true)); fail("must reject unsupported capability") }
            catch (error: AssistantPhoneFailure) { assertEquals("unavailable", error.code) }
        }
    }
    @Test fun acceptsNativeTtsPreferenceWithAvailabilityValidatedByBackend() {
        PhonePreferences.validate(PhonePreferences.defaults().put("tts_enabled", true))
    }
    @Test fun acceptsDisabledWebAndStrictSpeechPolicyWithoutBooleanCoercion() {
        PhonePreferences.validate(PhonePreferences.defaults().put("web_enabled", false).put("speech_network_fallback", false))
        for (key in listOf("web_enabled", "speech_network_fallback")) {
            try { PhonePreferences.validate(PhonePreferences.defaults().put(key, "false")); fail("must require boolean") }
            catch (error: AssistantPhoneFailure) { assertEquals("invalid_input", error.code) }
        }
    }
    @Test fun olderPayloadGetsOnlyTheDocumentedNewBooleanAndStringDefaults() {
        val legacy = PhonePreferences.defaults().apply {
            remove("web_enabled"); remove("speech_network_fallback")
            remove("speech_language"); remove("speech_tts_provider"); remove("speech_tts_voice")
        }
        PhonePreferences.validate(legacy)
        val normalized = PhonePreferences.normalize(legacy)
        assertTrue(normalized.getBoolean("web_enabled")); assertTrue(normalized.getBoolean("speech_network_fallback"))
        for (key in listOf("speech_language", "speech_tts_provider", "speech_tts_voice")) assertEquals("", normalized.getString(key))
        assertFalse(legacy.has("web_enabled"))
        legacy.remove("tts_enabled")
        assertThrows(AssistantPhoneFailure::class.java) { PhonePreferences.validate(legacy) }
    }
    @Test fun unknownKeysAndBooleanCoercionAreRejected() {
        for (value in listOf(PhonePreferences.defaults().put("extra", false), PhonePreferences.defaults().put("completion", "false"))) {
            try { PhonePreferences.validate(value); fail("must reject malformed preferences") }
            catch (error: AssistantPhoneFailure) { assertEquals("invalid_input", error.code) }
        }
    }
    @Test fun speechSelectionShapeRequiresStringsAndAValidLocaleWithoutInventingSdkSupport() {
        PhonePreferences.validate(PhonePreferences.defaults().put("speech_language", "en-AU")
            .put("speech_tts_provider", "synthetic.tts").put("speech_tts_voice", "synthetic-au-local"))
        for (value in listOf(PhonePreferences.defaults().put("speech_language", "en--AU"),
            PhonePreferences.defaults().put("speech_tts_voice", false), PhonePreferences.defaults().put("speech_tts_provider", "x".repeat(513)))) {
            assertThrows(AssistantPhoneFailure::class.java) { PhonePreferences.validate(value) }
        }
    }
    @Test fun multibyteSpeechIdsRespectTheCoreUtf8ByteLimitBeforePreferencePersistence() {
        val exact = "界".repeat(170) + "ab"
        PhonePreferences.validate(PhonePreferences.defaults().put("speech_tts_provider", exact).put("speech_tts_voice", exact))
        for (key in listOf("speech_tts_provider", "speech_tts_voice")) {
            assertThrows(AssistantPhoneFailure::class.java) { PhonePreferences.validate(PhonePreferences.defaults().put(key, "界".repeat(171))) }
        }
    }
}
