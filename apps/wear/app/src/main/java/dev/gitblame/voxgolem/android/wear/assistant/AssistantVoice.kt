package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Context
import android.os.Handler
import android.os.Looper
import dev.gitblame.voxgolem.assistant.recognition.*
import dev.gitblame.voxgolem.assistant.speech.*
import dev.gitblame.voxgolem.assistant.runtime.*
import java.util.Locale
import java.lang.ref.WeakReference
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantDiagnosticJournal.Metadata

enum class AssistantVoiceEntryMode { ASK, TALK }

private fun activeVoicePhases(dictation: DictationPhase, speech: SpeechPhase, loop: LoopPhase): Boolean =
    dictation == DictationPhase.CHECKING || dictation == DictationPhase.LISTENING || dictation == DictationPhase.FINISHING ||
        speech == SpeechPhase.CHECKING || speech == SpeechPhase.SPEAKING ||
        loop == LoopPhase.LISTENING || loop == LoopPhase.GENERATING || loop == LoopPhase.SPEAKING || loop == LoopPhase.BETWEEN_TURNS

internal data class AssistantVoiceOperationStatus(val active: Boolean, val failed: Boolean)

internal object AssistantVoiceLaunchGate {
    fun shouldStart(
        visible: Boolean,
        resumed: Boolean,
        state: AssistantState?,
        pendingMode: AssistantVoiceEntryMode?,
        hasRecordAudioPermission: Boolean = true,
        voiceActive: Boolean = false,
        inputPending: Boolean = false,
        permissionPending: Boolean = false,
        finishingOrDestroyed: Boolean = false,
    ): Boolean {
        if (!visible || !resumed || finishingOrDestroyed || pendingMode == null || !hasRecordAudioPermission ||
            voiceActive || inputPending || permissionPending) return false
        if (state == null || !state.contextReady || state.busy || state.model !in AssistantModels.aliases) return false
        return true
    }
}

data class AssistantVoiceState(
    val dictation: DictationState = DictationState(),
    val speech: SpeechState = SpeechState(),
    val loop: ConversationLoopState = ConversationLoopState(),
    val admissionReady: Boolean = false,
    val error: String? = null,
    val recognitionServiceAvailable: Boolean = true,
    val localOnlyRecognitionAvailable: Boolean = false,
    val speechOptions: SpeechOptions = SpeechOptions(),
    val speechSelection: SpeechSelection = SpeechSelection(),
    val entryMode: AssistantVoiceEntryMode? = null,
    val admissionPending: Boolean = false,
    val launchConfirmationRequired: Boolean = false,
    val recordingEnabled: Boolean = false,
    val recordingAvailable: Boolean = false,
    val recordingStatus: String = "provider-proof-required",
    val missingSpeechServices: Boolean = false,
    val speechRequirementMessage: String? = null,
) {
    val listening: Boolean get() = dictation.phase in setOf(DictationPhase.CHECKING, DictationPhase.LISTENING, DictationPhase.FINISHING)
    val active: Boolean get() = admissionPending || activeVoicePhases(dictation.phase, speech.phase, loop.phase)
}

interface AssistantVoiceController : AutoCloseable {
    fun snapshot(): AssistantVoiceState
    fun subscribe(observer: (AssistantVoiceState) -> Unit): AutoCloseable
    fun foreground(visible: Boolean)
    fun dictate(draft: String)
    /** Single authoritative utterance -> one reply -> idle. Legacy draft dictation is unchanged. */
    fun ask(draft: String) { dictate(draft) }
    fun ask(draft: String, timingParent: AssistantTiming?) { ask(draft) }
    fun conversation(draft: String)
    fun conversation(draft: String, timingParent: AssistantTiming?) { conversation(draft) }
    fun endUtterance()
    fun stop()
    fun tts(enabled: Boolean)
    fun prepareSpeechOptions() {}
    fun speak(text: String)
    /** Returns false only when the controller needs the client's legacy Send adapter. */
    fun send(): Boolean = false
    fun configureSpeech(selection: SpeechSelection): Boolean = false
    /** True means submitted. Only the callback reports actor commit/reject; local rejection has no callback. */
    fun configureSpeech(selection: SpeechSelection, onSettled: (Boolean) -> Unit): Boolean = false
    fun speechLanguage(language: String): Boolean = configureSpeech(snapshot().speechSelection.copy(language = language))
    fun speechTtsProvider(provider: String): Boolean = configureSpeech(snapshot().speechSelection.copy(ttsProvider = provider))
    fun speechTtsVoice(voice: String): Boolean = configureSpeech(snapshot().speechSelection.copy(ttsVoice = voice))
}

