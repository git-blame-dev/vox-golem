package dev.gitblame.voxgolem.android.wear.assistant

import android.os.Looper
import dev.gitblame.voxgolem.assistant.recognition.*
import dev.gitblame.voxgolem.assistant.runtime.*
import dev.gitblame.voxgolem.assistant.speech.*
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
class AssistantManualReplySpeechTest {
    private open class Client : AssistantClient {
        var state = AssistantState(foreground = true, draft = "Synthetic manual draft")
        var sends = 0
        val selections = mutableListOf<SpeechSelection>()
        private val observers = mutableListOf<(AssistantState) -> Unit>()
        override fun snapshot() = state
        override fun subscribe(observer: (AssistantState) -> Unit): AutoCloseable {
            observers += observer; observer(state); return AutoCloseable { observers -= observer }
        }
        fun publish(value: AssistantState) { state = value; observers.toList().forEach { it(value) } }
        override fun resume() {}
        override fun refresh() {}
        override fun background() {}
        override fun stop() {}
        override fun setDraft(text: String) { state = state.copy(draft = text) }
        override fun selectModel(model: String) {}
        override fun setSpeechSelection(selection: SpeechSelection) {
            selections += selection
            publish(state.copy(speechLanguage = selection.language, speechTtsProvider = selection.ttsProvider, speechTtsVoice = selection.ttsVoice))
        }
        override fun newConversation() {}
        override fun selectConversation(id: String) {}
        override fun selectHead(id: String) {}
        override fun send() { sends++ }
    }
    private class QueuedSelectionClient : Client() {
        data class Request(val selection: SpeechSelection, val result: (AssistantState) -> Unit)
        val requests = mutableListOf<Request>()
        var persisted = SpeechSelection()
        override fun setSpeechSelection(selection: SpeechSelection, onSettled: (AssistantState) -> Unit) {
            requests += Request(selection, onSettled)
        }
        fun settle(index: Int, failure: String? = null, mismatch: Boolean = false) {
            val request = requests[index]
            if (failure == null && !mismatch) persisted = request.selection
            val saved = state.copy(speechLanguage = persisted.language, speechTtsProvider = persisted.ttsProvider,
                speechTtsVoice = persisted.ttsVoice, error = failure)
            publish(saved)
            request.result(saved)
        }
    }
    private class Recognition : LocalRecognizerFactory {
        var delaySupport = false
        var route: RecognitionRoute? = null
        val sessions = mutableListOf<Session>()
        override fun available() = true
        override fun create() = Session().also { sessions += it }
        inner class Session : LocalRecognizerSession {
            override val route: RecognitionRoute? get() = this@Recognition.route
            lateinit var support: (Boolean) -> Unit
            lateinit var events: LocalRecognizerEvents
            var starts = 0
            var stops = 0
            var destroyed = false
            var cancelled = false
            var rejectDestroy = false
            override fun checkLanguage(language: String, result: (Boolean) -> Unit) { support = result; if (!delaySupport) result(true) }
            override fun start(language: String, events: LocalRecognizerEvents) { this.events = events; starts++ }
            override fun stop() { stops++ }
            override fun cancel() { cancelled = true }
            override fun destroy() { if (rejectDestroy) error("Synthetic cleanup failure"); destroyed = true }
        }
    }
    private class Generation : EnginePort {
        val prompts = mutableListOf<String>()
        val callbacks = mutableListOf<(GenerationResult) -> Unit>()
        var stops = 0
        override fun setDraft(text: String) {}
        override fun sendFinal(text: String, onTerminal: (GenerationResult) -> Unit) {
            prompts += text; callbacks += onTerminal
        }
        override fun stop() { stops++ }
    }
    private class Cue : ListeningCue {
        var plays = 0
        var ends = 0
        var stops = 0
        var closed = false
        var active: String? = null
        var beforeEnd: (() -> Unit)? = null
        override fun play() { check(!closed); plays++; active = "start" }
        override fun finish() { check(!closed); beforeEnd?.invoke(); ends++; active = "end" }
        override fun stop() { stops++; active = null }
        override fun close() { closed = true; active = null }
    }
    private class Tts : LocalTtsFactory {
        val spoken = mutableListOf<String>()
        val ready = mutableListOf<(Boolean) -> Unit>()
        var beforeSpeak: (() -> Unit)? = null
        override fun providers(): List<String?> = listOf(null, "synthetic.tts")
        override fun create(onReady: (Boolean) -> Unit): LocalTtsSession {
            ready += onReady
            return object : LocalTtsSession {
                override fun voices() = listOf(LocalVoice("synthetic-local", Locale.US, false, true))
                override fun speak(text: String, voice: LocalVoice, onTerminal: (String?) -> Unit): Boolean {
                    beforeSpeak?.invoke(); spoken += text; return true
                }
                override fun stop() {}
                override fun shutdown() {}
            }
        }
    }
    private val client = Client()
    private val recognition = Recognition()
    private val leases = RecognitionResourceLease()
    private val dictation = SharedDictationOwner(recognition, { true }, leases)
    private val tts = Tts()
    private val output = LocalSpeechOutput(tts, object : SpeechAudioFocus {
        override fun acquire(onLoss: () -> Unit) = true
        override fun release() {}
    }, leases = leases)
    private val generation = Generation()
    private var admissionReady = true
    private val cue = Cue()
    private val voice = WearAssistantVoice(client, dictation, output, generation, admissionReady = { admissionReady }, language = "en-US", listeningCue = cue)
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun start(enabled: Boolean = true) { voice.foreground(true); voice.tts(enabled); drain() }
    private fun prepareOptions() { voice.prepareSpeechOptions(); drain() }
    private fun ready() { tts.ready.last()(true); drain() }
    private fun complete(text: String = "Synthetic durable answer") {
        generation.callbacks.last()(GenerationResult.Complete(text)); drain()
    }

