package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.recognition.DictationState
import dev.gitblame.voxgolem.assistant.speech.SpeechOptions
import dev.gitblame.voxgolem.assistant.speech.SpeechSelection
import dev.gitblame.voxgolem.assistant.speech.SpeechVoiceOption
import org.junit.Assert.*
import org.junit.Test

class AssistantSpeechSelectionPresentationTest {
    private val ready = AssistantState(foreground = true)
    private val inventory = SpeechOptions(ttsProviders = listOf("", "synthetic.engine.one", "synthetic.engine.two"),
        ttsVoices = listOf(SpeechVoiceOption("synthetic.engine.one", "synthetic-local-voice", "en-AU", false),
            SpeechVoiceOption("synthetic.engine.one", "synthetic-network-voice", "en-AU", true),
            SpeechVoiceOption("synthetic.engine.two", "synthetic-other-engine-voice", "en-AU", false)),
        recognitionInstalledLanguages = listOf("en-AU"), recognitionSupportedLanguages = listOf("en-AU", "en-GB"),
        localOnlyRecognitionAvailable = true)

    @Test fun unknownCatalogOffersOnlyAutomaticAndDoesNotInventLanguagesEnginesVoicesOrModels() {
        for (field in AssistantSpeechField.values()) {
            val catalog = assistantSpeechCatalog(ready, AssistantVoiceState(), field)
            assertEquals(listOf(AssistantSpeechChoice("", "Automatic", true)), catalog.choices)
            assertEquals("Automatic", catalog.savedLabel)
            assertTrue(catalog.evidence.contains("not reported"))
        }
    }

    @Test fun choicesUseReportedInventoryAndVoicesNeverCrossThePersistedRequestedEngine() {
        val state = ready.copy(speechLanguage = "en-AU", speechTtsProvider = "synthetic.engine.one")
        val voice = AssistantVoiceState(speechOptions = inventory, speechSelection = SpeechSelection("en-GB", "synthetic.engine.two"))
        val languages = assistantSpeechCatalog(state, voice, AssistantSpeechField.LANGUAGE)
        assertEquals(listOf("", "en-AU", "en-GB"), languages.choices.map { it.value })
        assertEquals("en-AU", languages.savedValue)
        val engines = assistantSpeechCatalog(state, voice, AssistantSpeechField.ENGINE)
        assertEquals(listOf("", "synthetic.engine.one", "synthetic.engine.two"), engines.choices.map { it.value })
        assertTrue(engines.evidence.contains("System engine observed"))
        val voices = assistantSpeechCatalog(state, voice, AssistantSpeechField.VOICE)
        assertEquals(listOf("", "synthetic-local-voice", "synthetic-network-voice"), voices.choices.map { it.value })
        assertTrue(voices.choices.all { it.enabled })
        assertTrue(voices.choices.last().label.contains("network required"))
    }

    @Test fun removedSavedChoicesStayVisibleAndAutomaticRemainsAnExplicitRecoveryChoice() {
        val state = ready.copy(speechLanguage = "en-AU", speechTtsProvider = "synthetic.removed.engine", speechTtsVoice = "synthetic-removed-voice")
        val voice = AssistantVoiceState(speechOptions = inventory.copy(recognitionSupportedLanguages = emptyList()))
        for (field in AssistantSpeechField.values()) {
            val catalog = assistantSpeechCatalog(state, voice, field)
            assertFalse(catalog.savedReported)
            assertFalse(catalog.savedAvailable)
            assertTrue(catalog.savedValue.isNotEmpty())
            assertEquals(AssistantSpeechChoice("", "Automatic", true), catalog.choices.first())
        }
    }

    @Test fun observedSystemEngineVoiceUsesTheEmptyProviderKeyWithoutInventingANamedEngine() {
        val options = inventory.copy(ttsVoices = inventory.ttsVoices + SpeechVoiceOption("", "synthetic-system-voice", "en-AU", false))
        val catalog = assistantSpeechCatalog(ready.copy(speechLanguage = "en-AU"), AssistantVoiceState(speechOptions = options), AssistantSpeechField.VOICE)
        assertEquals(listOf("", "synthetic-system-voice"), catalog.choices.map { it.value })
        assertTrue(catalog.choices.all { it.enabled })
    }

