package dev.gitblame.voxgolem.assistant.speech

import android.content.Context
import android.os.Bundle
import android.os.Looper
import android.speech.RecognitionSupport
import android.speech.SpeechRecognizer
import android.speech.tts.TextToSpeech
import android.speech.tts.Voice
import dev.gitblame.voxgolem.voice.R
import dev.gitblame.voxgolem.assistant.recognition.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.annotation.LooperMode
import org.robolectric.shadow.api.Shadow
import org.robolectric.shadows.ShadowSpeechRecognizer
import org.robolectric.shadows.ShadowSpeechRecognizerImpl
import org.robolectric.shadows.ShadowTextToSpeech
import org.robolectric.util.ReflectionHelpers
import java.util.Locale

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE, shadows = [CueRetirementRecognizerShadow::class, CueOrderingTtsShadow::class])
@LooperMode(LooperMode.Mode.PAUSED)
class MicrophoneCueAdapterTest {
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun result(text: String) = Bundle().apply {
        putStringArrayList(SpeechRecognizer.RESULTS_RECOGNITION, arrayListOf(text))
    }

    private inner class Fixture : AutoCloseable {
        val context: Context = RuntimeEnvironment.getApplication()
        val leases = RecognitionResourceLease()
        val owner = SharedDictationOwner(AndroidLocalRecognizerFactory(context), { true }, leases)
        val pool = Pool(leases)
        val cue = MicrophoneCueController(AndroidListeningCue(context) { pool })
        val finals = mutableListOf<String>()
        private val subscription = owner.subscribe { value ->
            cue.onDictation(value)
            if (value.phase == DictationPhase.FINAL) finals += value.draft
        }
        val sdk: CueRetirementRecognizerShadow
        init {
            ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
            pool.loaded()
            cue.foreground(true); owner.setForeground(true)
            owner.start("en-AU"); drain()
            // SDK35 returns a proxy; lifecycle operations and callback state live on its delegate.
            val proxy = ShadowSpeechRecognizer.getLatestSpeechRecognizer()
            val delegate = ReflectionHelpers.getField<SpeechRecognizer>(proxy, "mDelegate")
            sdk = Shadow.extract(delegate)
            pool.sdk = sdk
            sdk.triggerSupportResult(RecognitionSupport.Builder().setInstalledOnDeviceLanguages(listOf("en-AU")).build()); drain()
            assertEquals(DictationPhase.LISTENING, owner.snapshot().phase)
        }
        fun finish() { cue.finish(owner.snapshot().utterance); owner.endUtterance() }
        override fun close() {
            sdk.rejectDestroy = false
            cue.close(); subscription.close(); owner.close(); drain()
        }
    }

    private class Pool(private val leases: RecognitionResourceLease) : ListeningCueSoundPool {
        private var listener: ((Int, Int) -> Unit)? = null
        lateinit var sdk: CueRetirementRecognizerShadow
        var starts = 0; var ends = 0; var stream = 0
        val events = mutableListOf<String>()
        override fun setLoadListener(listener: ((Int, Int) -> Unit)?) { this.listener = listener }
        override fun load(context: Context, resource: Int): Int = when (resource) {
            R.raw.start_listening -> 1
            R.raw.stop_listening -> 2
            else -> error("Unexpected cue resource")
        }
        fun loaded() { listener!!(1, 0); listener!!(2, 0) }
        override fun play(sound: Int): Int {
            if (sound == 1) { starts++; events += "start" }
            else {
                assertTrue("End sound requires completed SDK destroy", sdk.destroyCompleted)
                assertTrue("SDK cancel must precede end sound", sdk.timeline.contains("cancel"))
                assertNull("Retirement must release recognition admission before end sound", leases.current())
                sdk.timeline += "end"
                ends++; events += "end"
            }
            stream = sound
            return sound
        }
        override fun stop(stream: Int) { events += "stop:$stream"; this.stream = 0 }
        override fun release() { stream = 0 }
    }