    private fun voiceStartupDefersTtsUntilFinal(conversation: Boolean) {
        start()
        assertTrue(voice.snapshot().speech.enabled)
        assertTrue(tts.ready.isEmpty())
        voice.foreground(false); drain()
        voice.foreground(true); voice.tts(true)
        client.publish(client.state.copy(speechLanguage = "en-US")); drain()
        assertTrue(tts.ready.isEmpty())
        if (conversation) voice.conversation("") else voice.ask("")
        drain()
        assertEquals(1, recognition.sessions.single().starts)
        assertTrue(tts.ready.isEmpty())
        voice.prepareSpeechOptions(); drain()
        assertTrue(tts.ready.isEmpty())
        recognition.sessions.single().events.final("Synthetic finalized question"); drain()
        assertEquals(listOf("Synthetic finalized question"), generation.prompts)
        assertEquals(1, tts.ready.size)
        assertTrue(tts.spoken.isEmpty())
        ready(); complete()
        assertEquals(listOf("Synthetic durable answer"), tts.spoken)
        voice.close(); drain()
    }

    @Test fun askStartupAndResumeKeepRepliesEnabledWithoutPreparingTtsBeforeTheFinal() = voiceStartupDefersTtsUntilFinal(false)
    @Test fun talkStartupAndResumeKeepRepliesEnabledWithoutPreparingTtsBeforeTheFinal() = voiceStartupDefersTtsUntilFinal(true)

    @Test fun explicitSpeechOptionsPreparationLoadsSettingsChoicesWithoutEnablingRepliesOrStartingInput() {
        start(false)
        assertTrue(tts.ready.isEmpty())
        assertTrue(voice.snapshot().speechOptions.ttsVoices.isEmpty())
        prepareOptions()
        assertEquals(1, tts.ready.size)
        ready()
        assertTrue(voice.snapshot().speechOptions.ttsProviders.contains("synthetic.tts"))
        assertTrue(voice.snapshot().speechOptions.ttsVoices.any { it.name == "synthetic-local" })
        assertFalse(voice.snapshot().speech.enabled)
        assertTrue(tts.spoken.isEmpty())
        assertTrue(recognition.sessions.isEmpty())
        assertTrue(generation.prompts.isEmpty())
        voice.close(); drain()
    }

    @Test fun askUsesTheSameFinalOnlyLifecycleButIdlesWhileTalkAloneReopensListening() {
        start(false)
        voice.ask("Synthetic retained draft"); drain()
        assertEquals(AssistantVoiceEntryMode.ASK, voice.snapshot().entryMode)
        val first = recognition.sessions.single()
        first.events.partial("Synthetic partial"); drain(); assertTrue(generation.prompts.isEmpty())
        first.events.final("Synthetic question"); first.events.final("Synthetic duplicate"); drain()
        assertEquals(listOf("Synthetic retained draft Synthetic question"), generation.prompts)
        complete(); shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(LoopPhase.IDLE, voice.snapshot().loop.phase)
        assertEquals(1, recognition.sessions.size)
        voice.conversation(""); drain()
        assertEquals(AssistantVoiceEntryMode.TALK, voice.snapshot().entryMode)
        recognition.sessions.last().events.final("Synthetic next question"); drain(); complete()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(3, recognition.sessions.size)
        val followup = recognition.sessions.last()
        voice.stop(); followup.events.final("Synthetic cancelled followup"); drain()
        assertEquals(2, generation.prompts.size)
        voice.close(); drain()
    }

