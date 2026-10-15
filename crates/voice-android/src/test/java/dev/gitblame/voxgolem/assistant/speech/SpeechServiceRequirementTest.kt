package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.content.ContextWrapper
import android.content.ActivityNotFoundException
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
        for (successAt in 0..3) {
            val attempts = mutableListOf<Intent>()
            val launcher = object : ContextWrapper(context) {
                override fun startActivity(intent: Intent) {
                    attempts += intent
                    if (attempts.size - 1 != successAt) throw ActivityNotFoundException("Synthetic unavailable handler")
                }
            }
            assertEquals(successAt < 3, SpeechServiceRequirement.openPlayStore(launcher))
            assertEquals(minOf(successAt + 1, 3), attempts.size)
            assertEquals("com.android.vending", attempts.first().`package`)
            assertEquals(SpeechServiceRequirement.PLAY_STORE_MARKET_URI, attempts.first().data.toString())
            if (attempts.size > 1) {
                assertNull(attempts[1].`package`)
                assertEquals(SpeechServiceRequirement.PLAY_STORE_MARKET_URI, attempts[1].data.toString())
            }
            if (attempts.size > 2) {
                assertEquals(SpeechServiceRequirement.PLAY_STORE_WEB_URI, attempts[2].data.toString())
                assertTrue(attempts[2].hasCategory(Intent.CATEGORY_BROWSABLE))
            }
            assertTrue(attempts.all { it.flags and Intent.FLAG_ACTIVITY_NEW_TASK != 0 })
        }
    }

    @Test
    fun rejectsNonGoogleSpeechServices() {
        val packageManager = shadowOf(context.packageManager)

        val samsungStt = ResolveInfo().apply {
            serviceInfo = ServiceInfo().apply {
                packageName = "com.samsung.SMT"
                name = "com.samsung.SMT.SamsungRecognitionService"
            }
        }
        packageManager.addResolveInfoForIntent(Intent(RecognitionService.SERVICE_INTERFACE), samsungStt)

        val samsungTts = ResolveInfo().apply {
            serviceInfo = ServiceInfo().apply {
                packageName = "com.samsung.SMT"
                name = "com.samsung.SMT.SamsungTtsService"
            }
        }
        packageManager.addResolveInfoForIntent(Intent(TextToSpeech.Engine.INTENT_ACTION_TTS_SERVICE), samsungTts)

        assertFalse(SpeechServiceRequirement.isSttInstalled(context))
        assertFalse(SpeechServiceRequirement.hasRecognitionService(context))
        assertFalse(SpeechServiceRequirement.hasGoogleRecognitionService(context))
        assertFalse(SpeechServiceRequirement.isTtsInstalled(context))
        assertTrue(SpeechServiceRequirement.isMissing(context))
    }

    @Test
    fun opensTtsDataInstallAndFallsBackToPlayStore() {
        for (successAt in 0..1) {
            val attempts = mutableListOf<Intent>()
            val launcher = object : ContextWrapper(context) {
                override fun startActivity(intent: Intent) {
                    attempts += intent
                    if (attempts.size - 1 != successAt) throw ActivityNotFoundException("Synthetic unavailable handler")
                }
            }
            assertTrue(SpeechServiceRequirement.openTtsDataInstall(launcher))
            assertEquals(TextToSpeech.Engine.ACTION_INSTALL_TTS_DATA, attempts.first().action)
            assertEquals(SpeechServiceRequirement.GOOGLE_SPEECH_PACKAGE, attempts.first().`package`)
            assertTrue(attempts.first().flags and Intent.FLAG_ACTIVITY_NEW_TASK != 0)
            if (successAt == 1) {
                assertEquals(2, attempts.size)
                assertEquals("com.android.vending", attempts[1].`package`)
            }
        }
    }
}
