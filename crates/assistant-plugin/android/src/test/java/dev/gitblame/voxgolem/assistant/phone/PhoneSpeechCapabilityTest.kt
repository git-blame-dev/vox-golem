package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import android.content.Intent
import android.content.pm.ResolveInfo
import android.content.pm.ServiceInfo
import android.speech.RecognitionService
import org.robolectric.Shadows.shadowOf
import org.junit.After
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowSpeechRecognizer

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class PhoneSpeechCapabilityTest {
    @After fun resetAdmission() { AssistantRuntime.configureCaptureResourceAdmission(false) }
    @Test fun verifiedPhoneLeaseWiringAndRealOnDeviceFactoryExposeDictation() {
        AssistantRuntime.configureCaptureResourceAdmission(true)
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        assertTrue(PhoneSpeechSession.recognitionAvailable(RuntimeEnvironment.getApplication()))
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(false)
        assertFalse(PhoneSpeechSession.recognitionAvailable(RuntimeEnvironment.getApplication()))
    }
    @Test fun absentAdmissionNeverClaimsAnAvailableMicrophone() {
        AssistantRuntime.configureCaptureResourceAdmission(false)
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        assertFalse(PhoneSpeechSession.recognitionAvailable(RuntimeEnvironment.getApplication()))
    }
    @Suppress("DEPRECATION")
    @Test @Config(sdk = [32]) fun legacyConfiguredProviderIsAvailableForPermittedFallbackWithoutLocalMetadata() {
        AssistantRuntime.configureCaptureResourceAdmission(true)
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context.packageManager).addResolveInfoForIntent(Intent(RecognitionService.SERVICE_INTERFACE), ResolveInfo().apply {
            serviceInfo = ServiceInfo().apply {
                packageName = "synthetic.speech.provider"
                name = "synthetic.speech.provider.SyntheticRecognitionService"
            }
        })
        assertTrue(PhoneSpeechSession.recognitionAvailable(context))
    }
}