    @Test fun selectedHeadChangeWhileAsrListensRetiresConsentWithoutSendingLateFinal() {
        client.state = client.state.copy(selectedHead = "synthetic-admitted-head")
        start(false); voice.ask("Synthetic owned draft"); drain()
        val sdk = recognition.sessions.single()
        sdk.events.partial("Synthetic owned partial"); drain()
        client.publish(client.state.copy(selectedHead = "synthetic-other-head", draft = "Synthetic newer-head draft")); drain()
        assertTrue(sdk.destroyed)
        assertEquals(LoopPhase.ERROR, voice.snapshot().loop.phase)
        sdk.events.final("Synthetic stale final"); drain()
        assertTrue(generation.prompts.isEmpty())
        assertEquals("Synthetic newer-head draft", client.state.draft)
        voice.close(); drain()
    }
    private inner class SelectionFixture(prepareChoices: Boolean = true) : AutoCloseable {
        val client = QueuedSelectionClient()
        private val recognition = Recognition().apply {
            route = RecognitionRoute("synthetic.recognition", "en-US", installedLanguages = listOf("en-US", "en-AU"),
                supportedLanguages = listOf("en-US", "en-AU"))
        }
        private val leases = RecognitionResourceLease()
        private val input = SharedDictationOwner(recognition, { true }, leases)
        val tts = Tts()
        private val speech = LocalSpeechOutput(tts, object : SpeechAudioFocus {
            override fun acquire(onLoss: () -> Unit) = true
            override fun release() {}
        }, leases = leases)
        val voice = WearAssistantVoice(client, input, speech, Generation(), admissionReady = { true }, language = "en-US")
        init {
            voice.foreground(true); voice.tts(true); drain()
            if (prepareChoices) { voice.prepareSpeechOptions(); drain(); tts.ready.last()(true); drain() }
            voice.dictate(""); drain(); voice.stop(); drain() // Observe supported SDK locales, without a final/Send.
        }
        override fun close() { voice.close(); drain() }
    }

    @Test fun successfulExplicitSettingsSettlementPreparesChoicesButSubmissionAloneDoesNot() {
        val f = SelectionFixture(prepareChoices = false)
        try {
            val results = mutableListOf<Boolean>()
            assertTrue(f.tts.ready.isEmpty())
            assertTrue(f.voice.configureSpeech(SpeechSelection()) { results += it })
            drain()
            assertTrue(f.tts.ready.isEmpty())
            assertTrue(results.isEmpty())
            f.client.settle(0); drain()
            assertEquals(listOf(true), results)
            assertEquals(1, f.tts.ready.size)
            f.tts.ready.single()(true); drain()
            assertTrue(f.voice.snapshot().speechOptions.ttsVoices.any { it.name == "synthetic-local" })
            assertTrue(f.tts.spoken.isEmpty())
        } finally { f.close() }
    }

    @Test fun policyAcknowledgementWaitsForBackendSettlementNotMatchingSnapshotAndFencesRapidCrossfieldEdit() {
        val f = SelectionFixture()
        try {
            val results = mutableListOf<Boolean>()
            val first = SpeechSelection(language = "en-AU")
            assertTrue(f.voice.configureSpeech(first) { assertEquals(Looper.getMainLooper(), Looper.myLooper()); results += it })
            assertTrue(results.isEmpty()); assertEquals(1, f.client.requests.size)
            f.client.publish(f.client.state.copy(speechLanguage = first.language))
            f.voice.foreground(true); drain()
            assertEquals(first, f.voice.snapshot().speechSelection)
            assertTrue(results.isEmpty())
            val staleSecondCandidate = SpeechSelection(ttsProvider = "synthetic.tts")
            assertFalse(f.voice.configureSpeech(staleSecondCandidate) { results += it })
            assertFalse(f.voice.configureSpeech(staleSecondCandidate))
            assertEquals(1, f.client.requests.size); assertTrue(results.isEmpty())
            Thread { f.client.settle(0) }.apply { start(); join() }
            assertTrue(results.isEmpty()); drain()
            assertEquals(listOf(true), results)
            f.client.requests[0].result(f.client.state); f.client.publish(f.client.state.copy(toolStatus = "Searching")); drain()
            assertEquals(listOf(true), results)
            val next = f.voice.snapshot().speechSelection.copy(ttsProvider = "synthetic.tts")
            assertTrue(f.voice.configureSpeech(next) { results += it })
            f.client.requests[0].result(f.client.state); drain()
            assertEquals(listOf(true), results)
            assertFalse(f.voice.configureSpeech(SpeechSelection()) { results += it })
            f.client.settle(1); drain()
            assertEquals(listOf(true, true), results)
            assertEquals("en-AU", f.client.persisted.language); assertEquals("synthetic.tts", f.client.persisted.ttsProvider)
        } finally { f.close() }
    }

