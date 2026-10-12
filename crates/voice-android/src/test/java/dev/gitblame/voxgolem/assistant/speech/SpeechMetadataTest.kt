package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.recognition.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class SpeechMetadataTest {
    @Test fun providerPreferredInstalledLanguageDoesNotProveCurrentRouteOrHiddenModel() {
        val value = RecognitionRoute("synthetic.provider/Speech", "en-AU", "en-AU",
            installedLanguages = listOf("en-AU")).metadata()
        assertEquals("en-AU", value.getString("requestedLanguage"))
        assertEquals("en-AU", value.getJSONArray("installedLanguages").getString(0))
        assertTrue(value.isNull("reportedLanguage")); assertTrue(value.isNull("model"))
        assertEquals("not_reported", value.getString("actualProcessing"))
        assertFalse(value.getBoolean("offlineTrialVerified"))
    }
    @Test fun ttsEnginePreferenceVoiceSelectionAndUnknownProviderReportRemainDistinct() {
        val value = TtsRoute("synthetic.tts", "en-AU", "en-GB", false, providerIsPreference = true,
            selectedVoice = "synthetic-local-voice", installedLanguages = listOf("en-GB")).metadata()
        assertEquals("synthetic.tts", value.getString("configuredProvider"))
        assertTrue(value.isNull("reportedProvider")); assertTrue(value.isNull("model"))
        assertEquals("synthetic-local-voice", value.getString("selectedVoice"))
        assertFalse(value.getBoolean("networkRequired"))
        assertEquals("not_reported", value.getString("actualProcessing"))
        assertNull(recognitionUiError("recognition_error_7"))
        assertEquals("recognition_cleanup_failed", recognitionUiError("recognition_cleanup_failed"))
    }
}
