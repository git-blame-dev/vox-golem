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

/** Strips markdown formatting tokens before passing text to TextToSpeech so syntax characters are not spoken aloud. */
object SpeechTextSanitizer {
    private val FENCED_CODE_REGEX = Regex("```[a-zA-Z0-9_-]*\\s*([\\s\\S]*?)```")
    private val INLINE_CODE_REGEX = Regex("`([^`]+)`")
    private val MARKDOWN_LINK_REGEX = Regex("\\[([^\\]]+)]\\([^)]*\\)")
    private val HEADING_REGEX = Regex("(?m)^\\s*#{1,6}\\s+")
    private val LIST_MARKER_REGEX = Regex("(?m)^\\s*(?:[-*+]|\\d+\\.)\\s+")
    private val BLOCKQUOTE_REGEX = Regex("(?m)^\\s*>+\\s*")
    private val EMPHASIS_MARKER_REGEX = Regex("(\\*\\*|__|~~)")
    private val SINGLE_ASTERISK_REGEX = Regex("(?<=\\s|^)\\*([^*\n]+)\\*(?=\\s|[.,!?;:]|$)")
    private val WHITESPACE_REGEX = Regex("\\s+")

    fun sanitize(text: String): String {
        if (text.isBlank()) return ""
        var result = text
        result = FENCED_CODE_REGEX.replace(result, " $1 ")
        result = INLINE_CODE_REGEX.replace(result, "$1")
        result = MARKDOWN_LINK_REGEX.replace(result, "$1")
        result = HEADING_REGEX.replace(result, "")
        result = LIST_MARKER_REGEX.replace(result, "")
        result = BLOCKQUOTE_REGEX.replace(result, "")
        result = EMPHASIS_MARKER_REGEX.replace(result, "")
        result = SINGLE_ASTERISK_REGEX.replace(result, "$1")
        result = result.replace("`", "")
        result = WHITESPACE_REGEX.replace(result, " ").trim()
        return result
    }
}

