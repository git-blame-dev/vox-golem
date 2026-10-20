package dev.gitblame.voxgolem.assistant.speech

import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.recognition.DictationState
import dev.gitblame.voxgolem.assistant.recognition.SharedDictationOwner
import dev.gitblame.voxgolem.assistant.recognition.RecordedAudioResult
import dev.gitblame.voxgolem.assistant.recognition.VoiceCapturePurpose
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.atomic.AtomicLong

data class VoiceInputContext(val conversation: String?, val selectedHead: String?)
sealed class GenerationResult {
    data class Complete(val finalText: String, val inputContext: VoiceInputContext? = null,
        val recordedAudio: RecordedAudioResult? = null) : GenerationResult()
    data class Failed(val code: String) : GenerationResult()
}
/** Adapter must send once and report durable final completion to sendFinal; optional observeStreamText streams provisional text for low-latency clause playback.
 * Callbacks may arrive on any thread. Stop must cancel queued as well as active generation.
 */
interface EnginePort {
    fun beginInput() {}
    fun beginInput(timingParent: AssistantTiming?) { beginInput() }
    fun setDraft(text: String)
    fun setRecordedAudio(audio: RecordedAudioResult?) {}
    fun prewarm() {}
    fun observeStreamText(listener: (String) -> Unit): AutoCloseable = AutoCloseable {}
    fun sendFinal(text: String, onTerminal: (GenerationResult) -> Unit)
    fun stop()
}

enum class LoopPhase { IDLE, LISTENING, GENERATING, SPEAKING, BETWEEN_TURNS, PAUSED, ERROR }
/** Transient consent for this session, never a persisted global speech preference. */
enum class VoiceSessionPolicy { SINGLE_TURN, REPEAT_AFTER_REPLY }
data class ConversationLoopState(val phase: LoopPhase = LoopPhase.IDLE, val draft: String = "", val error: String? = null)