internal class WearAssistantVoice(
    private val engine: AssistantClient,
    private val dictation: SharedDictationOwner,
    private val output: LocalSpeechOutput,
    private val generation: EnginePort,
    loop: ConversationLoopController? = null,
    private val admissionReady: () -> Boolean = AssistantRuntime::captureResourceAdmissionReady,
    private val language: String = Locale.getDefault().toLanguageTag(),
    private val main: Handler = Handler(Looper.getMainLooper()),
    private val listeningCue: ListeningCue? = null,
    private val context: Context? = null,
) : AssistantVoiceController {
    constructor(context: Context, engine: AssistantEngine,
        providerGate: dev.gitblame.voxgolem.assistant.inputtransport.SuppliedAudioProviderGate =
            dev.gitblame.voxgolem.assistant.inputtransport.SuppliedAudioProviderGate.UNPROVEN,
        recordingEnabled: () -> Boolean = { false }) :
        this(engine, SharedDictationOwner.configuredWithAudio(context, { engine.snapshot().speechAdmission() },
                audio = recordedVoiceCapture(context), providerGate = providerGate, recordingEnabled = recordingEnabled),
            LocalSpeechOutput.configured(context) { engine.snapshot().speechAdmission() }, AssistantEnginePort(engine),
            listeningCue = AndroidListeningCue(context.applicationContext),
            context = context.applicationContext)
    private val observers = mutableListOf<(AssistantVoiceState) -> Unit>()
    private var mode = false
    private var entryMode: AssistantVoiceEntryMode? = null
    private var sessionConversation: String? = null
    private var sessionHead: String? = null
    private val loop = loop ?: ConversationLoopController(dictation, output, generation,
        inputStillOwned = { engine.snapshot().let { it.conversation == sessionConversation && it.selectedHead == sessionHead } },
        onReplyCompleted = { reply ->
            val owner = reply.inputContext ?: engine.snapshot().let { VoiceInputContext(it.conversation, it.selectedHead) }
            if (sessionConversation == null) sessionConversation = owner.conversation
            sessionHead = owner.selectedHead
        })
    private var error: String? = null
    private var lastFinal = 0L
    private var draftUtterance: Long? = null
    private var draftConversation: String? = null
    private var draftHead: String? = null
    private val microphoneCues = listeningCue?.let(::MicrophoneCueController)
    private var recognitionServiceAvailable = false
    private var localOnlyRecognitionAvailable = false
    private var missingSpeechServices = false
    private var speechRequirementMessage: String? = null
    private var lastAdmission = engine.snapshot().speechAdmission()
    private var optionsSource = optionsInput().frozenLanguages()
    private var speechOptions = optionsSource.bounded()
    private var publishedOptions: SpeechOptions? = null
    private val replyEpoch = AtomicLong()
    private var manualRequest: Long? = null
    // Own the policy before its first clause and retain effects ownership through playback.
    private var manualSpeechStreamer: ReplySpeechStreamer? = null
    private var manualStreamSubscription: AutoCloseable? = null
    private var foreground = false
    private var closed = false
    private var repliesEnabled = false
    private class SelectionSettlement(consumer: (Boolean) -> Unit) {
        val consumer = AtomicReference<((Boolean) -> Unit)?>(consumer)
        val received = AtomicBoolean(false)
    }
    private val selectionSettlement = AtomicReference<SelectionSettlement?>(null)
    private val subscriptions = listOf(
        dictation.subscribe { value ->
            updateOptions()
            microphoneCues?.onDictation(value)
            if (!mode && value.phase in setOf(DictationPhase.FINAL, DictationPhase.ERROR) && value.utterance != lastFinal && value.utterance == draftUtterance) {
                lastFinal = value.utterance
                saveOwnedDraft(value.draft, interrupted = value.phase == DictationPhase.ERROR)
                draftUtterance = null
                if (value.phase == DictationPhase.FINAL && output.snapshot().enabled) output.prepare(language)
            }
            publish()
        },
        output.subscribe { value -> microphoneCues?.onSpeech(value); updateOptions(); publish() },
        engine.subscribe { value -> main.post {
            if (closed) return@post
            this.loop.reconcileInputOwner()
            val admission = value.speechAdmission()
            if (admission != lastAdmission) {
                lastAdmission = admission
                recognitionServiceAvailable = foreground && runCatching { dictation.serviceAvailable() }.getOrDefault(false)
                publish()
            }
        } },
        this.loop.subscribe { value ->
            mode = value.phase in setOf(LoopPhase.LISTENING, LoopPhase.GENERATING, LoopPhase.SPEAKING, LoopPhase.BETWEEN_TURNS)
            publish()
        },
    )
    override fun snapshot(): AssistantVoiceState {
        val recordingEnabled = dictation.recordingEnabled()
        val recordingAvailable = foreground && runCatching { dictation.recordingAvailable() }.getOrDefault(false)
        return AssistantVoiceState(dictation.snapshot(), output.snapshot(), loop.snapshot(), admissionReady(), error,
            recognitionServiceAvailable = recognitionServiceAvailable,
            localOnlyRecognitionAvailable = localOnlyRecognitionAvailable, speechOptions = options(),
            speechSelection = engine.snapshot().speechAdmission().selection, entryMode = entryMode,
            recordingEnabled = recordingEnabled, recordingAvailable = recordingAvailable,
            recordingStatus = if (!recordingEnabled) "provider-proof-required" else if (!recordingAvailable)
                "recorded-input-unavailable" else "recorded-input-selected",
            missingSpeechServices = missingSpeechServices,
            speechRequirementMessage = speechRequirementMessage)
    }
    internal fun operationStatus(): AssistantVoiceOperationStatus {
        val input = dictation.snapshot()
        val speech = output.snapshot()
        val conversation = loop.snapshot()
        return AssistantVoiceOperationStatus(activeVoicePhases(input.phase, speech.phase, conversation.phase),
            error != null || input.error != null || speech.error != null || conversation.error != null)
    }
    private fun options() = speechOptions
    private fun optionsInput(): SpeechOptions {
        val route = dictation.snapshot().route
        return output.options().copy(recognitionInstalledLanguages = route?.installedLanguages,
            recognitionSupportedLanguages = route?.supportedLanguages, localOnlyRecognitionAvailable = localOnlyRecognitionAvailable)
    }
    private fun SpeechOptions.frozenLanguages() = copy(recognitionInstalledLanguages = recognitionInstalledLanguages?.toList(),
        recognitionSupportedLanguages = recognitionSupportedLanguages?.toList())
    private fun updateOptions() {
        val source = optionsInput()
        if (source != optionsSource) {
            optionsSource = source.frozenLanguages()
            speechOptions = optionsSource.bounded()
        }
        val value = options()
        if (value != publishedOptions) { publishedOptions = value; engine.publishSpeechOptions(value) }
    }
    override fun configureSpeech(selection: SpeechSelection): Boolean {
        if (!admitSpeechSelection(selection)) return false
        engine.setSpeechSelection(selection)
        error = null; return true
    }
    private fun admitSpeechSelection(selection: SpeechSelection): Boolean {
        if (closed || !foreground || snapshot().active || engine.snapshot().busy || selectionSettlement.get() != null) return false
        try { options().validate(selection, engine.snapshot().speechNetworkFallback) }
        catch (failure: IllegalArgumentException) { error = failure.message; publish(); return false }
        return true
    }
    override fun configureSpeech(selection: SpeechSelection, onSettled: (Boolean) -> Unit): Boolean {
        if (!admitSpeechSelection(selection)) return false
        val ticket = SelectionSettlement(onSettled)
        if (!selectionSettlement.compareAndSet(null, ticket)) return false
        val owner = WeakReference(this)
        val handler = main
        try {
            engine.setSpeechSelection(selection) { saved ->
                if (!ticket.received.compareAndSet(false, true) || ticket.consumer.get() == null) return@setSpeechSelection
                val success = saved.error == null && saved.speechAdmission().selection == selection
                val failure = if (success) null else saved.error ?: "speech_selection_not_saved"
                if (!handler.post {
                    val current = owner.get()
                    if (current == null || !current.selectionSettlement.compareAndSet(ticket, null)) {
                        ticket.consumer.set(null); return@post
                    }
                    val consumer = ticket.consumer.getAndSet(null)
                    if (current.closed) return@post
                    current.error = failure
                    if (success) current.prepareSpeechOptions()
                    try { current.publish() } catch (_: RuntimeException) { /* Observer failure cannot suppress settlement. */ }
                    try { consumer?.invoke(success) } catch (_: RuntimeException) { /* A Settings consumer cannot retire the controller. */ }
                }) ticket.consumer.set(null)
            }
        } catch (failure: RuntimeException) {
            ticket.consumer.set(null)
            selectionSettlement.compareAndSet(ticket, null)
            error = if (failure is UnsupportedOperationException) "speech_selection_unavailable" else "speech_selection_failed"
            publish()
            return false
        }
        error = null
        return true
    }
    override fun subscribe(observer: (AssistantVoiceState) -> Unit): AutoCloseable {
        observers += observer; observer(snapshot()); return AutoCloseable { observers -= observer }
    }
    override fun foreground(visible: Boolean) {
        if (closed) return
        foreground = visible
        microphoneCues?.foreground(visible)
        if (!visible) retireManualReply()
        recognitionServiceAvailable = visible && runCatching { dictation.serviceAvailable() }.getOrDefault(false)
        localOnlyRecognitionAvailable = visible && runCatching { dictation.localOnlyServiceAvailable() }.getOrDefault(false)
        missingSpeechServices = context?.let { ctx -> runCatching { SpeechServiceRequirement.isMissing(ctx) }.getOrDefault(false) } ?: false
        speechRequirementMessage = if (missingSpeechServices) context?.let { ctx -> runCatching { SpeechServiceRequirement.missingRequirementDescription(ctx) }.getOrNull() } else null
        updateOptions()
        if (!visible) preservePartial()
        loop.setForeground(visible)
        if (!visible) mode = false
        publish()
    }
    override fun prepareSpeechOptions() {
        if (closed || !foreground || snapshot().active || engine.snapshot().busy) return
        output.refreshProviders()
        output.prepare(language)
    }
    private fun admit(): Boolean {
        if (closed || !foreground) return false
        if (!engine.snapshot().contextReady) { error = "context_not_ready"; publish(); return false }
        if (!admissionReady()) { error = "capture_admission_unavailable"; publish(); return false }
        if (engine.snapshot().busy || manualRequest != null) { error = "generation_busy"; publish(); return false }
        error = null
        return true
    }
    override fun dictate(draft: String) {
        if (!admit()) return
        retireManualReply()
        mode = false; entryMode = null; loop.stop()
        val owner = engine.snapshot()
        draftConversation = owner.conversation; draftHead = owner.selectedHead
        generation.prewarm()
        draftUtterance = dictation.start(language, draft)
    }
    override fun conversation(draft: String) {
        startVoice(draft, AssistantVoiceEntryMode.TALK)
    }
    override fun conversation(draft: String, timingParent: AssistantTiming?) = startVoice(draft, AssistantVoiceEntryMode.TALK, timingParent)
    override fun ask(draft: String) {
        startVoice(draft, AssistantVoiceEntryMode.ASK)
    }
    override fun ask(draft: String, timingParent: AssistantTiming?) = startVoice(draft, AssistantVoiceEntryMode.ASK, timingParent)
    private fun startVoice(draft: String, entry: AssistantVoiceEntryMode, timingParent: AssistantTiming? = null) {
        if (!admit()) { timingParent?.finish(AssistantTiming.Stage.FAILED); return }
        val timing = timingParent ?: AssistantTiming.begin(context, AssistantTiming.Operation.VOICE_FLOW).also {
            it.mark(AssistantTiming.Stage.REQUESTED)
            it.metadata(Metadata(entry = if (entry == AssistantVoiceEntryMode.ASK) "ask" else "talk"))
        }
        retireManualReply()
        listeningCue?.timing(timing)
        entryMode = entry
        val owner = engine.snapshot()
        sessionConversation = owner.conversation; sessionHead = owner.selectedHead
        mode = true; loop.start(language, draft, if (entry == AssistantVoiceEntryMode.ASK) VoiceSessionPolicy.SINGLE_TURN else VoiceSessionPolicy.REPEAT_AFTER_REPLY,
            timingParent = timing)
    }
    override fun send(): Boolean {
        if (closed || !foreground || mode || snapshot().listening || engine.snapshot().busy || manualRequest != null) return true
        retireManualReply()
        val ticket = replyEpoch.incrementAndGet()
        manualRequest = ticket
        val speakReply = repliesEnabled
        error = null
        if (output.snapshot().phase != SpeechPhase.PREPARING) output.stop()
        if (speakReply) output.prepare(language)
        val speechStreamer = ReplySpeechStreamer(output.asController()).also { manualSpeechStreamer = it }
        manualStreamSubscription?.close()
        manualStreamSubscription = if (speakReply) {
            generation.observeStreamText { fullTextSoFar ->
                main.post {
                    if (manualRequest == ticket && currentReply(ticket)) {
                        speechStreamer.onDelta(fullTextSoFar, language, onError = { failure ->
                            main.post { if (currentReply(ticket)) { error = failure; publish() } }
                        })
                    }
                }
            }
        } else null
        try {
            generation.sendFinal(engine.snapshot().draft) { result -> main.post {
                if (manualRequest != ticket) return@post
                manualRequest = null
                manualStreamSubscription?.close()
                manualStreamSubscription = null
                if (!speakReply || !currentReply(ticket) || result !is GenerationResult.Complete || result.finalText.isBlank()) {
                    if (manualSpeechStreamer === speechStreamer) manualSpeechStreamer = null
                    speechStreamer.cancel()
                    return@post
                }
                speechStreamer.onComplete(result.finalText, language) { failure -> main.post {
                    if (manualSpeechStreamer === speechStreamer) manualSpeechStreamer = null
                    if (currentReply(ticket)) { error = failure; publish() }
                } }
            } }
        } catch (_: RuntimeException) {
            if (manualRequest == ticket) { retireManualReply(); error = "generation_failed"; publish() }
        }
        return true
    }
    private fun currentReply(ticket: Long) = ticket == replyEpoch.get() && foreground && repliesEnabled && !closed && !mode
    private fun retireManualReply() {
        replyEpoch.incrementAndGet()
        manualRequest = null
        manualStreamSubscription?.close()
        manualStreamSubscription = null
        val retired = manualSpeechStreamer
        manualSpeechStreamer = null
        retired?.cancel()
    }
    override fun endUtterance() {
        microphoneCues?.finish(dictation.snapshot().utterance)
        loop.endUtterance()
    }
    override fun stop() {
        microphoneCues?.cancel(); retireManualReply(); preservePartial()
        mode = false; loop.stop(); publish()
    }
    override fun tts(enabled: Boolean) {
        if (closed) return
        repliesEnabled = enabled
        if (!enabled) retireManualReply()
        output.setEnabled(enabled)
        publish()
    }
    override fun speak(text: String) {
        if (!admit() || snapshot().listening || mode) return
        val firstParagraph = FirstParagraphSpeech.extract(text)
        if (firstParagraph.isBlank()) return
        output.speak(firstParagraph, language) { failure -> error = failure; publish() }
    }
    private fun preservePartial() {
        val value = dictation.snapshot()
        if (value.utterance == draftUtterance && value.phase in setOf(DictationPhase.LISTENING, DictationPhase.FINISHING) && value.draft.isNotBlank() && !engine.snapshot().busy) {
            saveOwnedDraft(value.draft, true)
        }
        draftUtterance = null
    }
    private fun saveOwnedDraft(text: String, interrupted: Boolean = false) {
        if (engine is AssistantEngine) engine.setSpeechDraft(text, draftConversation, interrupted, expectedHead = draftHead, requireHeadOwner = true)
        else if (engine.snapshot().let { it.conversation == draftConversation && it.selectedHead == draftHead }) engine.setDraft(text)
    }
    private fun publish() { val value = snapshot(); observers.toList().forEach { it(value) } }
    override fun close() {
        if (closed) return
        closed = true; retireManualReply()
        selectionSettlement.getAndSet(null)?.consumer?.set(null)
        microphoneCues?.close()
        preservePartial(); subscriptions.forEach(AutoCloseable::close)
        loop.close(); dictation.close(); output.close(); observers.clear()
    }
}