    @Test fun storageFailureOrMismatchedCommittedStateReportsOneFalseAndClearsThePendingFence() {
        for (mismatch in listOf(false, true)) {
            val f = SelectionFixture()
            try {
                val results = mutableListOf<Boolean>()
                val wanted = SpeechSelection(language = "en-AU")
                assertTrue(f.voice.configureSpeech(wanted) { results += it })
                f.client.publish(f.client.state.copy(speechLanguage = wanted.language)); drain()
                assertTrue(results.isEmpty())
                f.client.settle(0, failure = if (mismatch) null else "storage_error", mismatch = mismatch)
                assertTrue(results.isEmpty()); drain()
                assertEquals(listOf(false), results)
                f.client.requests[0].result(f.client.state); drain()
                assertEquals(listOf(false), results)
                assertTrue(f.voice.configureSpeech(SpeechSelection()) { results += it })
                f.client.settle(1); drain()
                assertEquals(listOf(false, true), results)
            } finally { f.close() }
        }
    }

    @Test fun closeDropsPendingAndAlreadyPostedSaveConsumersWhileThrowingConsumerCannotRetireController() {
        for (posted in listOf(false, true)) {
            val f = SelectionFixture()
            var deliveries = 0
            assertTrue(f.voice.configureSpeech(SpeechSelection(language = "en-AU")) { deliveries++ })
            if (posted) f.client.settle(0)
            f.voice.close()
            if (!posted) f.client.settle(0)
            f.client.requests[0].result(f.client.state); drain()
            assertEquals(0, deliveries)
            assertEquals("en-AU", f.client.persisted.language)
            f.close()
        }
        val f = SelectionFixture()
        try {
            var deliveries = 0
            assertTrue(f.voice.configureSpeech(SpeechSelection(language = "en-AU")) { deliveries++; error("Synthetic Settings consumer failure") })
            f.client.settle(0); drain()
            assertEquals(1, deliveries)
            assertTrue(f.voice.configureSpeech(SpeechSelection()) { deliveries++ })
            f.client.settle(1); drain(); assertEquals(2, deliveries)
        } finally { f.close() }
    }

    @Test fun unsupportedClientAndLocalRejectionDoNotInventSaveCallbacksAndLegacySetterStillWorks() {
        start(); prepareOptions(); ready()
        var deliveries = 0
        assertThrows(UnsupportedOperationException::class.java) { client.setSpeechSelection(SpeechSelection()) { deliveries++ } }
        assertFalse(voice.configureSpeech(SpeechSelection()) { deliveries++ })
        assertFalse(voice.configureSpeech(SpeechSelection(ttsProvider = "synthetic.missing")) { deliveries++ })
        drain(); assertEquals(0, deliveries)
        assertTrue(voice.configureSpeech(SpeechSelection(ttsVoice = "synthetic-local")))
        assertEquals("synthetic-local", client.selections.single().ttsVoice)
        voice.close(); drain()
    }
    @Test fun stoppingOrBackgroundingSpeechCannotInventAnEarlyPolicyAcknowledgement() {
        val f = SelectionFixture()
        try {
            val results = mutableListOf<Boolean>()
            assertTrue(f.voice.configureSpeech(SpeechSelection(language = "en-AU")) { results += it })
            f.voice.stop(); f.voice.foreground(false); drain()
            assertTrue(results.isEmpty())
            f.client.settle(0); drain()
            assertEquals(listOf(true), results)
            assertEquals("en-AU", f.client.persisted.language)
        } finally { f.close() }
    }
    @Test fun settingsSettersExposeOnlyObservedChoicesAndDelegateAcceptedSelectionWithoutInventingModels() {
        start(); prepareOptions(); ready()
        val options = voice.snapshot().speechOptions
        assertTrue(options.ttsVoices.any { it.name == "synthetic-local" })
        assertFalse(options.json().getBoolean("modelSelectionAvailable"))
        assertTrue(options.json().isNull("modelIdentifiers"))
        assertTrue(voice.speechTtsVoice("synthetic-local")); drain()
        assertEquals("synthetic-local", client.selections.single().ttsVoice)
        assertFalse(voice.speechTtsProvider("synthetic-missing-provider"))
        assertFalse(voice.speechLanguage("en--AU"))
        assertEquals(1, client.selections.size)
        voice.close(); drain()
    }