    @Test fun queuedReadyFinishAndFinishingAreSilentUntilSdkCancellationAndDestroyComplete() {
        val f = Fixture()
        try {
            f.sdk.triggerOnReadyForSpeech(Bundle())
            f.finish()
            assertEquals(0, f.pool.starts); assertEquals(0, f.pool.ends)
            assertFalse(f.sdk.timeline.contains("stopListening"))
            drain()
            assertEquals(DictationPhase.FINISHING, f.owner.snapshot().phase)
            assertTrue(f.sdk.timeline.contains("stopListening"))
            assertFalse(f.sdk.destroyCompleted)
            assertEquals(0, f.pool.starts); assertEquals(0, f.pool.ends)
            f.sdk.triggerOnResults(result("Synthetic authoritative final")); drain()
            assertEquals(listOf("Synthetic authoritative final"), f.finals)
            assertEquals(listOf("stopListening", "cancel", "destroy", "destroyed", "end"), f.sdk.timeline)
            assertEquals(1, f.pool.ends)
            f.sdk.triggerOnResults(result("Synthetic stale final")); f.sdk.triggerOnReadyForSpeech(Bundle()); drain()
            f.cue.onDictation(f.owner.snapshot())
            assertEquals(1, f.pool.ends); assertEquals(0, f.pool.starts)
        } finally { f.close() }
    }

    @Test fun finishGestureSynchronouslyStopsAnAlreadyPlayingStartWithoutEarlyEnd() {
        val f = Fixture()
        try {
            f.sdk.triggerOnReadyForSpeech(Bundle()); drain()
            assertEquals(1, f.pool.stream)
            f.finish()
            assertEquals(0, f.pool.stream); assertEquals(listOf("start", "stop:1"), f.pool.events)
            assertTrue(f.sdk.timeline.isEmpty()); assertEquals(0, f.pool.ends)
            drain(); assertEquals(0, f.pool.ends)
            f.sdk.triggerOnResults(result("Synthetic final draft")); drain()
            assertEquals(1, f.pool.ends)
        } finally { f.close() }
    }

    @Test fun recognitionFailureBlankFinalAndFailedSdkRetirementNeverPlaySuccessEnd() {
        for (failure in listOf("no_match", "network", "blank_final", "stop_failure", "cleanup")) {
            val f = Fixture()
            try {
                f.sdk.triggerOnReadyForSpeech(Bundle()); drain()
                f.sdk.triggerOnPartialResults(result("Synthetic retained partial")); drain()
                f.sdk.rejectStop = failure == "stop_failure"
                f.finish(); drain()
                when (failure) {
                    "no_match" -> f.sdk.triggerOnError(SpeechRecognizer.ERROR_NO_MATCH)
                    "network" -> f.sdk.triggerOnError(SpeechRecognizer.ERROR_NETWORK)
                    "blank_final" -> f.sdk.triggerOnResults(result(""))
                    "stop_failure" -> Unit
                    "cleanup" -> { f.sdk.rejectDestroy = true; f.sdk.triggerOnResults(result("Synthetic unconfirmed final")) }
                }
                drain()
                assertEquals(failure, 0, f.pool.ends); assertTrue(failure, f.finals.isEmpty())
                assertEquals(failure, DictationPhase.ERROR, f.owner.snapshot().phase)
                assertTrue(failure, f.owner.snapshot().draft.isNotBlank())
                if (failure == "cleanup") {
                    assertEquals("recognition_cleanup_failed", f.owner.snapshot().error)
                    assertNotNull(f.leases.current())
                } else assertNull(f.leases.current())
                f.sdk.triggerOnResults(result("Synthetic late final")); drain()
                assertEquals(failure, 0, f.pool.ends); assertTrue(f.finals.isEmpty())
            } finally { f.close() }
        }
    }

