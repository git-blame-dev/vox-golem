package dev.gitblame.voxgolem.assistant.speech

import android.os.Looper
import dev.gitblame.voxgolem.assistant.recognition.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import java.time.Duration
import java.util.Locale

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class ConversationLoopControllerTest {
    private class Recognition : LocalRecognizerFactory {
        var delaySupport = false
        val sessions = mutableListOf<Session>()
        override fun available() = true
        override fun create() = Session().also { sessions += it }
        inner class Session : LocalRecognizerSession {
            lateinit var support: (Boolean) -> Unit
            lateinit var events: LocalRecognizerEvents
            var starts = 0
            var stops = 0
            var destroyed = false
            override fun checkLanguage(language: String, result: (Boolean) -> Unit) { support = result; if (!delaySupport) result(true) }
            override fun start(language: String, events: LocalRecognizerEvents) { this.events = events; starts++ }
            override fun stop() { stops++ }
            override fun cancel() {}
            override fun destroy() { destroyed = true }
        }
    }
    private class Engine : EnginePort {
        val sent = mutableListOf<String>()
        val draftWrites = mutableListOf<String>()
        lateinit var result: (GenerationResult) -> Unit
        var stops = 0
        var currentDraft = ""
        override fun setDraft(text: String) { draftWrites += text; currentDraft = text }
        override fun sendFinal(text: String, onTerminal: (GenerationResult) -> Unit) { sent += text; result = onTerminal }
        override fun stop() { stops++ }
    }
    private class Tts : LocalTtsFactory {
        lateinit var ready: (Boolean) -> Unit
        lateinit var done: (String?) -> Unit
        var spoken = 0
        var creates = 0
        override fun create(onReady: (Boolean) -> Unit): LocalTtsSession { creates++; ready = onReady; return object : LocalTtsSession {
            override fun voices() = listOf(LocalVoice("synthetic-local", Locale.US, false, true))
            override fun speak(text: String, voice: LocalVoice, onTerminal: (String?) -> Unit): Boolean { spoken++; done = onTerminal; return true }
            override fun stop() {}
            override fun shutdown() {}
        } }
    }
    private val recognition = Recognition()
    private val leases = RecognitionResourceLease()
    private val dictation = SharedDictationOwner(recognition, { true }, leases)
    private val tts = Tts()
    private val output = LocalSpeechOutput(tts, object : SpeechAudioFocus {
        override fun acquire(onLoss: () -> Unit) = true
        override fun release() {}
    })
    private val engine = Engine()
    private val loop = ConversationLoopController(dictation, output, engine)
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun start(ttsEnabled: Boolean = false) { loop.setForeground(true); output.setEnabled(ttsEnabled); loop.start("en-US"); drain() }

    @Test fun unchangedRecognitionPublicationsSkipDraftWritesButIdenticalFinalStillWritesAndSendsOnce() {
        recognition.delaySupport = true
        val initialDraft = "Synthetic initial draft"
        engine.currentDraft = initialDraft
        loop.setForeground(true)
        loop.start("en-US", initialDraft, VoiceSessionPolicy.SINGLE_TURN); drain()
        val session = recognition.sessions.single()
        assertEquals(DictationPhase.CHECKING, dictation.snapshot().phase)
        assertEquals(initialDraft, loop.snapshot().draft)
        assertTrue(engine.draftWrites.isEmpty())

        session.support(true); drain()
        assertEquals(DictationPhase.LISTENING, dictation.snapshot().phase)
        assertTrue(engine.draftWrites.isEmpty())
        session.events.ready(); drain()
        assertTrue(dictation.snapshot().readyForSpeech)
        session.events.partial(""); session.events.partial(""); drain()
        assertEquals(initialDraft, loop.snapshot().draft)
        assertTrue(engine.draftWrites.isEmpty())

        session.events.partial("Synthetic partial"); drain()
        val draft = "$initialDraft Synthetic partial"
        assertEquals(draft, loop.snapshot().draft)
        assertEquals(listOf(draft), engine.draftWrites)
        session.events.partial("Synthetic partial"); session.events.partial("Synthetic partial"); drain()
        assertEquals(listOf(draft), engine.draftWrites)
        assertTrue(engine.sent.isEmpty())

        session.events.final("Synthetic partial"); session.events.final("Synthetic duplicate final"); drain()
        assertEquals(listOf(draft, draft), engine.draftWrites)
        assertEquals(listOf(draft), engine.sent)
        assertEquals(LoopPhase.GENERATING, loop.snapshot().phase)
        loop.close(); dictation.close(); output.close(); drain()
    }

    @Test fun askSendsOnlyAuthoritativeFinalOnceAndIdlesWithoutASecondMicrophone() {
        loop.setForeground(true)
        loop.start("en-US", "Synthetic owned draft", VoiceSessionPolicy.SINGLE_TURN); drain()
        val first = recognition.sessions.single()
        first.events.partial("Synthetic partial"); drain()
        assertTrue(engine.sent.isEmpty())
        first.events.final("Synthetic question"); first.events.final("Synthetic duplicate"); drain()
        assertEquals(1, engine.sent.size)
        engine.result(GenerationResult.Complete("Synthetic answer")); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(LoopPhase.IDLE, loop.snapshot().phase)
        assertEquals(1, recognition.sessions.size)
        loop.close(); dictation.close(); output.close(); drain()
    }

    @Test fun askWaitsForPlaybackCompletionThenIdlesAndStopFencesLateFinal() {
        loop.setForeground(true); output.setEnabled(true)
        loop.start("en-US", policy = VoiceSessionPolicy.SINGLE_TURN); drain()
        recognition.sessions.single().events.final("Synthetic question"); drain()
        engine.result(GenerationResult.Complete("Synthetic spoken answer")); drain(); tts.ready(true); drain()
        assertEquals(LoopPhase.SPEAKING, loop.snapshot().phase)
        tts.done(null); drain()
        assertEquals(LoopPhase.IDLE, loop.snapshot().phase)
        loop.start("en-US", policy = VoiceSessionPolicy.SINGLE_TURN); drain()
        val second = recognition.sessions.last()
        second.events.partial("Synthetic retained input"); drain(); loop.stop()
        second.events.final("Synthetic cancelled final"); drain()
        assertEquals(1, engine.sent.size)
        assertEquals("Synthetic retained input", engine.currentDraft)
        loop.close(); dictation.close(); output.close(); drain()
    }

    private fun finishDuringChecking(conversation: Boolean, supportAlreadyQueued: Boolean = false) {
        recognition.delaySupport = true
        if (conversation) start() else {
            loop.setForeground(true); dictation.start("en-US", "Synthetic retained draft"); drain()
        }
        val session = recognition.sessions.single()
        assertEquals(DictationPhase.CHECKING, dictation.snapshot().phase)
        if (supportAlreadyQueued) session.support(true)
        loop.endUtterance()
        session.support(true); drain()
        assertEquals(0, session.starts); assertTrue(session.destroyed)
        assertEquals(DictationPhase.IDLE, dictation.snapshot().phase)
        assertEquals(LoopPhase.IDLE, loop.snapshot().phase)
        assertNull(leases.current()); assertTrue(engine.sent.isEmpty())
        session.support(true); drain()
        assertEquals(0, session.starts); assertTrue(engine.sent.isEmpty())
        if (!conversation) assertEquals("Synthetic retained draft", dictation.snapshot().draft)
        loop.close(); dictation.close(); output.close(); drain()
    }

    @Test fun finishDuringOrdinaryCheckingCancelsAdmissionBeforeLateSupportedCallback() = finishDuringChecking(false)
    @Test fun finishDuringLoopCheckingCancelsAdmissionAndLoopBeforeLateSupportedCallback() = finishDuringChecking(true)
    @Test fun finishDuringCheckingFencesAnAlreadyQueuedSupportedCallback() = finishDuringChecking(true, true)

    @Test fun finishDuringListeningStillRequestsFinalAndLoopSendsOnlyThatFinalOnce() {
        start(); val session = recognition.sessions.single()
        loop.endUtterance(); drain()
        assertEquals(1, session.stops); assertFalse(session.destroyed)
        assertEquals(DictationPhase.FINISHING, dictation.snapshot().phase)
        assertEquals(LoopPhase.LISTENING, loop.snapshot().phase)
        assertTrue(engine.sent.isEmpty())
        session.events.final("Synthetic finished prompt"); session.events.final("Synthetic duplicate final"); drain()
        assertEquals(listOf("Synthetic finished prompt"), engine.sent)
        assertEquals(LoopPhase.GENERATING, loop.snapshot().phase)
        loop.close(); dictation.close(); output.close(); drain()
    }

    @Test fun generationPreparesSpeechWithoutSpeakingProvisionalTextOrOpeningAnotherMicrophone() {
        start(true); recognition.sessions.single().events.final("Synthetic prompt"); drain()
        assertEquals(LoopPhase.GENERATING, loop.snapshot().phase)
        assertEquals(1, tts.creates)
        assertEquals(0, tts.spoken); assertEquals(1, recognition.sessions.size)
        tts.ready(true); drain()
        assertEquals(0, tts.spoken)
        engine.result(GenerationResult.Complete("Synthetic settled answer")); drain()
        assertEquals(1, tts.creates); assertEquals(1, tts.spoken)
        loop.close(); drain()
    }

    @Test fun emptyNoMatchAfterASuccessfullySpokenReplyEndsTheLoopWithoutReportingAFailedTurn() {
        start(true)
        val first = recognition.sessions.single()
        first.events.final("Synthetic successfully recognized prompt"); drain()
        engine.result(GenerationResult.Complete("Synthetic durable spoken answer")); drain()
        tts.ready(true); drain(); tts.done(null); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        val followup = recognition.sessions.last()
        assertNotSame(first, followup)
        followup.events.error("recognition_error_7"); drain()
        assertEquals(LoopPhase.IDLE, loop.snapshot().phase)
        assertNull(loop.snapshot().error)
        assertEquals(DictationPhase.IDLE, dictation.snapshot().phase)
        assertNull(dictation.snapshot().error)
        assertTrue(followup.destroyed); assertNull(leases.current())
        assertEquals(listOf("Synthetic successfully recognized prompt"), engine.sent)
        assertEquals(1, tts.spoken)
        followup.events.final("Synthetic late text"); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(2))
        assertEquals(2, recognition.sessions.size)
        assertEquals(1, engine.sent.size)
        loop.close(); dictation.close(); output.close(); drain()
    }

    @Test fun noMatchBeforeAnyReplyAndNoMatchWithRecognizedFollowupWordsRemainRealFailures() {
        start()
        recognition.sessions.single().events.error("recognition_error_7"); drain()
        assertEquals(LoopPhase.ERROR, loop.snapshot().phase)
        assertEquals("recognition_error_7", loop.snapshot().error)
        loop.start("en-US"); drain()
        recognition.sessions.last().events.final("Synthetic successful prompt"); drain()
        engine.result(GenerationResult.Complete("Synthetic successful answer")); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        val followup = recognition.sessions.last()
        followup.events.partial("Synthetic unconfirmed followup"); drain()
        followup.events.error("recognition_error_7"); drain()
        assertEquals(LoopPhase.ERROR, loop.snapshot().phase)
        assertEquals("recognition_error_7", loop.snapshot().error)
        assertEquals("Synthetic unconfirmed followup", engine.currentDraft)
        assertEquals(1, engine.sent.size)
        loop.close(); dictation.close(); output.close(); drain()
    }

    @Test fun onlyFinalSendsExactlyOnceAndNoListeningDuringGenerationOrSpeech() {
        start(true); val old = recognition.sessions.single()
        old.events.partial("Synthetic partial"); drain(); assertTrue(engine.sent.isEmpty())
        old.events.final("Synthetic prompt"); old.events.final("Synthetic duplicate"); drain()
        assertEquals(listOf("Synthetic prompt"), engine.sent)
        assertEquals(1, recognition.sessions.size)
        engine.result(GenerationResult.Complete("Synthetic complete answer")); drain(); tts.ready(true); drain()
        assertEquals(1, tts.spoken); assertEquals(1, recognition.sessions.size)
        tts.done(null); drain(); shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(2, recognition.sessions.size)
    }
    @Test fun stopDuringGenerationCancelsEveryOwnerAndLateCompletionNeverRestarts() {
        start(); recognition.sessions.single().events.final("Synthetic prompt"); drain()
        loop.stop(); drain(); engine.result(GenerationResult.Complete("Synthetic stale")); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(1, recognition.sessions.size); assertTrue(engine.stops > 0)
        assertEquals(LoopPhase.IDLE, loop.snapshot().phase)
    }
    @Test fun errorsKeepDraftAndBackgroundDoesNotAutomaticallyResume() {
        start(); recognition.sessions.single().events.partial("Synthetic retained"); drain()
        recognition.sessions.single().events.error("synthetic_error"); drain()
        assertEquals("Synthetic retained", engine.currentDraft); assertEquals(LoopPhase.ERROR, loop.snapshot().phase)
        loop.setForeground(false); drain(); loop.setForeground(true); drain()
        assertEquals(1, recognition.sessions.size)
    }
    @Test fun stopBeforeStartAllocationAndPendingRestartAreCancelled() {
        loop.setForeground(true); loop.start("en-US"); loop.stop(); drain()
        assertTrue(recognition.sessions.isEmpty())
        start(); recognition.sessions.single().events.final("Synthetic prompt"); drain()
        engine.result(GenerationResult.Complete("Synthetic answer")); drain(); loop.stop(); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(1, recognition.sessions.size)
    }
    @Test fun previousGenerationCannotCompleteANewerTurnInSameLoop() {
        start(); recognition.sessions.single().events.final("Synthetic first prompt"); drain()
        val previous = engine.result
        previous(GenerationResult.Complete("Synthetic first answer")); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        recognition.sessions.last().events.final("Synthetic second prompt"); drain()
        previous(GenerationResult.Failed("synthetic_stale_error")); drain()
        assertEquals(LoopPhase.GENERATING, loop.snapshot().phase)
        engine.result(GenerationResult.Complete("Synthetic second answer")); drain()
        assertEquals(LoopPhase.BETWEEN_TURNS, loop.snapshot().phase)
        loop.stop(); drain()
    }
    @Test fun stopDuringPlaybackSuppressesDoneAndPendingNextListen() {
        start(true); recognition.sessions.single().events.final("Synthetic prompt"); drain()
        engine.result(GenerationResult.Complete("Synthetic answer")); drain(); tts.ready(true); drain()
        loop.stop(); drain(); tts.done(null); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(1, recognition.sessions.size); assertEquals(LoopPhase.IDLE, loop.snapshot().phase)
    }
    @Test fun stopDuringListeningRetainsPartialAndNeverSendsLateFinal() {
        start(); val session = recognition.sessions.single()
        session.events.partial("Synthetic recoverable"); drain(); loop.stop(); drain()
        session.events.final("Synthetic late final"); drain()
        assertTrue(engine.sent.isEmpty()); assertEquals("Synthetic recoverable", engine.currentDraft)
    }
    @Test fun stopRetainsRecognizerSnapshotBeforeItsQueuedLoopProjectionAndNeverSends() {
        start()
        val session = recognition.sessions.single()
        // Recognition publishes synchronously to this observer before the loop's posted projection.
        val cancellation = dictation.subscribe { value ->
            if (value.phase == DictationPhase.LISTENING && value.draft == "Synthetic just-recognized input") loop.stop()
        }
        session.events.partial("Synthetic just-recognized input"); drain()
        session.events.final("Synthetic stale final"); drain()
        assertEquals("Synthetic just-recognized input", engine.currentDraft)
        assertTrue(engine.sent.isEmpty())
        cancellation.close(); loop.close(); dictation.close(); output.close(); drain()
    }
    @Test fun stopFromGeneratingProjectionPreventsEngineAdmission() {
        loop.subscribe { if (it.phase == LoopPhase.GENERATING) loop.stop() }
        start(); recognition.sessions.single().events.final("Synthetic prompt"); drain()
        assertTrue(engine.sent.isEmpty()); assertEquals(LoopPhase.IDLE, loop.snapshot().phase)
    }
    @Test fun generationTimeoutKeepsPromptAndLateCompletionCannotRestart() {
        start(); recognition.sessions.single().events.final("Synthetic retained prompt"); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(121))
        assertEquals("generation_timeout", loop.snapshot().error)
        assertEquals("Synthetic retained prompt", engine.currentDraft)
        engine.result(GenerationResult.Complete("Synthetic late answer")); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(1, recognition.sessions.size)
    }
}
