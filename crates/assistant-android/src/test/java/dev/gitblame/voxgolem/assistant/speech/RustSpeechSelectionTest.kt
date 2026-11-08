package dev.gitblame.voxgolem.assistant.speech

import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

@RunWith(dev.gitblame.voxgolem.assistant.AssistantStoreRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class RustSpeechSelectionTest {
    @Test fun sdkNormalizedObservationsRoundTripToRustRankingAndEligibility() {
        val requested = java.util.Locale.forLanguageTag("en-AU")
        val voices = listOf(
            LocalVoice("synthetic-network", requested, true, true),
            LocalVoice("synthetic-local-us", java.util.Locale.US, false, true),
            LocalVoice("synthetic-local-au", requested, false, true))
        fun select(observed: List<LocalVoice> = voices, network: Boolean = true,
            explicit: String = "", default: String? = null) =
            RustObservedVoiceSelector().select(requested, network, explicit, default, observed)
        assertEquals(2, select(default = "synthetic-local-us"))
        assertEquals(1, select(voices.take(2)))
        assertEquals(0, select(explicit = "synthetic-network"))
        assertEquals(-1, select(network = false, explicit = "synthetic-network"))
        assertEquals(-1, select(explicit = "synthetic-missing"))
        assertEquals(-1, select(voices.map { it.copy(installed = false) }))
        val scripted = listOf(LocalVoice("synthetic-script", java.util.Locale.forLanguageTag("sr-Cyrl-RS"), false, true))
        assertEquals(-1, RustObservedVoiceSelector().select(java.util.Locale.forLanguageTag("sr-Latn-RS"), true, "", null, scripted))
        assertEquals(1, select(listOf(voices[2].copy(name = "synthetic-a"), voices[2].copy(name = "synthetic-z")), default = "synthetic-z"))
        assertEquals(0, select(listOf(voices[2].copy(name = "synthetic-a"), voices[2].copy(name = "synthetic-z"))))
        assertEquals(-1, SpeechSelectionNative.select("{}"))
    }
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
            SpeechSelection(ttsProvider = "synthetic.missing"), SpeechSelection("fr-FR", "synthetic.tts", "synthetic-au-local"), SpeechSelection("en-AU", "synthetic.tts", "synthetic.missing"),
            SpeechSelection("en-AU", "synthetic.tts", "synthetic-au-network"))) {
            assertThrows(IllegalArgumentException::class.java) { choices.validate(selection, false) }
        }
        assertThrows(IllegalArgumentException::class.java) {
            choices.copy(localOnlyRecognitionAvailable = false).validate(SpeechSelection("en-AU"), false)
        }
    }
}

private fun SpeechOptions.validate(selection: SpeechSelection, networkFallback: Boolean) =
    validate(selection, networkFallback, RustObservedVoiceSelector())