    @Test fun contradictoryVoiceMetadataCannotAdvertiseANetworkVoiceAsAPolicySafeLocalChoice() {
        val options = inventory.copy(ttsVoices = inventory.ttsVoices + SpeechVoiceOption("synthetic.engine.one", "synthetic-network-voice", "en-AU", false))
        val state = ready.copy(speechNetworkFallback = false, speechLanguage = "en-AU", speechTtsProvider = "synthetic.engine.one",
            speechTtsVoice = "synthetic-network-voice")
        val catalog = assistantSpeechCatalog(state, AssistantVoiceState(speechOptions = options), AssistantSpeechField.VOICE)
        val choice = catalog.choices.single { it.value == "synthetic-network-voice" }
        assertFalse(choice.enabled)
        assertTrue(choice.label.contains("conflicting reported metadata"))
        assertTrue(catalog.savedReported)
        assertFalse(catalog.savedAvailable)
        assertTrue(catalog.choices.first().enabled)
    }

    @Test fun requestedLanguageCaseAndUnreportedRegionalVariantsRemainDistinctFromCatalogFacts() {
        val voice = AssistantVoiceState(speechOptions = inventory.copy(recognitionSupportedLanguages = listOf("en-AU")))
        val canonical = assistantSpeechCatalog(ready.copy(speechLanguage = "en-au"), voice, AssistantSpeechField.LANGUAGE)
        assertTrue(canonical.savedReported)
        assertEquals("en-au", canonical.savedLabel)
        val variant = assistantSpeechCatalog(ready.copy(speechLanguage = "en-GB"), voice, AssistantSpeechField.LANGUAGE)
        assertFalse(variant.savedReported)
        assertTrue("Native language-family admission is not an exact reported locale", variant.savedAvailable)
        assertEquals(listOf("", "en-AU"), variant.choices.map { it.value })
    }

    @Test fun networkPolicyDisablesNetworkVoiceAndUsesOnlyInstalledLocalLanguageChoices() {
        val state = ready.copy(speechNetworkFallback = false, speechLanguage = "en-AU", speechTtsProvider = "synthetic.engine.one",
            speechTtsVoice = "synthetic-network-voice")
        val voice = AssistantVoiceState(speechOptions = inventory)
        val voices = assistantSpeechCatalog(state, voice, AssistantSpeechField.VOICE)
        assertTrue(voices.savedReported)
        assertFalse(voices.savedAvailable)
        assertFalse(voices.choices.single { it.value == "synthetic-network-voice" }.enabled)
        assertTrue(voices.choices.single { it.value == "synthetic-local-voice" }.enabled)
        assertTrue(voices.choices.first().enabled)
        val languages = assistantSpeechCatalog(state.copy(speechTtsVoice = ""), voice, AssistantSpeechField.LANGUAGE)
        assertEquals(listOf("", "en-AU"), languages.choices.map { it.value })
        assertTrue(languages.evidence.contains("installed"))
        val unavailable = assistantSpeechCatalog(state.copy(speechTtsVoice = ""), voice.copy(speechOptions = inventory.copy(localOnlyRecognitionAvailable = false)), AssistantSpeechField.LANGUAGE)
        assertFalse(unavailable.choices.single { it.value == "en-AU" }.enabled)
    }

    @Test fun changingEngineClearsVoiceExplicitlyButOtherSelectionsNeverSubstituteAnEngineOrVoice() {
        val saved = SpeechSelection("en-AU", "synthetic.engine.one", "synthetic-voice")
        assertEquals(SpeechSelection("en-AU", "synthetic.engine.two", ""), assistantSpeechCandidate(saved, AssistantSpeechField.ENGINE, "synthetic.engine.two"))
        assertEquals(SpeechSelection("en-AU", "", ""), assistantSpeechCandidate(saved, AssistantSpeechField.ENGINE, ""))
        assertEquals(saved, assistantSpeechCandidate(saved, AssistantSpeechField.ENGINE, saved.ttsProvider))
        assertEquals(saved.copy(language = "en-GB"), assistantSpeechCandidate(saved, AssistantSpeechField.LANGUAGE, "en-GB"))
        assertEquals(saved.copy(ttsVoice = ""), assistantSpeechCandidate(saved, AssistantSpeechField.VOICE, ""))
    }

