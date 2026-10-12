package dev.gitblame.voxgolem.assistant.phone

import android.os.Looper
import android.os.Bundle
import android.media.AudioManager
import android.speech.tts.TextToSpeech
import android.speech.tts.Voice
import dev.gitblame.voxgolem.assistant.recognition.*
import dev.gitblame.voxgolem.assistant.speech.ListeningCue
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
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
import org.robolectric.shadows.ShadowSpeechRecognizer
import org.robolectric.shadows.ShadowTextToSpeech
import java.util.Locale
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneFinishSessionTest {
    private class Backend : PhoneSpeechBackend {
        val drafts = mutableListOf<String>()
        var conversation: String? = "synthetic-conversation-one"
        var head: String? = null
        private val contextObservers = mutableListOf<() -> Unit>()
        var ttsEnabled = false
        override fun busy() = false
        override fun saveSpeechDraft(text: String) { drafts += text }
        override fun speechConversation() = conversation
        override fun speechHead() = head
        override fun observeSpeechContext(observer: () -> Unit): AutoCloseable {
            contextObservers += observer; return AutoCloseable { contextObservers -= observer }
        }
        fun publishHead(value: String?) { head = value; contextObservers.toList().forEach { it() } }
        override fun saveOwnedSpeechDraft(text: String, conversation: String?) { if (this.conversation == conversation) saveSpeechDraft(text) }
        override fun stopAll() {}
        override fun nativePreferences() = JSONObject().put("tts_enabled", ttsEnabled)
        override var speechStatus: JSONObject? = null
        override var onPreferencesChanged: ((Boolean) -> Unit)? = null
    }
    private class Factory : LocalRecognizerFactory {
        var delaySupport = false
        var archiveExpected = false
        var recordedAudio: RecordedAudioResult? = null
        val sessions = mutableListOf<Session>()
        override fun available() = true
        override fun create() = Session().also { sessions += it }
        inner class Session : LocalRecognizerSession {
            override val archiveExpected: Boolean get() = this@Factory.archiveExpected
            override val recordedAudio: RecordedAudioResult? get() = this@Factory.recordedAudio
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
    private val backend = Backend()
    private val factory = Factory()
    private val leases = RecognitionResourceLease()
    private val cue = object : ListeningCue {
        var starts = 0; var ends = 0; var stops = 0; var active: String? = null
        override fun play() { starts++; active = "start" }
        override fun finish() {
            assertTrue(factory.sessions.last().cancelled); assertTrue(factory.sessions.last().destroyed)
            assertNull(leases.current()); ends++; active = "end"
        }
        override fun stop() { stops++; active = null }
        override fun close() { active = null }
    }
    private val events = mutableListOf<JSONObject>()
    private lateinit var phone: PhoneSpeechSession
    private fun drain() = shadowOf(Looper.getMainLooper()).idle()
    private fun start(loop: Boolean) {
        phone.start("synthetic-session", loop, "Synthetic retained draft", { events += JSONObject(it.toString()) }); drain()
    }
    private fun submits() = events.filter { it.getString("kind") == "submit" }

    private fun enableTts(): PhoneCueTtsShadow {
        val context = RuntimeEnvironment.getApplication()
        ShadowTextToSpeech.addVoice(Voice("synthetic-current-local", Locale.getDefault(), Voice.QUALITY_NORMAL,
            Voice.LATENCY_NORMAL, false, emptySet()))
        shadowOf(context.getSystemService(AudioManager::class.java)).setNextFocusRequestResponse(AudioManager.AUDIOFOCUS_REQUEST_GRANTED)
        backend.ttsEnabled = true; backend.onPreferencesChanged!!.invoke(true); drain()
        return readyTts()
    }
    private fun readyTts(): PhoneCueTtsShadow {
        val tts = Shadow.extract<PhoneCueTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        tts.onInitListener.onInit(TextToSpeech.SUCCESS); drain()
        return tts
    }
    private fun answer(text: String = "Synthetic answer-only speech") = Result.success<Any?>(JSONObject()
        .put("kind", "completed").put("speechText", text)
        .put("turn", JSONObject().put("answer", JSONObject().put("content", JSONObject()
            .put("kind", "text").put("text", "Synthetic canonical answer")))))

    @Before fun prepare() {
        AssistantRuntime.configureCaptureResourceAdmission(true)
        ShadowSpeechRecognizer.setIsOnDeviceRecognitionAvailable(true)
        phone = PhoneSpeechSession(RuntimeEnvironment.getApplication(), backend, SharedDictationOwner(factory, { true }, leases), listeningCue = cue)
        phone.foreground(true); drain()
    }
    @After fun close() { phone.close(); drain(); AssistantRuntime.configureCaptureResourceAdmission(false) }

    @Test fun recordedAskPublishesOnlyTheFinalArtifactReferenceAndKeepsDraftOwnership() {
        factory.archiveExpected = true
        val artifact = RecordedAudioResult("123e4567-e89b-42d3-a456-426614174000", "c".repeat(64), 4)
        phone.start("synthetic-recorded-session", false, "Synthetic retained draft", { events += JSONObject(it.toString()) },
            completionPolicy = "single_turn"); drain()
        val sdk = factory.sessions.single()
        sdk.events.partial("Synthetic partial"); drain()
        assertTrue(submits().isEmpty()); assertTrue(phone.status().isNull("recordedAudio"))
        factory.recordedAudio = artifact
        sdk.events.final("Synthetic spoken words"); sdk.events.final("Synthetic duplicate"); drain()
        val submit = submits().single()
        assertEquals("Synthetic retained draft Synthetic spoken words", submit.getString("text"))
        val audio = submit.getJSONObject("recordedAudio")
        assertEquals(artifact.captureId, audio.getString("captureId")); assertEquals(artifact.sha256, audio.getString("sha256"))
        assertEquals(4L, audio.getLong("frameCount")); assertTrue(phone.admitsVoiceSubmission(submit.getString("requestId")))
        assertTrue(phone.status().getBoolean("archiveExpected"))
        assertEquals(artifact.captureId, phone.status().getJSONObject("recordedAudio").getString("captureId"))
    }

    @Test fun requiredArchiveWithoutSaveEvidenceCannotEmitSubmission() {
        factory.archiveExpected = true
        phone.start("synthetic-recording-failure", false, "", { events += JSONObject(it.toString()) },
            completionPolicy = "single_turn"); drain()
        factory.sessions.single().events.final("Synthetic final without archive"); drain()
        assertTrue(submits().isEmpty())
        assertEquals("recording_save_failed", phone.status().getString("diagnosticError"))
        assertTrue(phone.status().getString("error").contains("Nothing was sent"))
    }

    @Test fun askUsesCurrentOwnedDraftSubmitsOnlyFinalOnceAndNeverRestartsAfterReply() {
        val conversation = backend.conversation
        phone.start("synthetic-ask-session", false, "Synthetic retained draft", { events += JSONObject(it.toString()) }, completionPolicy = "single_turn"); drain()
        assertEquals("ask", phone.status().getString("mode"))
        val sdk = factory.sessions.single()
        sdk.events.partial("Synthetic partial"); drain(); assertTrue(submits().isEmpty())
        sdk.events.final("Synthetic question"); sdk.events.final("Synthetic duplicate"); drain()
        val submit = submits().single()
        assertEquals("Synthetic retained draft Synthetic question", submit.getString("text"))
        assertEquals(conversation, backend.conversation)
        val id = submit.getString("requestId")
        assertTrue(phone.admitsVoiceSubmission(id))
        phone.onRequestResult(id, answer(), phone.onRequestSubmitted(id)); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals("idle", phone.status().getString("loop")); assertFalse(phone.active())
        assertEquals(1, factory.sessions.size)
        assertFalse(phone.admitsVoiceSubmission(id))
    }

    @Test fun onlyTalkRestartsAfterTheReplyAndStopOrContextChangeCannotSubmitLateFinal() {
        phone.start("synthetic-talk-session", true, "", { events += JSONObject(it.toString()) }, completionPolicy = "repeat_after_reply"); drain()
        val first = factory.sessions.single()
        first.events.final("Synthetic first question"); drain()
        val id = submits().single().getString("requestId")
        assertTrue(phone.admitsVoiceSubmission(id))
        phone.onRequestResult(id, answer(), phone.onRequestSubmitted(id)); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(2, factory.sessions.size)
        val second = factory.sessions.last()
        second.events.partial("Synthetic recoverable followup"); drain()
        phone.stop(); second.events.final("Synthetic cancelled followup"); drain()
        assertEquals(1, submits().size)
        assertEquals("Synthetic recoverable followup", backend.drafts.last())
        phone.start("synthetic-owned-ask", false, "", { events += JSONObject(it.toString()) }, completionPolicy = "single_turn"); drain()
        backend.conversation = "synthetic-different-conversation"
        factory.sessions.last().events.final("Synthetic wrong-context final"); drain()
        assertEquals(1, submits().size)
        assertEquals("error", phone.status().getString("loop"))
    }

    @Test fun stopAndChangedContextRetireNativeSubmitConsentBeforeDelayedFrontendIpc() {
        phone.start("synthetic-ask-session", false, "", { events += JSONObject(it.toString()) }, completionPolicy = "single_turn"); drain()
        factory.sessions.single().events.final("Synthetic question"); drain()
        val id = submits().single().getString("requestId")
        assertTrue(phone.admitsVoiceSubmission(id))
        val owner = backend.conversation
        backend.conversation = "synthetic-other-conversation"
        assertFalse(phone.admitsVoiceSubmission(id))
        backend.conversation = owner
        phone.stop()
        assertFalse(phone.admitsVoiceSubmission(id))
        drain(); phone.foreground(false); phone.foreground(true); drain()
        assertFalse(phone.admitsVoiceSubmission(id))
    }

    @Test fun talkCannotRebindItsNextMicrophoneToAnotherConversationAfterAReply() {
        phone.start("synthetic-talk-session", true, "", { events += JSONObject(it.toString()) }, completionPolicy = "repeat_after_reply"); drain()
        factory.sessions.single().events.final("Synthetic first question"); drain()
        val id = submits().single().getString("requestId")
        phone.onRequestResult(id, answer(), phone.onRequestSubmitted(id)); drain()
        backend.conversation = "synthetic-other-conversation"
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(1, factory.sessions.size)
        assertEquals("error", phone.status().getString("loop"))
        assertTrue(phone.status().getString("error").contains("conversation_changed"))
        assertEquals(1, submits().size)
    }

    @Test fun selectedHeadPublicationRetiresAsrAndLateFinalCannotOverwriteOrSubmitInAnotherBranch() {
        backend.head = "synthetic-admitted-head"
        phone.start("synthetic-owned-ask", false, "Synthetic admitted draft", { events += JSONObject(it.toString()) }, completionPolicy = "single_turn"); drain()
        val sdk = factory.sessions.single()
        sdk.events.partial("Synthetic owned partial"); drain()
        backend.drafts.clear()
        backend.publishHead("synthetic-selected-other-head"); drain()
        assertEquals("error", phone.status().getString("loop"))
        assertTrue(sdk.destroyed)
        sdk.events.final("Synthetic stale authoritative final"); drain()
        assertTrue(submits().isEmpty()); assertTrue(backend.drafts.isEmpty())
    }

    @Test fun delayedFrontendSubmitIsRejectedWhenOnlyTheSelectedHeadChanges() {
        backend.head = "synthetic-admitted-head"
        phone.start("synthetic-owned-ask", false, "", { events += JSONObject(it.toString()) }, completionPolicy = "single_turn"); drain()
        factory.sessions.single().events.final("Synthetic owned question"); drain()
        val id = submits().single().getString("requestId")
        assertTrue(phone.admitsVoiceSubmission(id))
        backend.head = "synthetic-other-head"
        assertFalse(phone.admitsVoiceSubmission(id))
    }

    @Test fun talkAdvancesOnlyToTheHeadOfItsAuthoritativeReplyAndRejectsAnotherHeadBeforeFollowup() {
        backend.head = "synthetic-admitted-head"
        phone.start("synthetic-owned-talk", true, "", { events += JSONObject(it.toString()) }, completionPolicy = "repeat_after_reply"); drain()
        factory.sessions.single().events.final("Synthetic owned question"); drain()
        val id = submits().single().getString("requestId")
        val ticket = phone.onRequestSubmitted(id)
        val completed = answer().getOrThrow() as JSONObject
        completed.put("voiceContext", JSONObject().put("conversation", backend.conversation).put("selectedHead", "synthetic-own-reply-head"))
        backend.head = "synthetic-own-reply-head"
        phone.onRequestResult(id, Result.success(completed), ticket); drain()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(1))
        assertEquals(2, factory.sessions.size)
        backend.publishHead("synthetic-different-head"); drain()
        assertEquals("error", phone.status().getString("loop"))
        factory.sessions.last().events.final("Synthetic cancelled followup"); drain()
        assertEquals(1, submits().size)
    }

    private fun finishDuringChecking(loop: Boolean, supportAlreadyQueued: Boolean = false) {
        factory.delaySupport = true; start(loop)
        val sdk = factory.sessions.single()
        assertEquals("checking", phone.status().getString("dictation"))
        if (supportAlreadyQueued) sdk.support(true)
        phone.finish()
        val retiredEvents = events.size
        sdk.support(true); drain()
        assertEquals(0, sdk.starts); assertTrue(sdk.destroyed)
        assertEquals("idle", phone.status().getString("dictation"))
        assertEquals("idle", phone.status().getString("loop"))
        assertFalse(phone.active()); assertNull(leases.current())
        assertTrue(submits().isEmpty()); assertEquals(retiredEvents, events.size)
        sdk.support(true); drain()
        assertEquals(0, sdk.starts); assertTrue(submits().isEmpty())
    }

    @Test fun normalFinishDuringDictationCheckingRetiresSessionAndLateSupportedAdmission() = finishDuringChecking(false)
    @Test fun voiceStatusExposesBoundedChoicesAutomaticSelectionsAndUnknownModels() {
        val status = phone.status()
        assertEquals("", status.getString("speechLanguage"))
        assertEquals("", status.getString("speechTtsProvider"))
        assertEquals("", status.getString("speechTtsVoice"))
        val options = status.getJSONObject("speechOptions")
        assertFalse(options.getBoolean("modelSelectionAvailable"))
        assertTrue(options.isNull("modelIdentifiers"))
        assertTrue(options.isNull("recognitionSupportedLanguages"))
        assertTrue(options.getJSONArray("ttsProviders").length() <= 8)
        assertTrue(options.getJSONArray("ttsVoices").length() <= 1024)
    }
    @Test fun normalFinishDuringLoopCheckingRetiresSessionAndLateSupportedAdmissionWithoutSubmit() = finishDuringChecking(true)
    @Test fun normalFinishFencesAnAlreadyQueuedSupportReply() = finishDuringChecking(true, true)

    @Test fun readinessIsProjectedOnlyAfterSdkReadyAndCachedStatusNeverContainsSessionIdentity() {
        start(false); val sdk = factory.sessions.single()
        fun statusProjection(ready: Boolean) {
            val cached = backend.speechStatus!!
            val emitted = JSONObject(events.last { it.getString("kind") == "voice_status" }.toString())
            assertEquals(ready, phone.status().getBoolean("readyForSpeech"))
            assertEquals(ready, cached.getBoolean("readyForSpeech"))
            assertEquals(ready, emitted.getBoolean("readyForSpeech"))
            assertFalse(cached.has("sessionId"))
            assertEquals("synthetic-session", emitted.getString("sessionId"))
            emitted.remove("sessionId")
            assertEquals(cached.toString(), emitted.toString())
        }
        statusProjection(false)
        sdk.events.ready(); drain(); statusProjection(true)
        phone.finish(); drain(); statusProjection(false)
        sdk.events.ready(); drain(); statusProjection(false)
        sdk.events.final("Synthetic authoritative words"); drain(); statusProjection(false)
        assertTrue(submits().isEmpty())
        phone.stop(); drain()
        assertFalse(phone.status().getBoolean("readyForSpeech"))
        assertFalse(backend.speechStatus!!.has("sessionId"))
        sdk.events.ready(); drain()
        assertFalse(phone.status().getBoolean("readyForSpeech"))
    }

    @Test fun normalFinishWhileListeningStillFinalizesTheDraftWithoutSubmitting() {
        start(false); val sdk = factory.sessions.single()
        phone.finish(); drain()
        assertEquals(1, sdk.stops); assertFalse(sdk.destroyed)
        assertEquals("finishing", phone.status().getString("dictation"))
        assertEquals(0, cue.ends)
        sdk.events.final("Synthetic finished words"); drain()
        assertEquals("Synthetic retained draft Synthetic finished words", backend.drafts.last())
        assertEquals("final", phone.status().getString("dictation")); assertTrue(submits().isEmpty())
        assertEquals(1, cue.ends)
    }
    @Test fun queuedReadyFinishDoesNotPlayEarlyAndRetiredFinalPlaysEndOnceWithoutSend() {
        start(false); val sdk = factory.sessions.single()
        sdk.events.ready(); phone.finish()
        assertEquals(0, cue.starts); assertEquals(0, cue.ends); assertEquals(0, sdk.stops)
        drain()
        assertEquals(1, sdk.stops); assertFalse(sdk.destroyed)
        assertEquals(0, cue.starts); assertEquals(0, cue.ends)
        sdk.events.final("Synthetic authoritative final"); sdk.events.final("Synthetic duplicate final"); drain()
        assertEquals(1, cue.ends); assertTrue(submits().isEmpty())
        assertEquals("Synthetic retained draft Synthetic authoritative final", backend.drafts.last())
    }
    @Test fun failedCleanupNeverPlaysEndOrPromotesFinalToSend() {
        start(true); val sdk = factory.sessions.single()
        sdk.events.ready(); sdk.events.partial("Synthetic partial"); drain()
        phone.finish(); drain(); sdk.rejectDestroy = true
        try {
            sdk.events.final("Synthetic unconfirmed final"); drain()
            assertEquals(0, cue.ends); assertNull(cue.active)
            assertEquals("error", phone.status().getString("dictation")); assertTrue(submits().isEmpty())
        } finally { sdk.rejectDestroy = false }
    }
    @Test @Config(shadows = [PhoneCueTtsShadow::class]) fun nextMicCheckingAndNativeTtsSpeakingBothPreemptSuccessfulEndFeedback() {
        start(false); factory.sessions.single().events.final("Synthetic final"); drain()
        assertEquals("end", cue.active)
        factory.delaySupport = true
        phone.start("synthetic-next-session", false, "Synthetic next draft", { events += it }); drain()
        assertEquals("checking", phone.status().getString("dictation"))
        assertNull(cue.active); assertEquals(0, factory.sessions.last().starts)
        phone.stop(); drain(); factory.delaySupport = false

        val context = RuntimeEnvironment.getApplication()
        ShadowTextToSpeech.addVoice(Voice("synthetic-current-local", Locale.getDefault(), Voice.QUALITY_NORMAL,
            Voice.LATENCY_NORMAL, false, emptySet()))
        shadowOf(context.getSystemService(AudioManager::class.java)).setNextFocusRequestResponse(AudioManager.AUDIOFOCUS_REQUEST_GRANTED)
        backend.ttsEnabled = true; backend.onPreferencesChanged!!.invoke(true); drain()
        val tts = Shadow.extract<PhoneCueTtsShadow>(ShadowTextToSpeech.getLastTextToSpeechInstance())
        tts.onInitListener.onInit(TextToSpeech.SUCCESS); drain()
        start(false); factory.sessions.last().events.final("Synthetic second final"); drain()
        assertEquals("end", cue.active)
        val stops = cue.stops
        tts.beforeSpeak = { assertNull(cue.active); assertTrue(cue.stops > stops) }
        val value = JSONObject().put("kind", "completed").put("speechText", "Synthetic answer-only speech")
            .put("turn", JSONObject().put("answer", JSONObject().put("content", JSONObject().put("kind", "text").put("text", "Synthetic canonical answer"))))
        val replyTicket = phone.onRequestSubmitted("synthetic-typed-answer")
        phone.onRequestResult("synthetic-typed-answer", Result.success(value), replyTicket); drain()
        assertEquals(listOf("Synthetic answer-only speech"), tts.spoken)
        assertEquals("speaking", phone.status().getString("ttsPhase"))
        assertNull(cue.active); assertTrue(cue.stops > stops); assertEquals(2, cue.ends)
        assertTrue(submits().isEmpty())
    }

    @Test @Config(shadows = [PhoneCueTtsShadow::class]) fun stopFencesQueuedTerminalAndOnlyANewSubmissionCanSpeak() {
        val tts = enableTts()
        val old = phone.onRequestSubmitted("synthetic-old-request")
        phone.onRequestResult("synthetic-old-request", answer("Synthetic stale answer"), old)
        phone.stop(); drain()
        assertTrue(tts.spoken.isEmpty()); assertEquals("idle", phone.status().getString("ttsPhase"))
        phone.onRequestResult("synthetic-old-request", answer("Synthetic duplicate stale answer"), old); drain()
        assertTrue(tts.spoken.isEmpty())
        val next = phone.onRequestSubmitted("synthetic-next-request")
        phone.onRequestResult("synthetic-next-request", answer(), next); drain()
        phone.onRequestResult("synthetic-next-request", answer("Synthetic duplicate answer"), next); drain()
        assertEquals(listOf("Synthetic answer-only speech"), tts.spoken)
    }

    @Test @Config(shadows = [PhoneCueTtsShadow::class]) fun backgroundAndResumeFenceQueuedAndLateResultsButAllowNewTypedReply() {
        val oldTts = enableTts()
        val old = phone.onRequestSubmitted("synthetic-background-request")
        phone.onRequestResult("synthetic-background-request", answer("Synthetic queued stale answer"), old)
        phone.foreground(false); phone.foreground(true); drain()
        phone.onRequestResult("synthetic-background-request", answer("Synthetic late stale answer"), old); drain()
        assertTrue(oldTts.spoken.isEmpty())
        val next = phone.onRequestSubmitted("synthetic-resumed-request")
        phone.onRequestResult("synthetic-resumed-request", answer(), next); drain()
        // Rapid lifecycle changes can fence eager preparation; the new reply opens its own SDK session.
        val resumedTts = readyTts()
        assertNotSame(oldTts, resumedTts)
        assertTrue(oldTts.spoken.isEmpty())
        assertEquals(listOf("Synthetic answer-only speech"), resumedTts.spoken)
    }

    @Test @Config(shadows = [PhoneCueTtsShadow::class]) fun closeFencesAlreadyQueuedAndLateTerminalsWithoutReadmittingSpeech() {
        val tts = enableTts()
        val old = phone.onRequestSubmitted("synthetic-close-request")
        phone.onRequestResult("synthetic-close-request", answer(), old)
        phone.close(); drain()
        phone.onRequestResult("synthetic-close-request", answer(), old); phone.foreground(true); drain()
        assertTrue(tts.spoken.isEmpty()); assertEquals("idle", phone.status().getString("ttsPhase"))
    }

    @Test @Config(shadows = [PhoneCueTtsShadow::class]) fun aRetiredTerminalCannotConsumeNewOwnershipEvenWithTheSameRequestId() {
        val tts = enableTts()
        val old = phone.onRequestSubmitted("synthetic-reused-request")
        phone.stop(); drain()
        val next = phone.onRequestSubmitted("synthetic-reused-request")
        phone.onRequestResult("synthetic-reused-request", answer("Synthetic stale answer"), old); drain()
        assertTrue(tts.spoken.isEmpty())
        phone.onRequestResult("synthetic-reused-request", answer(), next); drain()
        assertEquals(listOf("Synthetic answer-only speech"), tts.spoken)
    }

    @Test @Config(shadows = [PhoneCueTtsShadow::class]) fun ownedLoopReplySpeaksAnswerOnlyOnceAndCannotFallBackToTypedSpeechAfterStop() {
        val tts = enableTts()
        start(true); factory.sessions.last().events.final("Synthetic loop final"); drain()
        val id = submits().single().getString("requestId")
        val ticket = phone.onRequestSubmitted(id)
        phone.onRequestResult(id, answer(), ticket); drain()
        assertEquals(listOf("Synthetic answer-only speech"), tts.spoken)
        phone.stop(); drain()
        phone.onRequestResult(id, answer("Synthetic stale loop answer"), ticket); drain()
        assertEquals(listOf("Synthetic answer-only speech"), tts.spoken)
        assertEquals("idle", phone.status().getString("ttsPhase"))
    }

    @Test @Config(shadows = [PhoneCueTtsShadow::class]) fun stopFencesAQueuedGeneratingLoopTerminalInsteadOfReadmittingItAsTypedSpeech() {
        val tts = enableTts()
        start(true); factory.sessions.last().events.final("Synthetic loop final"); drain()
        val id = submits().single().getString("requestId")
        val ticket = phone.onRequestSubmitted(id)
        phone.onRequestResult(id, answer("Synthetic retired loop answer"), ticket)
        phone.stop(); drain()
        assertTrue(tts.spoken.isEmpty()); assertEquals("idle", phone.status().getString("ttsPhase"))
        phone.onRequestResult(id, answer("Synthetic late loop answer"), ticket); drain()
        assertTrue(tts.spoken.isEmpty()); assertEquals("idle", phone.status().getString("ttsPhase"))
    }

    @Test @Config(shadows = [PhoneCueTtsShadow::class]) fun deliberateShortcutReplyStillSpeaksAnswerOnly() {
        val tts = enableTts()
        phone.start("synthetic-shortcut-session", false, "", { events += JSONObject(it.toString()) },
            "11111111-1111-4111-8111-111111111111"); drain()
        factory.sessions.last().events.partial("Synthetic shortcut words"); drain()
        phone.shortcutPrepared("synthetic-shortcut-session")
        phone.stopAndSendShortcut(); drain()
        factory.sessions.last().events.final("Synthetic shortcut final"); drain()
        val id = submits().single().getString("requestId")
        val ticket = phone.onRequestSubmitted(id)
        phone.onRequestResult(id, answer(), ticket); drain()
        assertEquals(listOf("Synthetic answer-only speech"), tts.spoken)
        assertEquals(1, submits().size)
    }

    @Test fun idleBackgroundAndCloseDoNotOverwriteNewerDraftOrResurrectSentFinal() {
        start(false); val sdk = factory.sessions.single()
        sdk.events.final("Synthetic finished words"); drain()
        backend.drafts.clear() // represents an explicit Send clearing the authoritative draft
        phone.stop(); drain(); phone.foreground(false); drain(); phone.close(); drain()
        assertTrue(backend.drafts.isEmpty())
        sdk.events.partial("Synthetic late partial"); sdk.events.final("Synthetic late final"); drain()
        assertTrue(backend.drafts.isEmpty()); assertTrue(submits().isEmpty())
    }
    @Test fun selectedConversationOwnsDraftAndNoMatchIsQuietWithoutPromotingFailedFinal() {
        start(false); val sdk = factory.sessions.single()
        sdk.events.partial("Synthetic partial words"); drain()
        val preserved = backend.drafts.last()
        sdk.events.error("recognition_error_7"); drain()
        assertEquals(preserved, backend.drafts.last())
        assertEquals("error", phone.status().getString("dictation"))
        assertTrue(phone.status().isNull("error"))
        assertEquals("recognition_error_7", phone.status().getString("diagnosticError"))
        assertTrue(sdk.destroyed); assertNull(leases.current()); assertTrue(submits().isEmpty())
        assertEquals(0, cue.ends)
        phone.stop(); drain(); start(false)
        val next = factory.sessions.last()
        backend.conversation = "synthetic-conversation-two"
        backend.drafts.clear()
        next.events.final("Synthetic retired conversation final"); drain()
        assertTrue(backend.drafts.isEmpty())
    }

    @Test fun retryWithFailedRecognizerRetirementCannotOverwriteTheNewConversationDraft() {
        start(false); val sdk = factory.sessions.single()
        sdk.rejectDestroy = true
        try {
            sdk.events.partial("Synthetic old partial"); sdk.events.error("recognition_error_5"); drain()
            backend.conversation = "synthetic-new-conversation"; backend.drafts.clear()
            phone.start("synthetic-retry-session", false, "Synthetic newer edited draft", { events += it }); drain()
            assertEquals("error", phone.status().getString("dictation"))
            assertEquals("recognition_cleanup_failed", phone.status().getString("error"))
            assertEquals("Synthetic newer edited draft", backend.drafts.last())
            assertEquals(1, factory.sessions.size); assertTrue(submits().isEmpty())
        } finally { sdk.rejectDestroy = false }
    }

    @Test fun normalFinishWhileLoopListeningStillSubmitsOnlyTheFinalOnce() {
        start(true); val sdk = factory.sessions.single()
        phone.finish(); drain()
        assertEquals(1, sdk.stops); assertFalse(sdk.destroyed)
        assertTrue(submits().isEmpty())
        sdk.events.final("Synthetic finished words"); sdk.events.final("Synthetic duplicate"); drain()
        assertEquals("Synthetic retained draft Synthetic finished words", submits().single().getString("text"))
    }
}

@Implements(TextToSpeech::class)
class PhoneCueTtsShadow : ShadowTextToSpeech() {
    val spoken = mutableListOf<String>()
    var beforeSpeak: (() -> Unit)? = null
    @Implementation override fun speak(text: CharSequence?, queueMode: Int, params: Bundle?, utteranceId: String?): Int {
        beforeSpeak?.invoke(); spoken += text.toString(); return TextToSpeech.SUCCESS
    }
}