    @Test fun listeningCueWaitsForSdkReadinessAndPlaysOncePerUtteranceEvenWithRepliesDisabled() {
        recognition.delaySupport = true; start(false); voice.dictate(""); drain()
        val first = recognition.sessions.single()
        assertEquals(DictationPhase.CHECKING, voice.snapshot().dictation.phase)
        assertEquals(0, cue.plays)
        first.support(true); drain()
        assertEquals(DictationPhase.LISTENING, voice.snapshot().dictation.phase)
        assertEquals(0, cue.plays)
        first.events.ready(); first.events.ready(); first.events.partial("Synthetic partial"); drain()
        assertEquals(1, cue.plays)
        voice.endUtterance(); first.events.ready(); drain()
        assertEquals(1, cue.plays)
        assertEquals(0, cue.ends); assertNull(cue.active)
        first.events.final("Synthetic final draft"); drain()
        assertEquals(1, cue.ends)
        recognition.delaySupport = false; voice.dictate(client.snapshot().draft); drain()
        recognition.sessions.last().events.ready(); drain()
        assertEquals(2, cue.plays)
        assertTrue(generation.prompts.isEmpty()); assertTrue(tts.spoken.isEmpty())
        voice.close(); drain(); assertTrue(cue.closed)
    }

    @Test fun listeningCueCancelsOnStopBackgroundAndCloseWithoutPlayingQueuedReadiness() {
        start(false); voice.conversation(""); drain()
        val first = recognition.sessions.single()
        first.events.ready(); voice.stop(); drain()
        assertEquals(0, cue.plays)
        voice.conversation(""); drain()
        recognition.sessions.last().events.ready(); drain()
        assertEquals(1, cue.plays)
        val stops = cue.stops
        voice.foreground(false)
        assertTrue(cue.stops > stops)
        first.events.ready(); drain()
        assertEquals(1, cue.plays)
        voice.foreground(true); voice.dictate(""); drain()
        val latest = recognition.sessions.last()
        latest.events.ready(); voice.close(); drain()
        assertEquals(1, cue.plays); assertTrue(cue.closed)
        latest.events.ready(); drain(); assertEquals(1, cue.plays)
        assertEquals(0, cue.ends)
    }

