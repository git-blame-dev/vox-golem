package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.recognition.RecognitionRoute
import org.junit.Assert.*
import org.junit.Test

class SpeechRouteDescriptionTest {
    @Test fun providerControlledFallbackDoesNotPretendToBeInstalledOrOffline() {
        val route = RecognitionRoute("com.google.android.tts/SyntheticService", "en-AU", localPreferred = false,
            fallbackReason = "installed_language_unavailable")
        assertEquals("Google · requested en-AU · provider-controlled · actual local/network processing: not reported · fallback", route.description())
    }
    @Test fun requestedSelectedAndReportedVariantsRemainDistinct() {
        val route = RecognitionRoute("com.google.android.tts/SyntheticService", "en-AU", "en-US", "en-GB")
        assertEquals("Google · requested en-AU · selected en-US · reported en-GB · local preferred · provider-controlled · actual local/network processing: not reported · fallback", route.description())
    }
    @Test fun matchingLocalesStillIdentifyRequestedSelectedAndReportedEvidence() {
        val route = RecognitionRoute("com.google.android.tts/SyntheticService", "en-AU", "en-AU", "en-AU")
        assertEquals("Google · requested en-AU · selected en-AU · reported en-AU · local preferred · provider-controlled · actual local/network processing: not reported", route.description())
    }
    @Test fun reportedVariantDoesNotInventASelectionFallback() {
        val route = RecognitionRoute("com.google.android.tts/SyntheticService", "en-AU", "en-AU", "en-US")
        assertFalse(route.description().contains("fallback"))
        assertTrue(route.description().contains("selected en-AU · reported en-US"))
    }
    @Test fun unknownSelectedAndReportedLocalesAreNotPresentedAsConfirmed() {
        val route = RecognitionRoute("synthetic.provider/SyntheticService", "en-AU")
        assertEquals("synthetic.provider · requested en-AU · local preferred · provider-controlled · actual local/network processing: not reported", route.description())
    }
    @Test fun dedicatedOnDeviceSdkRouteIsDistinctFromAProviderOfflinePreference() {
        val route = RecognitionRoute("android.on-device", "en-AU", "en-AU", onDeviceOnly = true)
        assertEquals("System speech · requested en-AU · selected en-AU · processing: on-device", route.description())
        assertFalse(route.description().contains("may use network"))
    }
    @Test fun localTtsAccentFallbackIsVisibleWithoutImplyingNetworkUse() {
        val route = TtsRoute("com.samsung.SMT", "en-AU", "en-GB", false, "configured_variant_unavailable")
        assertEquals("Samsung · requested en-AU · selected en-GB · installed local voice selected · network not required · fallback", route.description())
    }
    @Test fun nativeTtsProviderPreferenceAndNetworkVoiceRemainExplicit() {
        val route = TtsRoute("com.google.android.tts", "en-AU", "en-AU", true,
            "no_installed_local_voice", providerIsPreference = true)
        assertEquals("Google preference · requested en-AU · selected en-AU · network voice selected · network required · fallback", route.description())
        assertFalse(route.description().contains("reported"))
    }
}
