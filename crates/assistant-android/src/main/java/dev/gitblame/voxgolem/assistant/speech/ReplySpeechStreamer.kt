package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.AssistantTiming
import java.util.ArrayDeque

/** Rust selects text; this serialized adapter owns only SDK scheduling and callback fences. */
class ReplySpeechStreamer(private val output: SpeechOutputController) : AutoCloseable {
    private var policy: RustReplySpeech? = null
    private var admissionEnabled = output.enabled
    private var sentOffset = 0
    private var awaitingCanonical = false
    private var epoch = 0L
    private var generation = 0L
    private var lastChunk = 0L
    private val pending = ArrayDeque<SpeechChunk>()
    private var pendingBytes = 0
    // Preserve QUEUE_ADD read-ahead without admitting an unbounded SDK backlog.
    private val inFlight = HashSet<Long>(2)
    private var completed = false
    private var settled = false
    private var started = false
    private var sdkStarted = false
    private var selectionClosed = false
    private var finishPosted = false
    private var language = ""
    private var timing: AssistantTiming? = null
    private var terminal: ((String?) -> Unit)? = null
    private var error: (String) -> Unit = {}
    private var afterGeneration: () -> Unit = {}
    val hasStreamedChunks: Boolean @Synchronized get() = started

    @Synchronized fun cancel() { retire(true); settled = true }
    @Synchronized fun reset() {
        retire(true)
        sentOffset = 0; completed = false; settled = false; started = false
        awaitingCanonical = false
        admissionEnabled = output.enabled
        error = {}; afterGeneration = {}
    }
    @Synchronized override fun close() { cancel() }

    private fun retire(notify: Boolean) {
        epoch++
        policy?.close(); policy = null
        pending.clear(); pendingBytes = 0; inFlight.clear()
        val hadOutput = started && !settled
        val callback = terminal; terminal = null
        if (hadOutput) output.stop()
        if (notify) callback?.invoke("speech_cancelled")
        sdkStarted = false
        selectionClosed = false; finishPosted = false
    }

    // Caller supplies cumulative append-only text, not deltas. Keep only its UTF-16
    // length: append the unseen suffix, and invalidate on observed shrink. Same-length
    // or growing prefix rewrites require canonical settlement to reconcile; no full
    // transcript/prefix is copied or retained here. Even after selection closes, track
    // length for shrink detection, but never bridge unrelated appended paragraphs.
    @Synchronized fun onDelta(fullTextSoFar: String, language: String,
        timingParent: AssistantTiming? = null, onFirstClause: () -> Unit = {},
        onError: (String) -> Unit = {}, onAllChunksCompletedAfterGeneration: () -> Unit = {}) {
        if (settled || completed) return
        this.language = language; timing = timingParent; error = onError
        afterGeneration = onAllChunksCompletedAfterGeneration
        observeEnabled()
        val shrank = fullTextSoFar.length < sentOffset
        val offset = sentOffset
        sentOffset = fullTextSoFar.length
        if (!admissionEnabled || awaitingCanonical) return
        if (selectionClosed && !shrank) return
        try {
            val owner = policy ?: RustReplySpeech(admissionEnabled).also { policy = it }
            val decision = if (shrank) owner.apply(1) else owner.apply(0, fullTextSoFar.substring(offset))
            if (shrank) awaitingCanonical = true
            apply(decision, onFirstClause)
        } catch (_: SpeechPolicyUnavailable) { fail("speech_policy_unavailable") }
    }

    @Synchronized fun onComplete(finalText: String, language: String,
        timingParent: AssistantTiming? = null, onSpeakingStarted: () -> Unit = {}, onSettled: (String?) -> Unit) {
        if (settled || completed) return
        this.language = language; timing = timingParent
        observeEnabled()
        terminal = onSettled
        try {
            val owner = policy ?: RustReplySpeech(admissionEnabled).also { policy = it }
            val decision = owner.apply(2, finalText)
            policy = null // Final decision is detached; callbacks use epoch + generation + chunk fences.
            completed = true
            apply(decision, onSpeakingStarted)
            finishIfIdle()
        } catch (_: SpeechPolicyUnavailable) { fail("speech_policy_unavailable") }
    }

    private fun observeEnabled() {
        if (admissionEnabled && !output.enabled) {
            admissionEnabled = false
            retire(true)
            started = false
        }
    }

    private fun apply(decision: SpeechDecision, onStarted: () -> Unit) {
        if (decision.cancel) {
            epoch++
            pending.clear(); pendingBytes = 0; inFlight.clear()
            if (started) output.stop()
            started = false; sdkStarted = false
            selectionClosed = false; finishPosted = false
        }
        selectionClosed = selectionClosed || decision.closed
        decision.chunk?.let { chunk ->
            if (pending.size >= 100 || pendingBytes + chunk.text.toByteArray(Charsets.UTF_8).size > 600) {
                fail("Speech output capacity exceeded"); return
            }
            generation = chunk.generation; lastChunk = chunk.id
            pending.add(chunk); pendingBytes += chunk.text.toByteArray(Charsets.UTF_8).size
            if (!started) { started = true; onStarted() }
        }
        pump()
    }

    private fun pump() {
        val fence = epoch
        while (!settled && epoch == fence && inFlight.size < 2) {
            val chunk = pending.pollFirst() ?: break
            pendingBytes -= chunk.text.toByteArray(Charsets.UTF_8).size
            inFlight.add(chunk.id)
            val flush = !sdkStarted
            sdkStarted = true
            output.speakStreamChunk(chunk.text, language, flush, true, if (flush) timing else null) { failure ->
                synchronized(this) {
                    if (epoch != fence || generation != chunk.generation || chunk.id > lastChunk || !inFlight.remove(chunk.id)) return@synchronized
                    if (failure != null) fail(failure) else pump()
                }
            }
        }
        if (epoch == fence) finishIfIdle()
    }

    private fun fail(failure: String) {
        if (settled) return
        val callback = terminal; terminal = null
        retire(false); settled = true
        if (callback != null) callback(failure) else error(failure)
    }

    private fun finishIfIdle() {
        if (settled || inFlight.isNotEmpty() || pending.isNotEmpty()) return
        if ((completed || selectionClosed) && started && !finishPosted) {
            finishPosted = true
            output.finishStream()
        }
        if (!completed) return
        settled = true
        val callback = terminal; terminal = null
        val fence = epoch
        val after = afterGeneration
        callback?.invoke(null)
        if (epoch == fence) after()
    }
}