/** Incrementally splits cumulative streamed text into speakable sentence/clause chunks. */
class SentenceStreamAccumulator(
    private val hardSplitChars: Int = 140,
) {
    private var consumedLength = 0
    private var consumedPrefix = ""
    private var hasEmittedClause = false

    fun reset() {
        consumedLength = 0
        consumedPrefix = ""
        hasEmittedClause = false
    }

    fun onDelta(fullTextSoFar: String): List<String> {
        val clean = stripSourcesSection(fullTextSoFar)
        if (clean.length < consumedLength || !clean.startsWith(consumedPrefix)) {
            reset()
        }
        val emitted = ArrayList<String>()
        while (consumedLength < clean.length) {
            val unconsumed = clean.substring(consumedLength)
            val splitEnd = findNextBoundary(unconsumed) ?: break
            var nextOffset = splitEnd
            while (nextOffset < unconsumed.length && unconsumed[nextOffset].isWhitespace()) {
                nextOffset++
            }
            val rawClause = unconsumed.substring(0, splitEnd)
            consumedLength += nextOffset
            consumedPrefix = clean.substring(0, consumedLength)
            val sanitized = SpeechTextSanitizer.sanitize(rawClause)
            if (sanitized.isNotEmpty()) {
                emitted += sanitized
                hasEmittedClause = true
            }
        }
        return emitted
    }

    fun flushRemainder(finalText: String): List<String> {
        val clean = stripSourcesSection(finalText)
        val emitted = ArrayList<String>()
        emitted += onDelta(clean)
        if (consumedLength < clean.length) {
            val tail = SpeechTextSanitizer.sanitize(clean.substring(consumedLength))
            if (tail.isNotEmpty()) {
                emitted += tail
            }
        }
        consumedLength = clean.length
        consumedPrefix = clean
        return emitted
    }

    private fun findNextBoundary(unconsumed: String): Int? {
        var codeDelimiter = 0
        var brackets = 0
        var linkParentheses = 0
        var hardBoundary: Int? = null
        var i = 0
        val minimumWords = if (hasEmittedClause) 1 else 3
        fun wordCount(end: Int): Int = SpeechTextSanitizer.sanitize(unconsumed.substring(0, end))
            .split(Regex("\\s+"))
            .count { word -> word.any { it.isLetterOrDigit() } }
        while (i < unconsumed.length) {
            val ch = unconsumed[i]
            if (ch == '`') {
                var end = i + 1
                while (end < unconsumed.length && unconsumed[end] == '`') end++
                val run = end - i
                if (codeDelimiter == 0) codeDelimiter = run else if (codeDelimiter == run) codeDelimiter = 0
                i = end
                continue
            }
            if (codeDelimiter == 0) {
                when (ch) {
                    '[' -> brackets++
                    ']' -> if (brackets > 0) brackets--
                    '(' -> if (linkParentheses > 0 || (i > 0 && unconsumed[i - 1] == ']')) linkParentheses++
                    ')' -> if (linkParentheses > 0) linkParentheses--
                }
                if (brackets == 0 && linkParentheses == 0) {
                    val nextIsWhitespace = i + 1 < unconsumed.length && unconsumed[i + 1].isWhitespace()
                    val atEnd = i == unconsumed.lastIndex
                    val decimal = if (ch == '.' && i > 0 && unconsumed[i - 1].isDigit()) {
                        val tokenStart = unconsumed.lastIndexOfAny(charArrayOf(' ', '\t', '\n'), i - 1) + 1
                        (!atEnd && unconsumed[i + 1].isDigit()) ||
                            (atEnd && '.' !in unconsumed.substring(tokenStart, i))
                    } else false
                    val sentenceBoundary = ch in ".!?:;" && !decimal && (atEnd || nextIsWhitespace)
                    if ((ch == '\n' || sentenceBoundary) && wordCount(i + 1) >= minimumWords) return i + 1
                    if (ch == ',' && nextIsWhitespace && wordCount(i + 1) >= 5) return i + 1
                    if (ch.isWhitespace() && (i <= hardSplitChars || hardBoundary == null) && wordCount(i) >= minimumWords) {
                        hardBoundary = i
                    }
                }
            }
            if (i >= hardSplitChars && hardBoundary != null) return hardBoundary
            i++
        }
        return null
    }

    private fun stripSourcesSection(text: String): String {
        val marker = text.indexOf("\nSources:")
        // Retain the separator already consumed by earlier deltas; shortening that prefix
        // would reset the cursor and speak the answer again when citations arrive.
        return if (marker >= 0) text.substring(0, marker + 1) else text
    }
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
    private val sentenceAccumulator = SentenceStreamAccumulator()
    private var streamedChunksInTurn = 0
    private var pendingStreamChunksInTurn = 0
    private var generationCompletedForTurn = false
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
        resetStreamTurnState()
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
                resetStreamTurnState()
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
        val clauses = sentenceAccumulator.onDelta(fullTextSoFar)
        for (clause in clauses) {
            if (!current(ticket) || turn != generation || state.phase != LoopPhase.GENERATING) return
            enqueueStreamClause(ticket, turn, clause)
        }
    }
    private fun enqueueStreamClause(ticket: Long, turn: Long, clause: String) {
        turnTiming?.mark(Stage.FIRST_CLAUSE_READY)
        streamedChunksInTurn++
        pendingStreamChunksInTurn++
        val isFirst = streamedChunksInTurn == 1
        output.speakStreamChunk(clause, language, flush = isFirst, keepOpen = true, timingParent = turnTiming) { error ->
            main.post { onStreamChunkDone(ticket, turn, error) }
        }
    }
    private fun onStreamChunkDone(ticket: Long, turn: Long, error: String?) {
        if (!current(ticket) || turn != generation) return
        if (error != null) {
            if (state.phase == LoopPhase.GENERATING || state.phase == LoopPhase.SPEAKING) {
                fail(ticket, error)
            }
            return
        }
        pendingStreamChunksInTurn = (pendingStreamChunksInTurn - 1).coerceAtLeast(0)
        if (generationCompletedForTurn && pendingStreamChunksInTurn == 0 && state.phase == LoopPhase.SPEAKING) {
            next(ticket)
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
                val remainingClauses = sentenceAccumulator.flushRemainder(result.finalText)
                if (streamedChunksInTurn == 0 && remainingClauses.size <= 1) {
                    val spokenText = remainingClauses.firstOrNull()
                        ?: SpeechTextSanitizer.sanitize(result.finalText).ifEmpty { result.finalText.trim() }
                    publish(state.copy(phase = LoopPhase.SPEAKING))
                    if (!current(ticket)) return
                    output.speak(spokenText, language, turnTiming) { error -> main.post {
                        if (!current(ticket) || state.phase != LoopPhase.SPEAKING) return@post
                        if (error != null) fail(ticket, error) else next(ticket)
                    } }
                    return
                }
                publish(state.copy(phase = LoopPhase.SPEAKING))
                if (!current(ticket)) return
                for (clause in remainingClauses) {
                    if (!current(ticket) || turn != generation || state.phase != LoopPhase.SPEAKING) return
                    enqueueStreamClause(ticket, turn, clause)
                }
                if (!current(ticket) || turn != generation || state.phase != LoopPhase.SPEAKING) return
                generationCompletedForTurn = true
                output.finishStream()
                if (pendingStreamChunksInTurn == 0) {
                    next(ticket)
                }
            }
        }
    }
    private fun next(ticket: Long) {
        if (!current(ticket)) return
        turnTiming?.finish(Stage.SETTLED); turnTiming = null
        resetStreamTurnState()
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
        onMain { clearPending(); resetStreamTurnState(); finishTimings(Stage.CANCELLED); publish(state.copy(phase = LoopPhase.IDLE, error = null)) }
    }
    private fun cancelOwners(retireGeneration: Boolean = true) {
        clearPending()
        resetStreamTurnState()
        dictation.stop()
        output.stop()
        if (retireGeneration) engine.stop()
    }
    private fun resetStreamTurnState() {
        sentenceAccumulator.reset()
        streamedChunksInTurn = 0
        pendingStreamChunksInTurn = 0
        generationCompletedForTurn = false
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
            resetStreamTurnState()
            recognitionSubscription.close()
            streamSubscription.close()
            observers.clear()
        }
    }
    private fun onMain(action: () -> Unit) {
        if (Looper.myLooper() == main.looper) action() else main.post { action() }
    }
}
