package dev.gitblame.voxgolem.assistant.recognition

import java.util.concurrent.CopyOnWriteArrayList

/** Process-local admission only: never invokes SDK calls or owner callbacks under its monitor. */
class RecognitionResourceLease {
    enum class Kind { CHAT, CAPTURE_LIVE, CAPTURE_REPLAY, CAPTURE_PROBE, WATCH_NOTE_AUDIO }
    inner class Token internal constructor(val kind: Kind) : AutoCloseable {
        override fun close() {
            val released = synchronized(this@RecognitionResourceLease) {
                if (active !== this) false else { active = null; retirementRetry = null; true }
            }
            if (released) available.forEach { observer -> runCatching { observer(kind) } }
        }
        /** Register only for a failed terminal teardown, never for an active recognition session. */
        fun onRetirementRetry(retry: () -> Boolean) {
            synchronized(this@RecognitionResourceLease) {
                if (active === this) retirementRetry = this to retry
            }
        }
        /** Capture owner calls only after its retired SDK session has actually terminated. */
        fun handoff(next: Kind): Token? = synchronized(this@RecognitionResourceLease) {
            if (active !== this || kind !in setOf(Kind.CAPTURE_REPLAY, Kind.CAPTURE_LIVE) || next != Kind.CAPTURE_LIVE) null
            else Token(next).also { active = it; retirementRetry = null }
        }
    }
    private var active: Token? = null
    private var retirementRetry: Pair<Token, () -> Boolean>? = null
    private val available = CopyOnWriteArrayList<(Kind) -> Unit>()
    fun subscribeAvailable(observer: (Kind) -> Unit): AutoCloseable {
        available += observer
        return AutoCloseable { available -= observer }
    }
    @Synchronized fun tryAcquire(kind: Kind): Token? {
        if (active != null) return null
        return Token(kind).also { active = it }
    }
    @Synchronized fun current(): Token? = active
    /** One explicit attempt. The native owner performs SDK teardown outside this monitor. */
    fun retryFailedRetirement(): Boolean {
        val (owner, retry) = synchronized(this) { active to retirementRetry?.takeIf { it.first === active }?.second }
        if (owner == null) return true
        val completed = retry?.let { runCatching(it).getOrDefault(false) } ?: false
        return completed && synchronized(this) { active !== owner }
    }
    companion object { val process = RecognitionResourceLease() }
}
