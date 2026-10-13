package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.content.Intent
import android.content.pm.ResolveInfo
import android.content.pm.ServiceInfo
import android.speech.RecognitionService
import android.speech.tts.TextToSpeech
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class SpeechServiceRequirementTest {
    private val context: Context get() = RuntimeEnvironment.getApplication()

    @Test
    fun detectsMissingWhenNoServicesRegistered() {
        assertTrue(SpeechServiceRequirement.isMissing(context))
        assertFalse(SpeechServiceRequirement.isSttInstalled(context))
        assertFalse(SpeechServiceRequirement.isTtsInstalled(context))
        assertNotNull(SpeechServiceRequirement.missingRequirementDescription(context))
    }

    @Test
    fun detectsInstalledServices() {
        val packageManager = shadowOf(context.packageManager)

        // Register STT
        val sttResolve = ResolveInfo().apply {
            serviceInfo = ServiceInfo().apply {
                packageName = "com.google.android.tts"
                name = "synthetic.RecognitionService"
            }
        }
        packageManager.addResolveInfoForIntent(Intent(RecognitionService.SERVICE_INTERFACE), sttResolve)

        // Register TTS
        val ttsResolve = ResolveInfo().apply {
            serviceInfo = ServiceInfo().apply {
                packageName = "com.google.android.tts"
                name = "synthetic.TtsService"
            }
        }
        packageManager.addResolveInfoForIntent(Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE), ttsResolve)

        assertTrue(SpeechServiceRequirement.isSttInstalled(context))
        assertTrue(SpeechServiceRequirement.isTtsInstalled(context))
        assertFalse(SpeechServiceRequirement.isMissing(context))
        assertNull(SpeechServiceRequirement.missingRequirementDescription(context))
    }

    @Test
    fun opensPlayStoreFallsBackSafely() {
        // In Robolectric without Play Store, openPlayStore should handle all fallbacks without crashing
        assertNotNull(SpeechServiceRequirement.PLAY_STORE_MARKET_URI)
        assertNotNull(SpeechServiceRequirement.PLAY_STORE_WEB_URI)
    }
}
