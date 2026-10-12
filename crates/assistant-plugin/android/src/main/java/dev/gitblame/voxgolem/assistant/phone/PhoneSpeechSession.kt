package dev.gitblame.voxgolem.assistant.phone

import android.content.Context
import android.os.Handler
import android.os.Looper
import dev.gitblame.voxgolem.assistant.recognition.AndroidLocalRecognizerFactory
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.recognition.SharedDictationOwner
import dev.gitblame.voxgolem.assistant.recognition.RecordedAudioFactory
import dev.gitblame.voxgolem.assistant.recognition.RecordedAudioResult
import dev.gitblame.voxgolem.assistant.recognition.VoiceCapturePurpose
import dev.gitblame.voxgolem.assistant.recognition.recognitionUiError
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import dev.gitblame.voxgolem.assistant.speech.ConversationLoopController
import dev.gitblame.voxgolem.assistant.speech.EnginePort
import dev.gitblame.voxgolem.assistant.speech.GenerationResult
import dev.gitblame.voxgolem.assistant.speech.LocalSpeechOutput
import dev.gitblame.voxgolem.assistant.speech.AndroidListeningCue
import dev.gitblame.voxgolem.assistant.speech.MicrophoneCueController
import dev.gitblame.voxgolem.assistant.speech.ListeningCue
import dev.gitblame.voxgolem.assistant.speech.description
import dev.gitblame.voxgolem.assistant.speech.metadata
import dev.gitblame.voxgolem.assistant.speech.SpeechAdmission
import dev.gitblame.voxgolem.assistant.speech.SpeechSelection
import dev.gitblame.voxgolem.assistant.speech.SpeechOptions
import dev.gitblame.voxgolem.assistant.speech.LoopPhase
import dev.gitblame.voxgolem.assistant.speech.VoiceSessionPolicy
import dev.gitblame.voxgolem.assistant.speech.VoiceInputContext
import org.json.JSONObject
import java.util.Locale
import java.util.UUID

internal interface PhoneSpeechBackend {
    fun busy(): Boolean
    fun saveSpeechDraft(text: String)
    fun speechConversation(): String? = null
    fun speechHead(): String? = null
    fun speechOwner() = SpeechInputOwner(speechConversation(), speechHead())
    fun observeSpeechContext(observer: () -> Unit): AutoCloseable = AutoCloseable {}
    fun saveOwnedSpeechDraft(text: String, conversation: String?) { saveSpeechDraft(text) }
    fun saveOwnedSpeechDraft(text: String, conversation: String?, head: String?) {
        if (speechHead() == head) saveOwnedSpeechDraft(text, conversation)
    }
    fun publishSpeechOptions(options: SpeechOptions) {}
    fun stopAll()
    fun nativePreferences(): JSONObject
    var speechStatus: JSONObject?
    var onPreferencesChanged: ((Boolean) -> Unit)?
}

typealias SpeechInputOwner = VoiceInputContext
data class SpeechAdmissionSnapshot(val owner: SpeechInputOwner, val draft: String)

