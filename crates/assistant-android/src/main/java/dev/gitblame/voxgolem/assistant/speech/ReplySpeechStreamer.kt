package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.AssistantTiming

/** Rust selects text; this serialized adapter owns only SDK scheduling and callback fences. */
class ReplySpeechStreamer(private val output: SpeechOutputController) : AutoCloseable {
    private var policy: RustReplySpeech? = null
    private var admissionEnabled = output.enabled
    private var sentOffset = 0
    private var awaitingCanonical = false
    private var epoch = 0L
    private var deliveryIdle = true
    private var dispatching = false
    private val deferredCallbacks = LinkedHashMap<Pair<Long, Long>, () -> Unit>(2)
    private var completed = false
    private var settled = false
    private var started = false
    private var sdkStarted = false
    private var selectionClosed = false
    private var finishPosted = false
    private var finishCompleted = false
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
        deliveryIdle = true; deferredCallbacks.clear()
        val hadOutput = started && !settled
        val callback = terminal; terminal = null
        if (hadOutput) output.stop()
        if (notify) callback?.invoke("speech_cancelled")
        sdkStarted = false
        selectionClosed = false; finishPosted = false; finishCompleted = false
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
            completed = true
            apply(decision, onSpeakingStarted)
            finishIfIdle()
        } catch (_: SpeechPolicyUnavailable) { fail("speech_policy_unavailable") }
    }

    private fun observeEnabled() {
        if (admissionEnabled && !output.enabled) {
            disableAdmission()
        }
    }

    private fun disableAdmission() {
        admissionEnabled = false
        // LocalSpeechOutput has already retired disabled SDK resources. Fence all
        // remaining callbacks without cancelling the independently owned generation.
        val callback = terminal
        terminal = null
        started = false
        retire(false)
        terminal = callback
        finishIfIdle()
    }

    private fun apply(decision: SpeechDecision, onStarted: () -> Unit) {
        if (decision.cancel) {
            epoch++
            deliveryIdle = true; deferredCallbacks.clear()
            if (started) output.stop()
            started = false; sdkStarted = false
            selectionClosed = false; finishPosted = false; finishCompleted = false
        }
        selectionClosed = selectionClosed || decision.closed
        deliveryIdle = decision.idle
        if (decision.chunks.isNotEmpty() && finishPosted) {
            // All old chunks acknowledged before close was posted. Canonical
            // settlement can still append a suffix; reopen without prefix replay
            // and fence any old, asynchronously posted cleanup callback.
            epoch++
            sdkStarted = false; finishPosted = false; finishCompleted = false
        }
        val fence = epoch
        dispatching = true
        try {
            for (chunk in decision.chunks) {
                if (epoch != fence || settled) break
                if (!started) { started = true; onStarted() }
                if (epoch != fence || settled) break
                val flush = !sdkStarted
                sdkStarted = true
                output.speakStreamChunk(chunk.text, language, flush, true, if (flush) timing else null) { failure ->
                    synchronized(this) {
                        val callback = callback@{
                            if (epoch != fence || settled) return@callback
                            try {
                                val next = policy?.complete(chunk) ?: return@callback
                                if (!next.accepted) return@callback
                                if (failure == "speech_disabled") disableAdmission()
                                else if (failure != null) fail(failure) else apply(next, {})
                            } catch (_: SpeechPolicyUnavailable) { fail("speech_policy_unavailable") }
                        }
                        // SDK test doubles may callback inline. Dispatch the entire
                        // ordered batch before acknowledging and asking Rust for more.
                        if (dispatching) deferredCallbacks.putIfAbsent(chunk.generation to chunk.id, callback) else callback()
                    }
                }
            }
        } finally { dispatching = false }
        while (deferredCallbacks.isNotEmpty()) {
            val first = deferredCallbacks.entries.first()
            val callback = first.value
            deferredCallbacks.remove(first.key)
            callback()
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
        if (settled || !deliveryIdle) return
        if ((completed || selectionClosed) && started && !finishPosted) {
            finishPosted = true
            val fence = epoch
            output.finishStream { failure -> synchronized(this) {
                if (epoch == fence && !settled) {
                    if (failure != null) fail(failure) else {
                        finishCompleted = true
                        finishIfIdle()
                    }
                }
            } }
            return
        }
        if (!completed || (started && !finishCompleted)) return
        settled = true
        policy?.close(); policy = null
        val callback = terminal; terminal = null
        val fence = epoch
        val after = afterGeneration
        callback?.invoke(null)
        if (epoch == fence) after()
    }
}
