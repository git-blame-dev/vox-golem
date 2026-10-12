package dev.gitblame.voxgolem.assistant.speech

import android.os.Looper
import android.os.Bundle
import android.content.pm.ApplicationInfo
import android.speech.tts.TextToSpeech
import android.speech.tts.Voice
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Kind
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.shadow.api.Shadow
import org.robolectric.shadows.ShadowTextToSpeech
import org.robolectric.shadows.ShadowLog
import java.util.Locale
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class AndroidLocalTtsFactoryTest {
    @Test @Config(shadows = [RecordingTtsShadow::class]) fun nativeTimingSeparatesInitDispatchAndAttributesOnlyMatchingUtteranceCallbacks() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        ShadowTextToSpeech.addVoice(Voice("synthetic-private-voice", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        var now = 0L
        val events = mutableListOf<AssistantTiming.Event>()
        val root = AssistantTiming.begin(context, Operation.SPEECH, { now }, AssistantTiming.Sink { events += it })
        var ready = 0
        val session = AndroidLocalTtsFactory(context).create(null, root) { ready++ }
        val sdk = Shadow.extract<RecordingTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        now = 10_000_000; sdk.onInitListener.onInit(TextToSpeech.SUCCESS)
        assertEquals(0, ready)
        now = 30_000_000; shadowOf(Looper.getMainLooper()).idle()
        assertEquals(1, ready)
        val initialization = events.filter { it.operation == Operation.TTS_INITIALIZATION }
        assertEquals(10_000_000L, initialization.single { it.stage == Stage.INIT_CALLBACK }.elapsedNanos)
        assertEquals(30_000_000L, initialization.single { it.stage == Stage.INIT_MAIN_DELIVERED }.elapsedNanos)
        var first = 0; var second = 0
        val voice = session.voices().single()
        assertTrue(session.speak("Synthetic private first reply", voice, root) { first++ })
        val firstListener = sdk.utteranceProgressListener
        assertTrue(session.speak("Synthetic private second reply", voice, root) { second++ })
        val listener = sdk.utteranceProgressListener
        firstListener.onStart(sdk.utteranceIds[0])
        listener.onStart(sdk.utteranceIds[0]); listener.onDone(sdk.utteranceIds[0])
        now = 80_000_000
        listener.onStart(sdk.utteranceIds[1]); listener.onStart(sdk.utteranceIds[1])
        now = 100_000_000
        listener.onDone(sdk.utteranceIds[1]); listener.onError(sdk.utteranceIds[1], 8); listener.onStop(sdk.utteranceIds[1], true)
        shadowOf(Looper.getMainLooper()).idle()
        val utterances = events.filter { it.operation == Operation.TTS_UTTERANCE }.groupBy { it.run }.values.toList()
        assertEquals(2, utterances.size)
        assertEquals(Stage.REPLACED, utterances[0].last().stage)
        assertFalse(utterances[0].any { it.stage == Stage.ON_START })
        assertEquals(80_000_000L, utterances[1].single { it.stage == Stage.ON_START }.elapsedNanos)
        assertEquals(Stage.ON_DONE, utterances[1].last().stage)
        assertTrue(utterances.all { span -> span.all { it.parentRun == root.run } && span.count { it.kind == Kind.END } == 1 })
        assertEquals(listOf(TextToSpeech.QUEUE_FLUSH, TextToSpeech.QUEUE_FLUSH), sdk.queueModes)
        assertEquals(0, first); assertEquals(1, second)
        assertFalse(events.toString().contains("synthetic-private")); assertFalse(events.toString().contains("Synthetic private"))
        session.shutdown(); root.finish(Stage.SETTLED)
    }

    @Test @Config(shadows = [RecordingTtsShadow::class]) fun rejectedNativeQueueClosesTimingWithoutInventingPlaybackOrCompletion() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        ShadowTextToSpeech.addVoice(Voice("synthetic-local", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        val events = mutableListOf<AssistantTiming.Event>()
        val factory = AndroidLocalTtsFactory(context) { AssistantTiming.begin(context, it, sink = AssistantTiming.Sink { event -> events += event }) }
        val session = factory.create {}
        val sdk = Shadow.extract<RecordingTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        sdk.onInitListener.onInit(TextToSpeech.SUCCESS); shadowOf(Looper.getMainLooper()).idle()
        sdk.queueResult = TextToSpeech.ERROR
        var completions = 0
        assertFalse(session.speak("Synthetic rejected reply", session.voices().single()) { completions++ })
        session.stop(); session.shutdown()
        val utterance = events.filter { it.operation == Operation.TTS_UTTERANCE }
        assertEquals(Stage.QUEUE_REJECTED, utterance.last().stage)
        assertEquals(1, utterance.count { it.kind == Kind.END })
        assertFalse(utterance.any { it.stage == Stage.ON_START || it.stage == Stage.ON_DONE })
        assertEquals(0, completions)
    }

    @Test @Config(shadows = [DelayedConnectionTtsShadow::class]) fun retiredInitializationCannotEmitIntoReplacementTiming() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        val events = mutableListOf<AssistantTiming.Event>()
        val factory = AndroidLocalTtsFactory(context) { AssistantTiming.begin(context, it, sink = AssistantTiming.Sink { event -> events += event }) }
        var oldReady = 0; var newReady = 0
        val old = factory.create { oldReady++ }
        val retired = Shadow.extract<DelayedConnectionTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        val oldRun = events.first().run
        old.shutdown()
        val oldEvents = events.filter { it.run == oldRun }
        val replacement = factory.create { newReady++ }
        val current = Shadow.extract<DelayedConnectionTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        current.connected = true; current.onInitListener.onInit(TextToSpeech.SUCCESS)
        retired.connected = true; retired.onInitListener.onInit(TextToSpeech.SUCCESS)
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(oldEvents, events.filter { it.run == oldRun })
        assertEquals(Stage.CANCELLED, oldEvents.last().stage)
        assertEquals(0, oldReady); assertEquals(1, newReady)
        assertFalse(retired.connected); assertTrue(current.connected)
        replacement.shutdown()
    }

    @Test @Config(shadows = [RecordingTtsShadow::class]) fun nativeLogExcludesReplyVoiceProviderAndRawErrorData() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        ShadowLog.clear()
        ShadowTextToSpeech.addVoice(Voice("synthetic-private-voice", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        val session = AndroidLocalTtsFactory(context).create("synthetic-private-provider") {}
        val sdk = Shadow.extract<RecordingTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        sdk.onInitListener.onInit(TextToSpeech.SUCCESS); shadowOf(Looper.getMainLooper()).idle()
        assertTrue(session.speak("Synthetic private reply", session.voices().single()) {})
        sdk.utteranceProgressListener.onError(sdk.utteranceIds.single(), 123456789)
        shadowOf(Looper.getMainLooper()).idle()
        session.shutdown()
        val logs = ShadowLog.getLogsForTag(AssistantTiming.TAG)
        assertTrue(logs.isNotEmpty())
        assertTrue(logs.any { it.msg.contains("stage=ON_ERROR") })
        assertTrue(logs.none { it.msg.contains("synthetic-private") || it.msg.contains("Synthetic private") || it.msg.contains("123456789") })
    }

    @Test @Config(shadows = [RecordingTtsShadow::class]) fun chunkedReplyTimingEndsOnlyAfterLastChunkAndNeverRevivesOnObsoleteCallbacks() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        ShadowTextToSpeech.addVoice(Voice("synthetic-local", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        val events = mutableListOf<AssistantTiming.Event>()
        val root = AssistantTiming.begin(context, Operation.SPEECH, sink = AssistantTiming.Sink { events += it })
        val session = AndroidLocalTtsFactory(context).create(null, root) {}
        val sdk = Shadow.extract<RecordingTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        sdk.onInitListener.onInit(TextToSpeech.SUCCESS); shadowOf(Looper.getMainLooper()).idle()
        val text = "🧪".repeat(TextToSpeech.getMaxSpeechInputLength())
        val results = mutableListOf<String?>()
        assertTrue(session.speak(text, session.voices().single(), root, results::add))
        val count = AndroidLocalTtsFactory.speechChunks(text, TextToSpeech.getMaxSpeechInputLength()).size
        repeat(count) { index ->
            assertFalse(events.any { it.operation == Operation.TTS_UTTERANCE && it.kind == Kind.END })
            sdk.utteranceProgressListener.onStart(sdk.utteranceIds[index])
            sdk.utteranceProgressListener.onDone(sdk.utteranceIds[index]); shadowOf(Looper.getMainLooper()).idle()
        }
        assertEquals(listOf<String?>(null), results)
        val utterance = events.filter { it.operation == Operation.TTS_UTTERANCE }
        assertEquals(1, utterance.count { it.kind == Kind.END })
        assertEquals(Stage.ON_DONE, utterance.last().stage)
        assertEquals(1, utterance.count { it.stage == Stage.ON_START })
        assertEquals(text, sdk.texts.joinToString(""))
        val settled = events.toList()
        sdk.utteranceProgressListener.onStart(sdk.utteranceIds.first())
        sdk.utteranceProgressListener.onDone(sdk.utteranceIds.last()); shadowOf(Looper.getMainLooper()).idle()
        assertEquals(settled, events)
        session.shutdown(); root.finish(Stage.SETTLED)
    }

    @Test @Config(shadows = [RecordingTtsShadow::class]) fun actualAdapterKeepsFocusAndSharedAdmissionUntilLastChunkThenSettlesOnce() {
        ShadowTextToSpeech.addVoice(Voice("synthetic-local", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        val leases = RecognitionResourceLease()
        var requests = 0; var releases = 0
        val output = LocalSpeechOutput(AndroidLocalTtsFactory(RuntimeEnvironment.getApplication()), object : SpeechAudioFocus {
            override fun acquire(onLoss: () -> Unit): Boolean { requests++; return true }
            override fun release() { releases++ }
        }, leases = leases)
        val text = "🧪".repeat(TextToSpeech.getMaxSpeechInputLength())
        val results = mutableListOf<String?>()
        output.setEnabled(true); output.setForeground(true); output.speak(text, "en-US", results::add)
        shadowOf(Looper.getMainLooper()).idle()
        val sdk = Shadow.extract<RecordingTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        sdk.onInitListener.onInit(TextToSpeech.SUCCESS); shadowOf(Looper.getMainLooper()).idle()
        val count = AndroidLocalTtsFactory.speechChunks(text, TextToSpeech.getMaxSpeechInputLength()).size
        repeat(count) { index ->
            assertNotNull(leases.current()); assertEquals(0, releases); assertTrue(results.isEmpty())
            assertEquals(SpeechPhase.SPEAKING, output.snapshot().phase)
            sdk.utteranceProgressListener.onDone(sdk.utteranceIds[index]); shadowOf(Looper.getMainLooper()).idle()
        }
        assertEquals(text, sdk.texts.joinToString("")); assertEquals(1, requests); assertEquals(1, releases)
        assertNull(leases.current()); assertEquals(listOf<String?>(null), results)
        sdk.utteranceProgressListener.onDone(sdk.utteranceIds.last()); shadowOf(Looper.getMainLooper()).idle()
        assertEquals(1, results.size)
        output.close(); shadowOf(Looper.getMainLooper()).idle()
    }
    @Test @Config(shadows = [RecordingTtsShadow::class]) fun longUnicodeAnswerUsesSequentialBoundedChunksAndOneTerminal() {
        ShadowTextToSpeech.addVoice(Voice("synthetic-local", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        val session = AndroidLocalTtsFactory(RuntimeEnvironment.getApplication()).create {}
        val sdk = Shadow.extract<RecordingTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        val text = "🧪Synthetic e\u0301 reply. ".repeat(900)
        val results = mutableListOf<String?>()
        assertTrue(session.speak(text, session.voices().single(), results::add))
        assertEquals(1, sdk.texts.size)
        val listener = sdk.utteranceProgressListener
        repeat(AndroidLocalTtsFactory.speechChunks(text, TextToSpeech.getMaxSpeechInputLength()).size) {
            val id = sdk.utteranceIds.last()
            listener.onDone(id); listener.onDone(id)
            shadowOf(Looper.getMainLooper()).idle()
        }
        assertEquals(listOf<String?>(null), results)
        assertEquals(text, sdk.texts.joinToString(""))
        assertTrue(sdk.texts.size > 1)
        assertTrue(sdk.texts.all { it.length < TextToSpeech.getMaxSpeechInputLength() && !it.last().isHighSurrogate() && !it.first().isLowSurrogate() })
        assertEquals(sdk.utteranceIds.size, sdk.utteranceIds.toSet().size)
        listener.onError(sdk.utteranceIds.last(), 8); listener.onStop(sdk.utteranceIds.last(), true)
        shadowOf(Looper.getMainLooper()).idle(); assertEquals(1, results.size)
        session.shutdown()
    }
    @Test @Config(shadows = [RecordingTtsShadow::class]) fun stopAndChunkAdmissionFailureFenceQueuedDoneAndLateCallbacks() {
        ShadowTextToSpeech.addVoice(Voice("synthetic-local", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        for (stop in listOf(true, false)) {
            val session = AndroidLocalTtsFactory(RuntimeEnvironment.getApplication()).create {}
            val sdk = Shadow.extract<RecordingTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
            val results = mutableListOf<String?>()
            assertTrue(session.speak("🧪".repeat(TextToSpeech.getMaxSpeechInputLength()), session.voices().single(), results::add))
            val listener = sdk.utteranceProgressListener
            val id = sdk.utteranceIds.single()
            listener.onDone(id)
            if (stop) session.stop() else sdk.failAt = 2
            shadowOf(Looper.getMainLooper()).idle()
            if (stop) { assertEquals(1, sdk.texts.size); assertTrue(results.isEmpty()) }
            else { assertEquals(2, sdk.texts.size); assertEquals(listOf("speech_failed"), results) }
            listener.onDone(id); listener.onError(id, 8); listener.onStop(id, true)
            shadowOf(Looper.getMainLooper()).idle()
            assertEquals(if (stop) 0 else 1, results.size)
            session.shutdown()
        }
    }
    @Test @Config(shadows = [DelayedConnectionTtsShadow::class]) fun lateInitializationAfterWarmPreparationRetirementCannotReviveOrDisconnectTheReplacement() {
        ShadowTextToSpeech.addVoice(Voice("synthetic-local", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        val factory = AndroidLocalTtsFactory(RuntimeEnvironment.getApplication())
        val retirements: List<Pair<String, (LocalSpeechOutput) -> Unit>> = listOf(
            "stop" to { output: LocalSpeechOutput -> output.stop() },
            "disable" to { output: LocalSpeechOutput -> output.setEnabled(false) },
            "background" to { output: LocalSpeechOutput -> output.setForeground(false) },
            "close" to { output: LocalSpeechOutput -> output.close() },
            "initialization timeout" to { _: LocalSpeechOutput -> shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10)) },
        )
        for ((reason, retire) in retirements) {
            var focusRequests = 0
            val focus = object : SpeechAudioFocus {
                override fun acquire(onLoss: () -> Unit): Boolean { focusRequests++; return true }
                override fun release() {}
            }
            fun prepare() = LocalSpeechOutput(factory, focus, leases = RecognitionResourceLease()).also {
                it.setForeground(true); it.setEnabled(true); it.prepare("en-US")
                shadowOf(Looper.getMainLooper()).idle()
            }
            val old = prepare()
            val retired = Shadow.extract<DelayedConnectionTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
            retire(old); shadowOf(Looper.getMainLooper()).idle()
            assertEquals(reason, 1, retired.shutdownRequests)
            val retiredState = old.snapshot()
            val replacement = prepare()
            val current = Shadow.extract<DelayedConnectionTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
            current.connected = true; current.onInitListener.onInit(TextToSpeech.SUCCESS)
            shadowOf(Looper.getMainLooper()).idle()
            assertTrue(reason, replacement.snapshot().localVoiceAvailable)
            retired.connected = true; retired.onInitListener.onInit(TextToSpeech.SUCCESS)
            shadowOf(Looper.getMainLooper()).idle()
            assertFalse(reason, retired.connected)
            assertEquals(reason, 2, retired.shutdownRequests)
            assertEquals(reason, retiredState, old.snapshot())
            assertEquals(reason, SpeechPhase.IDLE, replacement.snapshot().phase)
            assertTrue(reason, current.connected); assertEquals(reason, 0, current.shutdownRequests)
            assertEquals(reason, 0, focusRequests)
            old.close(); replacement.close(); shadowOf(Looper.getMainLooper()).idle()
        }
    }
    @Test @Config(shadows = [DelayedConnectionTtsShadow::class]) fun queuedInitializationAndLateFailureBothDisconnectRetiredClientsWithoutReadyCallbacks() {
        val factory = AndroidLocalTtsFactory(RuntimeEnvironment.getApplication())
        for (status in listOf(TextToSpeech.SUCCESS, TextToSpeech.ERROR)) {
            val ready = mutableListOf<Boolean>()
            val session = factory.create(ready::add)
            val sdk = Shadow.extract<DelayedConnectionTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
            sdk.connected = true; sdk.onInitListener.onInit(status)
            session.shutdown()
            shadowOf(Looper.getMainLooper()).idle()
            assertFalse(sdk.connected); assertTrue(ready.isEmpty())
            sdk.connected = true; sdk.onInitListener.onInit(status)
            shadowOf(Looper.getMainLooper()).idle()
            assertFalse(sdk.connected); assertTrue(ready.isEmpty())
            assertEquals(3, sdk.shutdownRequests)
        }
    }
    @Test @Config(shadows = [DelayedConnectionTtsShadow::class]) fun lateInitializationDisconnectsTheRetiredClientWithoutTouchingItsReplacement() {
        val factory = AndroidLocalTtsFactory(RuntimeEnvironment.getApplication())
        val oldReady = mutableListOf<Boolean>()
        val old = factory.create(oldReady::add)
        val retired = Shadow.extract<DelayedConnectionTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        old.shutdown()
        assertEquals(1, retired.shutdownRequests)
        val newReady = mutableListOf<Boolean>()
        val replacement = factory.create(newReady::add)
        val current = Shadow.extract<DelayedConnectionTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        current.connected = true; current.onInitListener.onInit(TextToSpeech.SUCCESS)
        shadowOf(Looper.getMainLooper()).idle()
        retired.connected = true; retired.onInitListener.onInit(TextToSpeech.SUCCESS)
        shadowOf(Looper.getMainLooper()).idle()
        assertFalse(retired.connected)
        assertEquals(2, retired.shutdownRequests)
        assertTrue(oldReady.isEmpty()); assertEquals(listOf(true), newReady)
        assertTrue(current.connected); assertEquals(0, current.shutdownRequests)
        replacement.shutdown()
    }
    @Test @Config(shadows = [RecordingTtsShadow::class]) fun warmSdkUsesDistinctIdsAndSettlesEachCurrentUtteranceOnce() {
        ShadowTextToSpeech.addVoice(Voice("synthetic-local", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        val session = AndroidLocalTtsFactory(RuntimeEnvironment.getApplication()).create {}
        val sdk = Shadow.extract<RecordingTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        sdk.onInitListener.onInit(TextToSpeech.SUCCESS)
        val selected = session.voices().single()
        var first = 0; var second = 0
        assertTrue(session.speak("Synthetic first", selected) { first++ })
        assertTrue(session.speak("Synthetic second", selected) { second++ })
        assertEquals(2, sdk.utteranceIds.toSet().size)
        val listener = sdk.utteranceProgressListener
        listener.onDone(sdk.utteranceIds[0]); assertEquals(0, second)
        listener.onError(sdk.utteranceIds[1], 8)
        listener.onDone(sdk.utteranceIds[1])
        listener.onStop(sdk.utteranceIds[1], true)
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(1, second); assertEquals(0, first)
        session.shutdown()
    }
    @Suppress("DEPRECATION")
    @Test fun samsungIso3LocaleIsNormalizedWithoutReplacingTheNativeVoice() {
        val native = Voice("synthetic-samsung-local", Locale("eng", "GBR", "default"), Voice.QUALITY_NORMAL,
            Voice.LATENCY_NORMAL, false, emptySet())
        ShadowTextToSpeech.addVoice(native)
        val factory = AndroidLocalTtsFactory(RuntimeEnvironment.getApplication())
        val session = factory.create {}
        val sdk = shadowOf(ShadowTextToSpeech.getLastTextToSpeechInstance())
        sdk.onInitListener.onInit(TextToSpeech.SUCCESS); shadowOf(Looper.getMainLooper()).idle()
        val projected = session.voices().single()
        assertEquals("en", projected.locale.language); assertEquals("GB", projected.locale.country)
        assertTrue(session.speak("Synthetic reply", projected) {})
        assertSame(native, sdk.currentVoice)
        assertEquals("eng", sdk.currentVoice.locale.language)
        session.shutdown()
    }
    @Test fun actualSdkSelectsInstalledLocalVoiceAndNeverNetworkOrMissingVoice() {
        ShadowTextToSpeech.addVoice(Voice("synthetic-network", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, true, emptySet()))
        ShadowTextToSpeech.addVoice(Voice("synthetic-missing", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false,
            setOf(TextToSpeech.Engine.KEY_FEATURE_NOT_INSTALLED)))
        ShadowTextToSpeech.addVoice(Voice("synthetic-local", Locale.US, Voice.QUALITY_NORMAL, Voice.LATENCY_NORMAL, false, emptySet()))
        val leases = RecognitionResourceLease()
        val output = LocalSpeechOutput(AndroidLocalTtsFactory(RuntimeEnvironment.getApplication()), object : SpeechAudioFocus {
            override fun acquire(onLoss: () -> Unit) = true
            override fun release() {}
        }, leases = leases)
        output.setForeground(true); output.setEnabled(true); output.speak("Synthetic final answer", "en-US") {}
        shadowOf(Looper.getMainLooper()).idle()
        val sdk = shadowOf(ShadowTextToSpeech.getLastTextToSpeechInstance())
        sdk.onInitListener.onInit(TextToSpeech.SUCCESS); shadowOf(Looper.getMainLooper()).idle()
        assertEquals("synthetic-local", sdk.currentVoice.name)
        assertEquals("Synthetic final answer", sdk.lastSpokenText)
        output.close(); shadowOf(Looper.getMainLooper()).idle()
        assertTrue(sdk.isShutdown); assertNull(leases.current())
    }
}

@Implements(TextToSpeech::class)
class RecordingTtsShadow : ShadowTextToSpeech() {
    val utteranceIds = mutableListOf<String>()
    val queueModes = mutableListOf<Int>()
    var queueResult = TextToSpeech.SUCCESS
    val texts = mutableListOf<String>()
    var failAt = Int.MAX_VALUE
    @Implementation override fun speak(text: CharSequence?, queueMode: Int, params: Bundle?, utteranceId: String?): Int {
        utteranceIds += requireNotNull(utteranceId)
        texts += text.toString()
        queueModes += queueMode
        return if (texts.size == failAt) TextToSpeech.ERROR else queueResult
    }
}

@Implements(TextToSpeech::class)
class DelayedConnectionTtsShadow : ShadowTextToSpeech() {
    var connected = false
    var shutdownRequests = 0
    @Implementation override fun shutdown() {
        shutdownRequests++
        if (connected) { connected = false; super.shutdown() }
    }
}