    @Test fun automaticConversationFollowupGetsItsOwnCueOnlyWhenReady() {
        start(false); voice.conversation(""); drain()
        recognition.sessions.single().events.ready(); drain()
        assertEquals(1, cue.plays)
        recognition.sessions.single().events.final("Synthetic loop prompt"); drain()
        assertEquals(1, cue.ends)
        recognition.delaySupport = true
        complete()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMillis(300))
        assertEquals(2, recognition.sessions.size)
        assertEquals(1, cue.plays)
        assertEquals(DictationPhase.CHECKING, voice.snapshot().dictation.phase)
        assertNull(cue.active)
        recognition.sessions.last().support(true); drain()
        recognition.sessions.last().events.ready(); drain()
        assertEquals(2, cue.plays)
        voice.close(); drain()
    }

    private fun finishBeforeQueuedReadiness(conversation: Boolean) {
        start(false)
        if (conversation) voice.conversation("") else voice.dictate("")
        drain()
        val session = recognition.sessions.single()
        session.events.ready()
        voice.endUtterance()
        assertEquals(0, cue.ends)
        drain()
        assertEquals(0, cue.plays)
        assertEquals(DictationPhase.FINISHING, voice.snapshot().dictation.phase)
        assertEquals(0, cue.ends)
        cue.beforeEnd = { assertTrue(session.cancelled); assertTrue(session.destroyed); assertNull(leases.current()) }
        session.events.final("Synthetic authoritative final"); drain()
        if (conversation) assertEquals(listOf("Synthetic authoritative final"), generation.prompts)
        else {
            assertEquals("Synthetic authoritative final", client.snapshot().draft)
            assertTrue(generation.prompts.isEmpty())
        }
        assertEquals(0, cue.plays)
        assertEquals(1, cue.ends)
        voice.close(); drain()
    }

    @Test fun ordinaryFinishSuppressesAlreadyQueuedReadyCueButPreservesFinal() = finishBeforeQueuedReadiness(false)
    @Test fun conversationFinishSuppressesAlreadyQueuedReadyCueButPreservesFinal() = finishBeforeQueuedReadiness(true)
    @Test fun replySpeechStopsSuccessfulEndFeedbackBeforePlaybackWithoutDelayingLoopGeneration() {
        start(); voice.conversation(""); drain()
        val sdk = recognition.sessions.single()
        sdk.events.ready(); drain(); voice.endUtterance(); drain()
        assertEquals(0, cue.ends)
        cue.beforeEnd = { assertTrue(sdk.destroyed); assertTrue(sdk.cancelled); assertNull(leases.current()) }
        sdk.events.final("Synthetic loop prompt"); drain()
        assertEquals(listOf("Synthetic loop prompt"), generation.prompts)
        assertEquals("end", cue.active)
        ready()
        tts.beforeSpeak = { assertNull(cue.active) }
        complete("Synthetic durable spoken answer"); drain()
        assertEquals(listOf("Synthetic durable spoken answer"), tts.spoken)
        assertEquals(1, cue.ends); assertNull(cue.active)
        voice.close(); drain()
    }
    @Test fun noMatchAndCleanupFailureNeverPlayEndOrSubmitUnconfirmedSpeech() {
        for (cleanup in listOf(false, true)) {
            start(false); voice.dictate(""); drain()
            val sdk = recognition.sessions.last()
            sdk.events.ready(); sdk.events.partial("Synthetic partial"); drain()
            voice.endUtterance(); drain()
            sdk.rejectDestroy = cleanup
            if (cleanup) sdk.events.final("Synthetic unconfirmed final") else sdk.events.error("recognition_error_7")
            drain()
            assertEquals(0, cue.ends); assertNull(cue.active)
            assertEquals(DictationPhase.ERROR, voice.snapshot().dictation.phase)
            assertTrue(generation.prompts.isEmpty()); assertEquals(0, client.sends)
            sdk.rejectDestroy = false; voice.stop(); drain()
        }
        voice.close(); drain()
    }

    @Test fun recognitionErrorsPreserveOwnedUnconfirmedDraftForReviewWithoutSending() {
        start(false)
        for (failure in listOf("recognition_error_7", "recognition_error_5", "recognition_cleanup_failed")) {
            voice.dictate("Synthetic saved prefix"); drain()
            val sdk = recognition.sessions.last()
            sdk.events.partial("Synthetic partial words"); drain()
            voice.endUtterance(); drain()
            val retained = if (failure == "recognition_cleanup_failed") {
                sdk.rejectDestroy = true
                sdk.events.final("Synthetic unconfirmed final")
                "Synthetic saved prefix Synthetic unconfirmed final"
            } else {
                sdk.events.error(failure)
                "Synthetic saved prefix Synthetic partial words"
            }
            drain()
            assertEquals(DictationPhase.ERROR, voice.snapshot().dictation.phase)
            assertEquals(retained, client.state.draft)
            if (failure == "recognition_error_7") assertNull(recognitionUiError(voice.snapshot().dictation.error))
            assertTrue(generation.prompts.isEmpty()); assertEquals(0, client.sends); assertTrue(tts.spoken.isEmpty())
            sdk.rejectDestroy = false
            voice.stop(); voice.foreground(false); drain()
            assertEquals(retained, client.state.draft)
            voice.foreground(true); drain()
        }
        voice.close(); drain()
    }

    @Test fun retryWithFailedRecognizerRetirementPreservesTheNewDraftAndConversation() {
        start(false); voice.dictate("Synthetic old prefix"); drain()
        val sdk = recognition.sessions.single()
        sdk.rejectDestroy = true
        try {
            sdk.events.partial("Synthetic old partial"); sdk.events.error("recognition_error_5"); drain()
            assertEquals(DictationPhase.ERROR, voice.snapshot().dictation.phase)
            for (conversation in listOf(client.state.conversation, "synthetic-new-conversation")) {
                client.publish(client.state.copy(conversation = conversation, draft = "Synthetic newer edited draft"))
                voice.dictate(client.state.draft); drain()
                assertEquals(DictationPhase.ERROR, voice.snapshot().dictation.phase)
                assertEquals("recognition_cleanup_failed", voice.snapshot().dictation.error)
                assertEquals("Synthetic newer edited draft", client.state.draft)
                assertEquals("Synthetic newer edited draft", voice.snapshot().dictation.draft)
                assertEquals(1, recognition.sessions.size)
                assertTrue(generation.prompts.isEmpty()); assertEquals(0, client.sends)
            }
        } finally {
            sdk.rejectDestroy = false; voice.close(); drain()
        }
    }

    @Test fun failedDictationFromRetiredConversationCannotOverwriteCurrentDraftOnErrorOrTeardown() {
        start(false); voice.dictate("Synthetic old prefix"); drain()
        val sdk = recognition.sessions.single()
        sdk.events.partial("Synthetic old partial"); drain()
        client.publish(client.state.copy(conversation = "synthetic-new-conversation", draft = "Synthetic new draft"))
        sdk.events.error("recognition_error_7"); drain()
        assertEquals("Synthetic new draft", client.state.draft)
        voice.stop(); voice.foreground(false); voice.close(); drain()
        assertEquals("Synthetic new draft", client.state.draft)
        assertTrue(generation.prompts.isEmpty()); assertEquals(0, client.sends)
    }

    private fun finishDuringChecking(conversation: Boolean, supportAlreadyQueued: Boolean = false) {
        recognition.delaySupport = true; start(false)
        if (conversation) voice.conversation("") else voice.dictate("Synthetic retained draft")
        drain()
        val session = recognition.sessions.single()
        assertEquals(DictationPhase.CHECKING, voice.snapshot().dictation.phase)
        assertTrue(voice.snapshot().listening)
        if (supportAlreadyQueued) session.support(true)
        voice.endUtterance()
        session.support(true); drain()
        assertEquals(0, session.starts); assertTrue(session.destroyed)
        assertEquals(DictationPhase.IDLE, voice.snapshot().dictation.phase)
        assertEquals(LoopPhase.IDLE, voice.snapshot().loop.phase)
        assertFalse(voice.snapshot().active); assertNull(leases.current())
        assertTrue(generation.prompts.isEmpty()); assertEquals(0, client.sends)
        session.support(true); drain()
        assertEquals(0, session.starts); assertTrue(generation.prompts.isEmpty())
        if (!conversation) assertEquals("Synthetic retained draft", voice.snapshot().dictation.draft)
        voice.close(); drain()
    }

    @Test fun secondMicTapDuringOrdinaryDictationCheckingCancelsLateSupportedAdmission() = finishDuringChecking(false)
    @Test fun secondMicTapDuringConversationCheckingCancelsLateSupportedAdmissionAndAutosend() = finishDuringChecking(true)
    @Test fun secondMicTapFencesSupportAlreadyQueuedDuringConversationChecking() = finishDuringChecking(true, true)

    @Test fun secondMicTapWhileListeningStillFinalizesOrdinaryDraftWithoutSending() {
        start(false); voice.dictate(""); drain()
        val session = recognition.sessions.single()
        voice.endUtterance(); drain()
        assertEquals(1, session.stops); assertFalse(session.destroyed)
        assertEquals(DictationPhase.FINISHING, voice.snapshot().dictation.phase)
        session.events.final("Synthetic finished draft"); drain()
        assertEquals("Synthetic finished draft", client.snapshot().draft)
        assertTrue(generation.prompts.isEmpty()); assertEquals(0, client.sends)
        voice.close(); drain()
    }

    @Test fun manualSendPreparesSilentlyAndSpeaksOnlyOneDurableTerminalNotProjectionOrHistory() {
        start(); assertTrue(voice.send()); drain()
        assertTrue(voice.send())
        assertEquals(listOf("Synthetic manual draft"), generation.prompts)
        assertEquals(0, client.sends)
        assertEquals(1, tts.ready.size)
        ready()
        client.publish(client.state.copy(provisional = "Synthetic stream", viewJson = "{\"records\":[{\"text\":\"Synthetic historical answer\"}]}"))
        drain(); assertTrue(tts.spoken.isEmpty())
        Thread { generation.callbacks.single()(GenerationResult.Complete("Synthetic durable answer")) }.apply { start(); join() }
        drain(); complete("Synthetic duplicate terminal")
        assertEquals(listOf("Synthetic durable answer"), tts.spoken)
        voice.close(); drain()
    }

    @Test fun disabledFailedAndEmptyManualRepliesNeverSpeakOrStartRecognition() {
        start(false); voice.send(); drain(); complete()
        assertTrue(tts.ready.isEmpty())
        voice.tts(true); drain(); assertTrue(tts.ready.isEmpty())
        voice.send(); drain()
        ready()
        generation.callbacks.last()(GenerationResult.Failed("not_saved")); drain()
        voice.send(); drain(); complete("")
        assertTrue(tts.spoken.isEmpty()); assertTrue(recognition.sessions.isEmpty())
        voice.close(); drain()
    }
    @Test fun durableWarningStopsLoopWithoutSpeechOrNextMicrophone() {
        start(); voice.conversation(""); drain()
        recognition.sessions.single().events.final("Synthetic prompt"); drain()
        generation.callbacks.single()(GenerationResult.Failed("saved_local_state_failed")); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertTrue(tts.spoken.isEmpty()); assertEquals(1, recognition.sessions.size)
        assertEquals(LoopPhase.ERROR, voice.snapshot().loop.phase)
        assertEquals("saved_local_state_failed", voice.snapshot().loop.error)
        voice.close(); drain()
    }
    @Test fun finalFromRetiredConversationAndIdleTeardownCannotOverwriteCurrentDraft() {
        start(false); voice.dictate(""); drain()
        val sdk = recognition.sessions.single()
        client.publish(client.state.copy(conversation = "synthetic-other-conversation", draft = "Synthetic newer draft"))
        sdk.events.final("Synthetic retired final"); drain()
        assertEquals("Synthetic newer draft", client.state.draft)
        voice.stop(); voice.foreground(false); voice.close(); drain()
        assertEquals("Synthetic newer draft", client.state.draft)
    }

    @Test fun enablingDuringADisabledSendDoesNotReadItsReplyRetroactively() {
        start(false); voice.send(); voice.tts(true); drain(); complete()
        assertTrue(tts.ready.isEmpty())
        assertTrue(tts.spoken.isEmpty())
        voice.close(); drain()
    }

    @Test fun stopBackgroundDisableAndCloseFenceEvenAlreadyQueuedTerminalCallbacks() {
        for (retire in listOf<(WearAssistantVoice) -> Unit>(
            { it.stop() }, { it.foreground(false) }, { it.tts(false) }, { it.close() },
        )) {
            val localGeneration = Generation()
            val localTts = Tts()
            val localDictation = SharedDictationOwner(Recognition(), { true }, RecognitionResourceLease())
            val localOutput = LocalSpeechOutput(localTts, object : SpeechAudioFocus {
                override fun acquire(onLoss: () -> Unit) = true
                override fun release() {}
            }, leases = RecognitionResourceLease())
            val localVoice = WearAssistantVoice(Client(), localDictation, localOutput, localGeneration,
                admissionReady = { true }, language = "en-US")
            localVoice.foreground(true); localVoice.tts(true); drain()
            localVoice.send(); drain()
            localGeneration.callbacks.single()(GenerationResult.Complete("Synthetic queued reply"))
            retire(localVoice)
            drain()
            localTts.ready.forEach { it(true) }; drain()
            localVoice.foreground(true); localVoice.tts(true); drain()
            localGeneration.callbacks.single()(GenerationResult.Complete("Synthetic late reply")); drain()
            assertTrue(localTts.spoken.isEmpty())
            localVoice.close(); drain()
        }
        voice.close(); drain()
    }

    @Test fun retiredTerminalCannotConsumeANewerManualSendOrSpeakOverIt() {
        start(); voice.send(); drain(); ready()
        val old = generation.callbacks.single()
        val stops = generation.stops
        voice.stop(); drain()
        assertEquals(stops + 1, generation.stops)
        voice.send(); drain()
        old(GenerationResult.Complete("Synthetic stale answer")); drain()
        complete(); ready()
        assertEquals(listOf("Synthetic durable answer"), tts.spoken)
        voice.close(); drain()
    }

    @Test fun manualSendDoesNotNeedMicrophoneAdmissionAndStoppedSpeechInitializationNeverPlays() {
        admissionReady = false
        start(); assertTrue(voice.send()); drain()
        complete()
        assertEquals(SpeechPhase.CHECKING, output.snapshot().phase)
        voice.stop(); drain(); ready()
        assertTrue(tts.spoken.isEmpty()); assertTrue(recognition.sessions.isEmpty())
        voice.close(); drain()
    }

    @Test fun manualDictationOnlyFillsDraftAndConversationLoopReplyIsNotSpokenTwice() {
        start()
        voice.dictate(""); drain()
        recognition.sessions.last().events.final("Synthetic dictated draft"); drain()
        assertEquals("Synthetic dictated draft", client.snapshot().draft)
        assertTrue(generation.prompts.isEmpty()); assertEquals(0, client.sends)
        voice.conversation(""); drain()
        recognition.sessions.last().events.final("Synthetic loop prompt"); drain()
        assertTrue(voice.send()); assertEquals(listOf("Synthetic loop prompt"), generation.prompts)
        complete("Synthetic loop answer"); ready()
        assertEquals(listOf("Synthetic loop answer"), tts.spoken)
        voice.close(); drain()
    }
}