/** Lifecycle/input adapter only. Shared native owners control recognition, playback and loop epochs. */
class PhoneSpeechSession internal constructor(private val context: Context, private val backend: PhoneSpeechBackend,
    private val dictation: SharedDictationOwner = SharedDictationOwner.configured(context) { speechAdmission(backend.nativePreferences()) },
    listeningCue: ListeningCue = AndroidListeningCue(context.applicationContext)) : AutoCloseable {
    constructor(context: Context, backend: EnginePhoneBackend,
        audio: RecordedAudioFactory? = null,
        providerGate: dev.gitblame.voxgolem.assistant.inputtransport.SuppliedAudioProviderGate =
            dev.gitblame.voxgolem.assistant.inputtransport.SuppliedAudioProviderGate.UNPROVEN,
        recordingEnabled: () -> Boolean = { false }) : this(context, object : PhoneSpeechBackend {
        override fun busy() = backend.busy()
        override fun saveSpeechDraft(text: String) = backend.saveSpeechDraft(text)
        override fun speechConversation() = backend.speechConversation()
        override fun speechHead() = backend.speechHead()
        override fun speechOwner() = backend.speechAdmissionSnapshot().owner
        override fun observeSpeechContext(observer: () -> Unit) = backend.observeSpeechContext(observer)
        override fun saveOwnedSpeechDraft(text: String, conversation: String?) = backend.saveOwnedSpeechDraft(text, conversation)
        override fun saveOwnedSpeechDraft(text: String, conversation: String?, head: String?) = backend.saveOwnedSpeechDraft(text, conversation, head)
        override fun publishSpeechOptions(options: SpeechOptions) = backend.publishSpeechOptions(options)
        override fun stopAll() = backend.stopAll()
        override fun nativePreferences() = backend.nativePreferences()
        override var speechStatus: JSONObject?
            get() = backend.speechStatus
            set(value) { backend.speechStatus = value }
        override var onPreferencesChanged: ((Boolean) -> Unit)?
            get() = backend.onPreferencesChanged
            set(value) { backend.onPreferencesChanged = value }
    }, dictation = SharedDictationOwner.configuredWithAudio(context,
        { speechAdmission(backend.nativePreferences()) }, audio = audio,
        providerGate = providerGate, recordingEnabled = recordingEnabled))
    private val main = Handler(Looper.getMainLooper())
    private val output = LocalSpeechOutput.configured(context) { speechAdmission(backend.nativePreferences()) }
    private val microphoneCues = MicrophoneCueController(listeningCue)
    private val preferenceObserver: (Boolean) -> Unit = { enabled ->
        if (!closed) {
            output.setEnabled(enabled)
            if (visible) output.prepare(language)
        }
    }
    private data class ReplyOwner(val requestId: String, val ticket: Long, val loopReply: Boolean)
    private val replyLock = Any()
    private var replySerial = 0L
    private var replyOwner: ReplyOwner? = null
    @Volatile private var closed = false
    private var sink: ((JSONObject) -> Unit)? = null
    private var sessionId: String? = null
    private var mode = "dictation"
    private var waiting: Pair<String, (GenerationResult) -> Unit>? = null
    private var waitingAudio: Pair<String, RecordedAudioResult?>? = null
    private var shortcut: StopSendNewPolicy? = null
    private var shortcutToken: String? = null
    private var shortcutUtterance = 0L
    private var shortcutPrepared = false
    private var shortcutReady = false
    private var shortcutPhase = "idle"
    private var shortcutRequest: String? = null
    private var shortcutTimer: Runnable? = null
    @Volatile private var visible = false
    private var draftUtterance: Long? = null
    private var draftConversation: String? = null
    private var draftHead: String? = null
    private var sessionConversation: String? = null
    private var sessionHead: String? = null
    private var publishedOptions: SpeechOptions? = null
    private val language = Locale.getDefault().toLanguageTag()
    private var inputAudio: RecordedAudioResult? = null
    private val loop = ConversationLoopController(dictation, output, object : EnginePort {
        override fun beginInput() { val owner = backend.speechOwner(); draftConversation = owner.conversation; draftHead = owner.selectedHead }
        override fun setDraft(text: String) = backend.saveOwnedSpeechDraft(text, draftConversation, draftHead)
        override fun setRecordedAudio(audio: RecordedAudioResult?) { inputAudio = audio }
        override fun sendFinal(text: String, onTerminal: (GenerationResult) -> Unit) {
            if (backend.speechOwner() != SpeechInputOwner(draftConversation, draftHead)) { onTerminal(GenerationResult.Failed("conversation_changed")); return }
            val requestId = "voice-${UUID.randomUUID()}"
            waiting = requestId to onTerminal
            waitingAudio = requestId to inputAudio
            emitSubmission(requestId, text)
        }
        override fun stop() { retireReplies(); waiting = null; waitingAudio = null; inputAudio = null; backend.stopAll() }
    }, inputStillOwned = { backend.speechOwner() == SpeechInputOwner(sessionConversation, sessionHead) },
        onReplyCompleted = { reply ->
            val owner = reply.inputContext ?: backend.speechOwner()
            if (sessionConversation == null) sessionConversation = owner.conversation
            sessionHead = owner.selectedHead
        })
    private val subscriptions = listOf(
        dictation.subscribe { value ->
            if (mode == "shortcut" && shortcut != null && value.utterance != shortcutUtterance) return@subscribe
            if (mode == "shortcut" && shortcut?.terminal() == true) return@subscribe
            microphoneCues.onDictation(value)
            var shortcutAction: StopSendNewPolicy.Action = StopSendNewPolicy.Action.None
            var projectedDraft = value.draft
            if (mode == "dictation" && value.utterance == draftUtterance && value.phase in setOf(
                    DictationPhase.LISTENING, DictationPhase.FINISHING, DictationPhase.FINAL, DictationPhase.ERROR)) {
                backend.saveOwnedSpeechDraft(value.draft, draftConversation, draftHead)
                if (value.phase in setOf(DictationPhase.FINAL, DictationPhase.ERROR)) draftUtterance = null
            }
            if (value.phase == DictationPhase.FINAL && output.snapshot().enabled) output.prepare(language)
            if (mode == "shortcut" && shortcut != null) {
                shortcutAction = shortcut!!.state(value.phase.name.lowercase(), value.draft, value.error)
                projectedDraft = shortcut!!.latest
                if (!shortcutReady && projectedDraft.isNotBlank()) {
                    shortcutReady = true
                    emit(JSONObject().put("kind", "shortcut_ready"))
                }
                if (shortcutPrepared && projectedDraft.isNotEmpty()) backend.saveOwnedSpeechDraft(projectedDraft, draftConversation, draftHead)
            }
            emit(JSONObject().put("kind", "dictation").put("phase", value.phase.name.lowercase())
                .put("draft", projectedDraft).put("error", recognitionUiError(value.error) ?: JSONObject.NULL)
                .put("diagnosticError", value.diagnosticError ?: JSONObject.NULL).put("utterance", value.utterance.toString()))
            handleShortcut(shortcutAction)
            publishStatus()
        },
        loop.subscribe { value ->
            emit(JSONObject().put("kind", "loop").put("phase", value.phase.name.lowercase())
                .put("error", recognitionUiError(value.error) ?: JSONObject.NULL).put("diagnosticError", value.error ?: JSONObject.NULL))
            publishStatus()
        },
        output.subscribe { value -> microphoneCues.onSpeech(value); publishStatus() },
        backend.observeSpeechContext { if (!closed) loop.reconcileInputOwner() },
    )

    init {
        backend.onPreferencesChanged = preferenceObserver
        output.setEnabled(backend.nativePreferences().getBoolean("tts_enabled"))
    }

    fun status(): JSONObject {
        val admissionReady = AssistantRuntime.captureResourceAdmissionReady()
        val available = admissionReady && runCatching { dictation.serviceAvailable() }.getOrDefault(false)
        val spoken = output.snapshot()
        val recognized = dictation.snapshot()
        val fallback = backend.nativePreferences().optBoolean("speech_network_fallback", true)
        val recordingEnabled = dictation.recordingEnabled()
        val recordingAvailable = visible && runCatching { dictation.recordingAvailable() }.getOrDefault(false)
        return JSONObject().put("kind", "voice_status").put("available", available)
            .put("reason", if (!admissionReady) "Native recognition resource sharing is not ready; typing remains available"
                else if (recordingEnabled && !recordingAvailable) "Recorded input requires a validated supplied-audio provider and local archive adapter; typing remains available"
                else if (available && fallback) "Native dictation prefers the configured installed language; provider-controlled fallback is allowed"
                else if (available) "Dedicated on-device dictation; network fallback is disabled"
                else "Native speech service is unavailable under the configured policy; typing remains available")
            .put("mode", mode).put("dictation", recognized.phase.name.lowercase()).put("readyForSpeech", recognized.readyForSpeech).put("loop", loop.snapshot().phase.name.lowercase())
            .put("recordingEnabled", recordingEnabled).put("recordingAvailable", recordingAvailable)
            .put("recordingStatus", if (!recordingEnabled) "provider-proof-required" else if (!recordingAvailable)
                "recorded-input-unavailable" else "recorded-input-selected")
            .put("archiveExpected", recognized.archiveExpected)
            .put("recordedAudio", recognized.recordedAudio?.let { JSONObject().put("captureId", it.captureId)
                .put("sha256", it.sha256).put("frameCount", it.frameCount) } ?: JSONObject.NULL)
            .put("ttsEnabled", spoken.enabled).put("ttsAvailable", admissionReady && (spoken.localVoiceAvailable || fallback && spoken.voiceAvailable)).put("ttsPhase", spoken.phase.name.lowercase())
            .put("recognitionRoute", recognized.route?.description() ?: JSONObject.NULL)
            .put("ttsRoute", spoken.route?.description() ?: JSONObject.NULL)
            .put("recognitionMetadata", recognized.route?.metadata() ?: JSONObject.NULL)
            .put("ttsMetadata", spoken.route?.metadata() ?: JSONObject.NULL)
            .put("speechOptions", options().json())
            .put("speechLanguage", backend.nativePreferences().optString("speech_language", ""))
            .put("speechTtsProvider", backend.nativePreferences().optString("speech_tts_provider", ""))
            .put("speechTtsVoice", backend.nativePreferences().optString("speech_tts_voice", ""))
            .put("ttsNetworkRequired", spoken.route?.networkRequired ?: false)
            .put("speechNetworkFallback", fallback)
            .put("localOnlyRecognitionAvailable", AndroidLocalRecognizerFactory(context, allowProviderFallback = false).available())
            .put("diagnosticError", recognized.diagnosticError ?: JSONObject.NULL)
            .put("error", recognitionUiError(recognized.error) ?: recognitionUiError(loop.snapshot().error) ?: spoken.error?.takeIf { spoken.enabled } ?: JSONObject.NULL)
            .put("shortcutPhase", if (mode == "shortcut") shortcutPhase else JSONObject.NULL)
    }

    private fun emit(value: JSONObject) {
        val id = sessionId ?: return
        sink?.invoke(value.put("sessionId", id))
    }
    private fun emitSubmission(requestId: String, text: String) {
        val value = JSONObject().put("kind", "submit").put("requestId", requestId).put("text", text)
        (if (mode == "shortcut") dictation.snapshot().recordedAudio else inputAudio)?.let { value.put("recordedAudio", JSONObject().put("captureId", it.captureId)
            .put("sha256", it.sha256).put("frameCount", it.frameCount)) }
        // Legacy in-memory adapters may use non-canonical synthetic identifiers; production uses canonical IDs.
        if (mode != "shortcut" || shortcutPrepared) runCatching {
            JSONObject().put("sessionId", draftConversation?.let { PhoneSnapshotEncoder.wireId(JSONObject(it)) } ?: "0")
                .put("selectedHead", draftHead?.let { PhoneSnapshotEncoder.wireId(JSONObject(it)) } ?: JSONObject.NULL)
        }.getOrNull()?.let { value.put("context", it) }
        emit(value)
    }
    private fun options() = output.options().copy(recognitionInstalledLanguages = dictation.options().recognitionInstalledLanguages,
        recognitionSupportedLanguages = dictation.options().recognitionSupportedLanguages,
        localOnlyRecognitionAvailable = dictation.localOnlyServiceAvailable())
    private fun publishStatus() {
        val choices = options()
        if (choices != publishedOptions) { publishedOptions = choices; backend.publishSpeechOptions(choices) }
        val value = status()
        backend.speechStatus = value
        if (sessionId != null) emit(JSONObject(value.toString()))
    }

    fun foreground(visible: Boolean) {
        synchronized(replyLock) {
            if (closed) return
            this.visible = visible
            if (!visible) replyOwner = null
        }
        if (!visible) preserveActiveDraft()
        microphoneCues.foreground(visible)
        if (!visible) { cancelShortcut(); sessionId = null; sink = null; waiting = null }
        loop.setForeground(visible)
        if (visible) output.prepare(language)
    }

    fun start(id: String, loopMode: Boolean, draft: String, events: (JSONObject) -> Unit, shortcutLaunch: String? = null,
        completionPolicy: String = if (shortcutLaunch != null) "stop_send_new" else "manual") {
        check(Looper.myLooper() == Looper.getMainLooper())
        if (closed || !visible) throw AssistantPhoneFailure("unavailable", "Voice input requires the visible app")
        if (backend.busy()) throw EnginePhoneBackend.providerFailure("busy")
        if (!status().getBoolean("available") || !dictation.serviceAvailable()) throw AssistantPhoneFailure("unavailable", "Dictation is unavailable under the configured speech policy")
        if (active()) throw EnginePhoneBackend.providerFailure("busy")
        val legacyShortcut = completionPolicy == "stop_send_new"
        val singleTurn = completionPolicy == "single_turn"
        val repeat = completionPolicy == "repeat_after_reply" || completionPolicy == "manual" && loopMode
        require(completionPolicy in setOf("manual", "stop_send_new", "single_turn", "repeat_after_reply"))
        val owner = backend.speechOwner()
        inputAudio = null
        sessionConversation = owner.conversation; sessionHead = owner.selectedHead
        sessionId = id; sink = events; mode = if (legacyShortcut) "shortcut" else if (singleTurn) "ask" else if (repeat) "loop" else "dictation"
        shortcutToken = shortcutLaunch
        if (legacyShortcut) {
            shortcut = StopSendNewPolicy(); shortcutToken = shortcutLaunch; shortcutPrepared = false; shortcutReady = false; shortcutPhase = "listening"
            shortcutUtterance = dictation.start(language, "", purpose = VoiceCapturePurpose.ASK)
        } else if (singleTurn || repeat) loop.start(language, draft, if (singleTurn) VoiceSessionPolicy.SINGLE_TURN else VoiceSessionPolicy.REPEAT_AFTER_REPLY) else {
            draftConversation = owner.conversation; draftHead = owner.selectedHead
            draftUtterance = dictation.start(language, draft)
        }
        publishStatus()
    }

    fun finish() {
        microphoneCues.finish(dictation.snapshot().utterance)
        when {
            mode == "shortcut" -> dictation.endUtterance()
            dictation.snapshot().phase == DictationPhase.CHECKING -> stop()
            else -> loop.endUtterance()
        }
    }
    fun active(): Boolean = dictation.snapshot().phase in setOf(DictationPhase.CHECKING, DictationPhase.LISTENING, DictationPhase.FINISHING) ||
        loop.snapshot().phase in setOf(LoopPhase.LISTENING, LoopPhase.GENERATING, LoopPhase.SPEAKING, LoopPhase.BETWEEN_TURNS) || output.snapshot().phase.name == "SPEAKING" || shortcut?.terminal() == false
    fun shortcutPrepared(id: String) {
        if (sessionId != id || mode != "shortcut" || shortcut == null || !shortcutReady) throw AssistantPhoneFailure("not_found", "Voice shortcut draft is not ready")
        shortcutPrepared = true
        val owner = backend.speechOwner()
        draftConversation = owner.conversation; draftHead = owner.selectedHead
        shortcut?.latest?.takeIf { it.isNotEmpty() }?.let { backend.saveOwnedSpeechDraft(it, draftConversation, draftHead) }
    }
    fun stopAndSendShortcut() {
        val policy = shortcut ?: throw AssistantPhoneFailure("not_found", "No voice shortcut is active")
        if (policy.finishing() || policy.terminal()) return
        shortcutTimer?.let(main::removeCallbacks); shortcutTimer = null
        shortcutPhase = "finishing"
        val action = policy.stopAndSend()
        if (action != StopSendNewPolicy.Action.None) handleShortcut(action)
        else {
            microphoneCues.finish(dictation.snapshot().utterance)
            dictation.endUtterance()
            val timeout = Runnable { handleShortcut(policy.timeout()) }
            shortcutTimer = timeout; main.postDelayed(timeout, 5_000)
        }
        publishStatus()
    }
    private fun handleShortcut(action: StopSendNewPolicy.Action) {
        when (action) {
            is StopSendNewPolicy.Action.Restart -> {
                val policy = shortcut ?: return
                val restart = Runnable {
                    shortcutTimer = null
                    if (shortcut === policy && !policy.finishing() && !policy.terminal() && sessionId != null && visible)
                        shortcutUtterance = dictation.start(language, action.prefix, purpose = VoiceCapturePurpose.ASK)
                }
                shortcutTimer?.let(main::removeCallbacks); shortcutTimer = restart; main.postDelayed(restart, 300)
            }
            is StopSendNewPolicy.Action.Submit -> {
                shortcutTimer?.let(main::removeCallbacks); shortcutTimer = null
                dictation.stop(); shortcutPhase = "submitted"
                val requestId = "shortcut-${UUID.randomUUID()}"; shortcutRequest = requestId
                emitSubmission(requestId, action.text)
            }
            is StopSendNewPolicy.Action.Review -> {
                shortcutTimer?.let(main::removeCallbacks); shortcutTimer = null
                dictation.stop(); shortcutPhase = "review"
                shortcutToken?.let(NewVoiceShortcutLauncher.actions::release)
                emit(JSONObject().put("kind", "shortcut_review").put("draft", action.text)
                    .put("reason", recognitionUiError(action.reason) ?: "Review the available dictation; nothing was sent"))
            }
            StopSendNewPolicy.Action.None -> Unit
        }
    }
    private fun cancelShortcut() {
        shortcut?.cancel(); shortcutTimer?.let(main::removeCallbacks); shortcutTimer = null
        shortcutToken?.let(NewVoiceShortcutLauncher.actions::release)
        shortcut = null; shortcutToken = null; shortcutRequest = null; shortcutPhase = "idle"
    }
    fun stop() {
        retireReplies()
        preserveActiveDraft()
        microphoneCues.cancel()
        cancelShortcut()
        sessionId = null; sink = null; waiting = null
        loop.stop()
    }
    private fun preserveActiveDraft() {
        val value = dictation.snapshot()
        if (mode == "dictation" && value.utterance == draftUtterance && value.phase in setOf(DictationPhase.LISTENING, DictationPhase.FINISHING)) {
            backend.saveOwnedSpeechDraft(value.draft, draftConversation, draftHead)
        }
        draftUtterance = null
    }
    /** Register before submission: even a synchronous terminal must carry this exact ownership ticket. */
    fun onRequestSubmitted(requestId: String): Long = synchronized(replyLock) {
        if (closed || !visible) return@synchronized 0L
        val owner = ReplyOwner(requestId, ++replySerial, waiting?.first == requestId)
        replyOwner = owner
        owner.ticket
    }
    private fun retireReplies() { synchronized(replyLock) { replyOwner = null } }
    /** Revalidate native-minted auto-submit consent when the frontend returns over IPC. */
    fun submissionOwner(requestId: String): SpeechInputOwner? {
        if (closed || !visible || (waiting?.first != requestId && shortcutRequest != requestId) ||
            backend.speechOwner() != SpeechInputOwner(draftConversation, draftHead)) return null
        return SpeechInputOwner(draftConversation, draftHead)
    }
    fun admitsVoiceSubmission(requestId: String): Boolean = submissionOwner(requestId) != null
    fun onRequestResult(requestId: String, result: Result<Any?>, replyTicket: Long) {
        main.post {
            synchronized(replyLock) {
                val owner = replyOwner?.takeIf { !closed && visible && it.requestId == requestId && it.ticket == replyTicket }
                    ?: return@post
                replyOwner = null
                if (shortcutRequest == requestId) { shortcutToken?.let(NewVoiceShortcutLauncher.actions::release); shortcutRequest = null }
                val value = result.getOrNull() as? JSONObject
                val turn = value?.optJSONObject("turn")
                val pending = waiting?.takeIf { it.first == requestId }
                if (pending == null) {
                    if (!owner.loopReply && value != null && !value.has("warning") && turn != null && AssistantRuntime.captureResourceAdmissionReady() && output.snapshot().enabled && value.optString("kind") in setOf("completed", "refused") && loop.snapshot().phase !in setOf(LoopPhase.GENERATING, LoopPhase.SPEAKING, LoopPhase.LISTENING)) {
                        output.speak(value.optString("speechText").takeIf { it.isNotBlank() } ?: turn.getJSONObject("answer").getJSONObject("content").let { it.getString(if (it.getString("kind") == "refusal") "reason" else "text") }, language) { publishStatus() }
                    }
                    return@post
                }
                val audio = waitingAudio?.takeIf { it.first == requestId }?.second
                waiting = null; waitingAudio = null
                pending.second(if (value != null && value.has("warning")) GenerationResult.Failed(value.getString("warning"))
                else if (value != null && turn != null && value.optString("kind") in setOf("completed", "refused")) {
                    GenerationResult.Complete(value.optString("speechText").takeIf { it.isNotBlank() } ?: turn.getJSONObject("answer").getJSONObject("content").let { it.getString(if (it.getString("kind") == "refusal") "reason" else "text") },
                        value.optJSONObject("voiceContext")?.let { context -> SpeechInputOwner(
                            if (context.isNull("conversation")) null else context.getString("conversation"),
                            if (context.isNull("selectedHead")) null else context.getString("selectedHead")) }, audio)
                } else GenerationResult.Failed((result.exceptionOrNull() as? AssistantPhoneFailure)?.boundedMessage ?: "Request interrupted"))
            }
        }
    }

    override fun close() {
        synchronized(replyLock) {
            if (closed) return
            closed = true
            replyOwner = null
        }
        microphoneCues.close()
        stop(); subscriptions.forEach { it.close() }; loop.close(); dictation.close(); output.close()
        if (backend.onPreferencesChanged === preferenceObserver) backend.onPreferencesChanged = null
    }

    companion object {
        private fun speechAdmission(value: JSONObject) = SpeechAdmission(SpeechSelection(value.optString("speech_language", ""),
            value.optString("speech_tts_provider", ""), value.optString("speech_tts_voice", "")), value.optBoolean("speech_network_fallback", true))
        fun recognitionAvailable(context: Context): Boolean = AssistantRuntime.captureResourceAdmissionReady() &&
            runCatching { AndroidLocalRecognizerFactory(context).available() }.getOrDefault(false)
    }
}