    @Test fun nativeInventoryBoundsAreReusedAndAValueBeyondTheBoundIsNotInventedBackIntoTheCatalog() {
        val many = inventory.copy(ttsProviders = listOf("synthetic.engine.one") + (0..20).map { "synthetic.extra.engine.$it" },
            ttsVoices = (0..1200).map { SpeechVoiceOption("synthetic.engine.one", "synthetic-voice-$it", "en-AU", false) })
        val state = ready.copy(speechLanguage = "en-AU", speechTtsProvider = "synthetic.engine.one", speechTtsVoice = "synthetic-voice-1200")
        val voice = AssistantVoiceState(speechOptions = many)
        assertEquals(9, assistantSpeechCatalog(state, voice, AssistantSpeechField.ENGINE).choices.size)
        val voices = assistantSpeechCatalog(state, voice, AssistantSpeechField.VOICE)
        assertEquals(1025, voices.choices.size)
        assertFalse(voices.savedReported)
        assertTrue(voices.choices.none { it.value == state.speechTtsVoice })
    }

    @Test fun anExplicitlyEmptyReportedLanguageListIsNotAnUnknownInventory() {
        val voice = AssistantVoiceState(speechOptions = inventory.copy(recognitionSupportedLanguages = emptyList()))
        val catalog = assistantSpeechCatalog(ready, voice, AssistantSpeechField.LANGUAGE)
        assertEquals("No supported languages reported", catalog.evidence)
        assertEquals(listOf(AssistantSpeechChoice("", "Automatic", true)), catalog.choices)
    }

    @Test fun fallbackRecoveryAndSpeechAdmissionRespectCapabilitiesAndBusyState() {
        val unknown = AssistantVoiceState()
        assertFalse(assistantCanChangeSpeechFallback(ready, unknown))
        assertTrue(assistantCanChangeSpeechFallback(ready.copy(speechNetworkFallback = false), unknown))
        assertTrue(assistantCanChangeSpeechFallback(ready, unknown.copy(localOnlyRecognitionAvailable = true)))
        assertTrue(assistantCanChangeSpeechFallback(ready, unknown.copy(speechOptions = inventory)))
        assertFalse(assistantCanChangeSpeechFallback(ready.copy(foreground = false, speechNetworkFallback = false), unknown))
        assertTrue(assistantCanSelectSpeech(ready, unknown))
        assertFalse("Pending settlement disables even an idle Automatic reset", assistantCanSelectSpeech(ready, unknown, pending = true))
        assertFalse(assistantCanSelectSpeech(ready.copy(busy = true), unknown))
        assertFalse(assistantCanSelectSpeech(ready.copy(foreground = false), unknown))
        assertFalse(assistantCanSelectSpeech(ready, unknown.copy(dictation = DictationState(phase = DictationPhase.LISTENING))))
    }

    @Test fun capabilityStatusSeparatesBackgroundResourcePreparationAndDedicatedRouteEvidence() {
        val voice = AssistantVoiceState(admissionReady = true, recognitionServiceAvailable = false)
        assertEquals("Keep Chat foreground to change speech policy",
            assistantSpeechFallbackDescription(ready.copy(foreground = false), voice))
        assertTrue(assistantSpeechFallbackDescription(ready, voice.copy(admissionReady = false)).contains("Speech resources are not ready"))
        val unknown = assistantSpeechFallbackDescription(ready, voice)
        assertTrue(unknown.contains("Dedicated on-device API unavailable or not reported"))
        assertTrue(unknown.contains("Ordinary recognition may still work offline"))
        assertEquals("Future speech may use network", assistantSpeechFallbackDescription(ready,
            voice.copy(localOnlyRecognitionAvailable = true)))
        assertEquals("Future speech may use network", assistantSpeechFallbackDescription(ready,
            voice.copy(speechOptions = inventory)))
        assertEquals("Dedicated on-device recognition required; network speech disabled",
            assistantSpeechFallbackDescription(ready.copy(speechNetworkFallback = false), voice))
        assertTrue(assistantDictationAvailabilityDescription(ready.copy(foreground = false), voice).contains("Keep Chat foreground"))
        assertTrue(assistantDictationAvailabilityDescription(ready, voice.copy(admissionReady = false))
            .contains("does not establish whether offline recognition is available"))
        assertTrue(assistantDictationAvailabilityDescription(ready, voice).contains("under the current speech policy"))
        assertTrue(assistantDictationAvailabilityDescription(ready, voice.copy(recognitionServiceAvailable = true))
            .contains("not a local-processing guarantee"))
    }
}