/** Explicit opt-in, bounded utterance -> one generation -> optional local TTS -> next utterance. */
class ConversationLoopController @JvmOverloads constructor(
    private val dictation: SharedDictationOwner,
    private val output: LocalSpeechOutput,
    private val engine: EnginePort,
    private val main: Handler = Handler(Looper.getMainLooper()),
    private val restartDelayMillis: Long = 300,
    private val generationTimeoutMillis: Long = 120_000,
    private val inputStillOwned: () -> Boolean = { true },
    private val onReplyCompleted: (GenerationResult.Complete) -> Unit = {},
    private val draftSyncIntervalMillis: Long = 200,
    private val clockMillis: () -> Long = { SystemClock.elapsedRealtime() },
) : AutoCloseable {
    private val epoch = AtomicLong()
    private val observers = CopyOnWriteArrayList<(ConversationLoopState) -> Unit>()
    @Volatile private var state = ConversationLoopState()
    private var foreground = false
    private var closed = false
    private var language = ""
    private var policy = VoiceSessionPolicy.REPEAT_AFTER_REPLY
    private var utterance = 0L
    private var listeningEpoch = 0L
    private var generation = 0L
    private var pending: Runnable? = null
    private var ttsPreparedForTurn = false
    private var enginePrewarmedForTurn = false
    private var lastDraftSyncAt: Long? = null
    private var unsyncedDraft = false
    private val speechStreamer = ReplySpeechStreamer(output.asController())
    private var flowTiming: AssistantTiming? = null
    private var turnTiming: AssistantTiming? = null
    private val recognitionSubscription = dictation.subscribe { value ->
        main.post { onDictation(value) }
    }
    private val streamSubscription = engine.observeStreamText { fullTextSoFar ->
        onAssistantStreamText(fullTextSoFar)
    }
    fun snapshot(): ConversationLoopState = state
    /** Native context publications retire a listening owner's consent even before another ASR event. */
    fun reconcileInputOwner() { main.post {
        if (state.phase == LoopPhase.LISTENING && !inputStillOwned()) fail(listeningEpoch, "conversation_changed")
    } }
    fun subscribe(observer: (ConversationLoopState) -> Unit): AutoCloseable {
        observers += observer; main.post { if (observer in observers) observer(state) }
        return AutoCloseable { observers -= observer }
    }
    fun setForeground(visible: Boolean, retireGeneration: Boolean = true) {
        if (!visible) { retainInput(epoch.getAndIncrement()); if (retireGeneration) engine.stop() }
        dictation.setForeground(visible); output.setForeground(visible)
        onMain {
            foreground = visible
            if (!visible) { cancelOwners(retireGeneration); finishTimings(Stage.CANCELLED); publish(state.copy(phase = LoopPhase.PAUSED)) }
        }
    }
    /** Foregrounding alone never restarts. Call this only for deliberate visible user consent. */
    fun start(language: String, initialDraft: String = "", policy: VoiceSessionPolicy = VoiceSessionPolicy.REPEAT_AFTER_REPLY,
        timingParent: AssistantTiming? = null) {
        val ticket = epoch.incrementAndGet()
        onMain {
            if (ticket != epoch.get() || closed) { timingParent?.finish(Stage.CANCELLED); return@onMain }
            cancelOwners()
            finishTimings(Stage.REPLACED)
            flowTiming = timingParent
            if (!foreground) { finishTimings(Stage.FAILED); publish(state.copy(phase = LoopPhase.ERROR, error = "visible_activity_required")); return@onMain }
            this.language = language
            this.policy = policy
            listen(ticket, initialDraft)
        }
    }
    private fun listen(ticket: Long, draft: String = "", automaticFollowup: Boolean = false) {
        if (!current(ticket)) return
        if (!inputStillOwned()) { fail(ticket, "conversation_changed"); return }
        listeningEpoch = ticket
        turnTiming = flowTiming?.fork(Operation.VOICE_TURN)?.also { it.mark(Stage.REQUESTED) }
        ttsPreparedForTurn = false
        enginePrewarmedForTurn = false
        lastDraftSyncAt = null
        unsyncedDraft = false
        speechStreamer.reset()
        engine.beginInput(turnTiming)
        engine.setRecordedAudio(null)
        publish(ConversationLoopState(LoopPhase.LISTENING, draft))
        if (!current(ticket)) return
        utterance = dictation.start(language, draft, emptyNoMatchIsIdle = automaticFollowup,
            purpose = if (policy == VoiceSessionPolicy.SINGLE_TURN) VoiceCapturePurpose.ASK else VoiceCapturePurpose.TALK,
            immediateAdmission = true, timingParent = turnTiming)
    }
    private fun onDictation(value: DictationState) {
        val ticket = listeningEpoch
        if (!current(ticket) || state.phase != LoopPhase.LISTENING || value.utterance != utterance) return
        if (!inputStillOwned()) { fail(ticket, "conversation_changed"); return }
        if (value.readyForSpeech && !ttsPreparedForTurn && output.snapshot().enabled) {
            ttsPreparedForTurn = true
            output.prepare(language, turnTiming)
        }
        if (!current(ticket)) return
        val textChanged = state.draft != value.draft
        publish(state.copy(draft = value.draft))
        if (!current(ticket)) return
        if (textChanged && !enginePrewarmedForTurn && value.draft.isNotBlank()) {
            enginePrewarmedForTurn = true
            engine.prewarm()
        }
        if (!current(ticket)) return
        if (value.phase == DictationPhase.FINAL) {
            unsyncedDraft = false
            lastDraftSyncAt = clockMillis()
            engine.setDraft(value.draft)
        } else if (textChanged) {
            val now = clockMillis()
            val previousSync = lastDraftSyncAt
            if (previousSync == null || draftSyncIntervalMillis <= 0L || now - previousSync >= draftSyncIntervalMillis) {
                unsyncedDraft = false
                lastDraftSyncAt = now
                engine.setDraft(value.draft)
            } else {
                unsyncedDraft = true
            }
        }
        if (!current(ticket)) return
        when (value.phase) {
            DictationPhase.IDLE -> if (value.endedWithoutSpeech) {
                epoch.incrementAndGet()
                cancelOwners()
                finishTimings(Stage.SETTLED)
                publish(ConversationLoopState())
            }
            DictationPhase.ERROR -> {
                if (unsyncedDraft && value.draft.isNotBlank()) {
                    unsyncedDraft = false
                    engine.setDraft(value.draft)
                }
                if (!current(ticket)) return
                fail(ticket, value.error ?: "recognition_failed")
            }
            DictationPhase.FINAL -> {
                if (value.archiveExpected && value.recordedAudio == null) { fail(ticket, "recording_save_failed"); return }
                engine.setRecordedAudio(value.recordedAudio)
                if (!current(ticket)) return
                speechStreamer.reset()
                // Phase changes before any external call: repeated SDK final callbacks cannot send twice.
                publish(state.copy(phase = LoopPhase.GENERATING))
                if (!current(ticket)) return
                if (!inputStillOwned()) { fail(ticket, "conversation_changed"); return }
                if (!ttsPreparedForTurn && output.snapshot().enabled) {
                    ttsPreparedForTurn = true
                    output.prepare(language, turnTiming)
                }
                val turn = ++generation
                val deadline = Runnable { if (state.phase == LoopPhase.GENERATING) fail(ticket, "generation_timeout") }
                pending = deadline; main.postDelayed(deadline, generationTimeoutMillis)
                try { engine.sendFinal(value.draft) { result -> main.post { onGeneration(ticket, turn, result) } } }
                catch (_: RuntimeException) { fail(ticket, "generation_failed") }
            }
            else -> Unit
        }
    }
    /** Feeds cumulative provisional assistant text to extract and enqueue completed sentences into TTS early. */
    fun onAssistantStreamText(fullTextSoFar: String) {
        val ticket = epoch.get()
        val turn = generation
        main.post { handleStreamText(ticket, turn, fullTextSoFar) }
    }
    private fun handleStreamText(ticket: Long, turn: Long, fullTextSoFar: String) {
        if (!current(ticket) || turn != generation || state.phase != LoopPhase.GENERATING) return
        if (!output.snapshot().enabled) return
        speechStreamer.onDelta(
            fullTextSoFar = fullTextSoFar,
            language = language,
            timingParent = turnTiming,
            onFirstClause = { turnTiming?.mark(Stage.FIRST_CLAUSE_READY) },
            onError = { error ->
                main.post { onStreamChunkDone(ticket, turn, error) }
            }
        )
    }
    private fun onStreamChunkDone(ticket: Long, turn: Long, error: String?) {
        if (!current(ticket) || turn != generation) return
        if (error != null) {
            if (state.phase == LoopPhase.GENERATING || state.phase == LoopPhase.SPEAKING) {
                fail(ticket, error)
            }
            return
        }
    }
    private fun onGeneration(ticket: Long, turn: Long, result: GenerationResult) {
        if (!current(ticket) || turn != generation || state.phase != LoopPhase.GENERATING) return
        clearPending()
        when (result) {
            is GenerationResult.Failed -> fail(ticket, result.code)
            is GenerationResult.Complete -> {
                if (result.finalText.isBlank()) { fail(ticket, "empty_generation"); return }
                onReplyCompleted(result)
                if (!current(ticket)) return
                if (!output.snapshot().enabled) { next(ticket); return }
                publish(state.copy(phase = LoopPhase.SPEAKING))
                if (!current(ticket)) return
                speechStreamer.onComplete(result.finalText, language, turnTiming) { error ->
                    main.post {
                        if (!current(ticket) || state.phase != LoopPhase.SPEAKING) return@post
                        if (error != null) fail(ticket, error) else next(ticket)
                    }
                }
            }
        }
    }
    private fun next(ticket: Long) {
        if (!current(ticket)) return
        turnTiming?.finish(Stage.SETTLED); turnTiming = null
        speechStreamer.reset()
        if (policy == VoiceSessionPolicy.SINGLE_TURN) {
            flowTiming?.finish(Stage.SETTLED); flowTiming = null
            engine.stop()
            if (!current(ticket)) return
            publish(ConversationLoopState())
            return
        }
        publish(state.copy(phase = LoopPhase.BETWEEN_TURNS))
        if (!current(ticket)) return
        val restart = Runnable { if (current(ticket) && state.phase == LoopPhase.BETWEEN_TURNS) { pending = null; listen(ticket, automaticFollowup = true) } }
        pending = restart; main.postDelayed(restart, restartDelayMillis)
    }
    private fun current(ticket: Long) = ticket == epoch.get() && foreground && !closed
    private fun fail(ticket: Long, error: String) {
        if (!current(ticket)) return
        epoch.incrementAndGet(); cancelOwners(); finishTimings(Stage.FAILED)
        publish(state.copy(phase = LoopPhase.ERROR, error = error))
    }
    /** A finish toggle cancels unstarted admission; an active utterance still requests its final result. */
    fun endUtterance() {
        if (dictation.snapshot().phase == DictationPhase.CHECKING) stop() else dictation.endUtterance()
    }
    /** Stop cancels admission, recognition, generation, playback and every pending next-turn timer. */
    fun stop() {
        retainInput(epoch.getAndIncrement())
        dictation.stop(); output.stop(); engine.stop()
        onMain { clearPending(); speechStreamer.reset(); finishTimings(Stage.CANCELLED); publish(state.copy(phase = LoopPhase.IDLE, error = null)) }
    }
    private fun cancelOwners(retireGeneration: Boolean = true) {
        clearPending()
        speechStreamer.reset()
        dictation.stop()
        output.stop()
        if (retireGeneration) engine.stop()
    }
    private fun retainInput(ticket: Long) {
        val input = dictation.snapshot()
        if (listeningEpoch == ticket && state.phase == LoopPhase.LISTENING && input.utterance == utterance && inputStillOwned() &&
            input.phase in setOf(DictationPhase.LISTENING, DictationPhase.FINISHING, DictationPhase.FINAL)) {
            unsyncedDraft = false
            engine.setDraft(input.draft)
        }
    }
    private fun clearPending() { pending?.let(main::removeCallbacks); pending = null }
    private fun finishTimings(stage: Stage) {
        turnTiming?.finish(stage); turnTiming = null
        flowTiming?.finish(stage); flowTiming = null
    }
    private fun publish(value: ConversationLoopState) { state = value; observers.forEach { it(value) } }
    override fun close() {
        retainInput(epoch.getAndIncrement()); dictation.stop(); output.stop(); engine.stop()
        onMain {
            closed = true
            finishTimings(Stage.CANCELLED)
            clearPending()
            speechStreamer.reset()
            recognitionSubscription.close()
            streamSubscription.close()
            observers.clear()
        }
    }
    private fun onMain(action: () -> Unit) {
        if (Looper.myLooper() == main.looper) action() else main.post { action() }
    }
}
