package dev.gitblame.voxgolem.android.wear

import android.content.Intent
import android.content.pm.ResolveInfo
import android.content.pm.ServiceInfo
import android.os.Looper
import android.speech.RecognitionService
import android.speech.RecognitionSupport
import android.speech.tts.TextToSpeech
import android.speech.tts.Voice
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLog
import org.robolectric.shadows.ShadowSpeechRecognizer
import org.robolectric.shadows.ShadowTextToSpeech
import java.time.Duration
import java.util.Locale

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class StandaloneCapabilityProbeTest {
    private fun registerRecognizer() {
        val context = RuntimeEnvironment.getApplication()
        val resolution = ResolveInfo().apply {
            serviceInfo = ServiceInfo().apply { packageName = "synthetic.speech"; name = "synthetic.speech.Recognition" }
        }
        shadowOf(context.packageManager).addResolveInfoForIntent(Intent(RecognitionService.SERVICE_INTERFACE), resolution)
    }

    @Test fun supportInspectionNeverStartsListeningOrSpeaksAndClosesItsEngines() {
        val context = RuntimeEnvironment.getApplication()
        registerRecognizer()
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        ShadowTextToSpeech.addVoice(Voice("synthetic-offline", Locale.getDefault(), Voice.QUALITY_NORMAL,
            Voice.LATENCY_NORMAL, false, emptySet()))
        StandaloneCapabilityProbe(context).run()
        val recognizer = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val speech = shadowOf(recognizer)
        assertNull(speech.lastRecognizerIntent)
        speech.triggerSupportResult(RecognitionSupport.Builder().addInstalledOnDeviceLanguage("en-US").build())
        val tts = shadowOf(ShadowTextToSpeech.getLastTextToSpeechInstance())
        tts.onInitListener.onInit(TextToSpeech.SUCCESS)
        shadowOf(Looper.getMainLooper()).idle()
        assertTrue(speech.isDestroyed)
        assertTrue(tts.isShutdown)
        assertTrue(tts.spokenTextList.isEmpty())
        val reports = ShadowLog.getLogsForTag(StandaloneCapabilityProbe.TAG)
        assertTrue(reports.any { it.msg.contains("installedOfflineLanguages") })
        assertTrue(reports.any { it.msg.contains("offlineLocaleVoices") })
    }

    @Test fun silentProvidersAreReleasedAtTheProbeDeadlineWithoutListeningOrSpeech() {
        registerRecognizer()
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        StandaloneCapabilityProbe(RuntimeEnvironment.getApplication()).run()
        val recognizer = shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        val tts = shadowOf(ShadowTextToSpeech.getLastTextToSpeechInstance())
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(6))
        assertTrue(recognizer.isDestroyed)
        assertTrue(tts.isShutdown)
        assertNull(recognizer.lastRecognizerIntent)
        assertTrue(tts.spokenTextList.isEmpty())
        val count = ShadowLog.getLogsForTag(StandaloneCapabilityProbe.TAG).size
        recognizer.triggerSupportError(14)
        tts.onInitListener.onInit(TextToSpeech.SUCCESS)
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(count, ShadowLog.getLogsForTag(StandaloneCapabilityProbe.TAG).size)
    }

    @Test @Config(sdk = [32]) fun olderApiProbeReportsUnsupportedWithoutCreatingEngines() {
        val recognizer = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
        val tts = ShadowTextToSpeech.getLastTextToSpeechInstance()
        StandaloneCapabilityProbe(RuntimeEnvironment.getApplication()).run()
        assertSame(recognizer, ShadowSpeechRecognizer.getLatestSpeechRecognizer())
        assertSame(tts, ShadowTextToSpeech.getLastTextToSpeechInstance())
        assertTrue(ShadowLog.getLogsForTag(StandaloneCapabilityProbe.TAG).any { it.msg.contains("supportedProbeApi") })
    }
}
