package dev.gitblame.voxgolem.assistant.speech

import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class SpeechSelectionTest {
    private val choices = SpeechOptions(ttsProviders = listOf("synthetic.tts"), ttsVoices = listOf(
        SpeechVoiceOption("synthetic.tts", "synthetic-au-local", "en-AU", false),
        SpeechVoiceOption("synthetic.tts", "synthetic-au-network", "en-AU", true)),
        recognitionInstalledLanguages = listOf("en-US", "en-AU"), recognitionSupportedLanguages = listOf("en-US", "en-AU", "fr-FR"),
        localOnlyRecognitionAvailable = true)
    @Test fun automaticDefaultsNeedNoInventedAvailabilityAndExplicitSelectionsUseObservedChoices() {
        SpeechOptions().validate(SpeechSelection(), false)
        choices.validate(SpeechSelection("en-AU", "synthetic.tts", "synthetic-au-local"), false)
        choices.validate(SpeechSelection("en-AU", "synthetic.tts", "synthetic-au-network"), true)
        choices.validate(SpeechSelection("fr-FR"), true)
        assertThrows(IllegalArgumentException::class.java) { choices.validate(SpeechSelection("fr-FR"), false) }
        assertThrows(IllegalArgumentException::class.java) { SpeechOptions().validate(SpeechSelection("en-AU"), true) }
    }
    @Test fun invalidUnknownOrPolicyForbiddenSelectionsAreRejectedRatherThanSubstituted() {
        for (selection in listOf(SpeechSelection("en--AU"), SpeechSelection("zz-ZZ"),
            SpeechSelection(ttsProvider = "synthetic.missing"), SpeechSelection("en-AU", "synthetic.tts", "synthetic.missing"),
            SpeechSelection("en-AU", "synthetic.tts", "synthetic-au-network"))) {
            assertThrows(IllegalArgumentException::class.java) { choices.validate(selection, false) }
        }
        assertThrows(IllegalArgumentException::class.java) {
            choices.copy(localOnlyRecognitionAvailable = false).validate(SpeechSelection("en-AU"), false)
        }
    }
    @Test fun optionProjectionIsBoundedAndHiddenModelsRemainUnknownNotSelectable() {
        val many = SpeechOptions(ttsProviders = (0..20).map { "synthetic.provider.$it" },
            ttsVoices = (0..1200).map { SpeechVoiceOption("synthetic.tts", "synthetic.voice.$it", "en-AU", false) },
            recognitionSupportedLanguages = listOf("en_AU", "en-AU", "invalid--locale"))
        val value = many.json()
        assertEquals(8, value.getJSONArray("ttsProviders").length())
        assertEquals(1024, value.getJSONArray("ttsVoices").length())
        assertEquals(1, value.getJSONArray("recognitionSupportedLanguages").length())
        assertTrue(value.isNull("recognitionInstalledLanguages"))
        assertFalse(value.getBoolean("modelSelectionAvailable")); assertTrue(value.isNull("modelIdentifiers"))
    }
    @Test fun opaqueIdsMustFitThe512ByteUtf8WireWithoutBeingTruncatedOrRepaired() {
        val exact = "界".repeat(170) + "ab"
        val oversized = "界".repeat(171)
        val supplementary = "🧪".repeat(128)
        assertEquals(512, exact.toByteArray(Charsets.UTF_8).size)
        assertEquals(513, oversized.toByteArray(Charsets.UTF_8).size)
        assertEquals(512, supplementary.toByteArray(Charsets.UTF_8).size)
        for (accepted in listOf(exact, supplementary, "x".repeat(512))) SpeechSelection(ttsProvider = accepted, ttsVoice = accepted).validateShape()
        for (rejected in listOf(oversized, "x".repeat(513), "\uD83E")) {
            assertThrows(IllegalArgumentException::class.java) { SpeechSelection(ttsProvider = rejected).validateShape() }
            assertThrows(IllegalArgumentException::class.java) { SpeechSelection(ttsVoice = rejected).validateShape() }
        }
        val value = SpeechOptions(ttsProviders = listOf(exact, oversized), ttsVoices = listOf(
            SpeechVoiceOption(exact, supplementary, "en-AU", false),
            SpeechVoiceOption(oversized, "synthetic-local", "en-AU", false),
            SpeechVoiceOption(exact, oversized, "en-AU", false))).json()
        assertEquals(1, value.getJSONArray("ttsProviders").length())
        assertEquals(exact, value.getJSONArray("ttsProviders").getString(0))
        assertEquals(1, value.getJSONArray("ttsVoices").length())
        assertEquals(supplementary, value.getJSONArray("ttsVoices").getJSONObject(0).getString("name"))
        assertEquals(exact, value.getJSONArray("ttsVoices").getJSONObject(0).getString("provider"))
    }
}