    @Test fun stopBackgroundAndCloseFenceQueuedFinalWithoutSuccessEnd() {
        for (retirement in listOf("stop", "background", "close")) {
            val f = Fixture()
            try {
                f.sdk.triggerOnReadyForSpeech(Bundle()); drain()
                f.sdk.triggerOnResults(result("Synthetic queued final"))
                when (retirement) {
                    "stop" -> { f.cue.cancel(); f.owner.stop() }
                    "background" -> { f.cue.foreground(false); f.owner.setForeground(false) }
                    "close" -> { f.cue.close(); f.owner.close() }
                }
                drain()
                assertEquals(retirement, 0, f.pool.ends); assertTrue(f.finals.isEmpty())
                assertNull(f.leases.current())
            } finally { f.close() }
        }
    }

    @Test fun nativeTtsSpeakingBoundaryStopsEndBeforeSdkSpeakAndNextMicCheckingAlsoPreemptsEnd() {
        val f = Fixture()
        try {
            f.sdk.triggerOnResults(result("Synthetic final draft")); drain()
            assertEquals(2, f.pool.stream)
            ShadowTextToSpeech.addVoice(Voice("synthetic-au-local", Locale.forLanguageTag("en-AU"),
                Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
            val output = LocalSpeechOutput(AndroidLocalTtsFactory(f.context), object : SpeechAudioFocus {
                override fun acquire(onLoss: () -> Unit) = true
                override fun release() {}
            }, leases = f.leases)
            val subscription = output.subscribe(f.cue::onSpeech)
            try {
                output.setForeground(true); output.setEnabled(true); output.speak("Synthetic spoken answer", "en-AU") {}
                drain()
                val tts = Shadow.extract<CueOrderingTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
                tts.beforeSpeak = { assertEquals(0, f.pool.stream); f.pool.events += "tts_speak" }
                tts.onInitListener.onInit(TextToSpeech.SUCCESS); drain()
                assertEquals(listOf("Synthetic spoken answer"), tts.texts)
                assertTrue(f.pool.events.indexOf("stop:2") < f.pool.events.indexOf("tts_speak"))
                assertEquals(1, f.pool.ends)
            } finally { subscription.close(); output.close(); drain() }
        } finally { f.close() }

        val next = Fixture()
        try {
            next.sdk.triggerOnResults(result("Synthetic first final")); drain()
            assertEquals(2, next.pool.stream)
            next.owner.start("en-AU"); drain()
            assertEquals(DictationPhase.CHECKING, next.owner.snapshot().phase)
            assertEquals(0, next.pool.stream); assertEquals(1, next.pool.ends)
            assertNull(shadowOf(ShadowSpeechRecognizer.getLatestSpeechRecognizer()).lastRecognizerIntent)
        } finally { next.close() }
    }
}

@Implements(className = "android.speech.SpeechRecognizerImpl", isInAndroidSdk = false, minSdk = 35)
class CueRetirementRecognizerShadow : ShadowSpeechRecognizerImpl() {
    val timeline = mutableListOf<String>()
    var rejectDestroy = false
    var rejectStop = false
    var destroyCompleted = false
    @Implementation(methodName = "stopListening") fun recordStopListening() {
        timeline += "stopListening"
        if (rejectStop) error("Synthetic SDK stop failure")
    }
    @Implementation(methodName = "cancel") fun recordCancel() { timeline += "cancel" }
    @Implementation override fun destroy() {
        timeline += "destroy"
        if (rejectDestroy) error("Synthetic SDK cleanup failure")
        super.destroy()
        destroyCompleted = true; timeline += "destroyed"
    }
}

@Implements(TextToSpeech::class)
class CueOrderingTtsShadow : ShadowTextToSpeech() {
    val texts = mutableListOf<String>()
    var beforeSpeak: (() -> Unit)? = null
    @Implementation override fun speak(text: CharSequence?, queueMode: Int, params: Bundle?, utteranceId: String?): Int {
        beforeSpeak?.invoke(); texts += text.toString(); return TextToSpeech.SUCCESS
    }
}
