package dev.gitblame.voxgolem.assistant.runtime

/** Construct before the pipe write attempt, including lock/write/flush; not an HTTP or handshake timer. */
internal class AgyRequestTiming(private val elapsedRealtimeMs: () -> Long) {
    private val dispatchMs = elapsedRealtimeMs()
    var firstTextMs: Long? = null
        private set

    // Only call for answer text being forwarded, never internal thought/reasoning events.
    fun onForwardedText(text: String): Long? {
        if (firstTextMs != null || text.isBlank()) return null
        return elapsedMs().also { firstTextMs = it }
    }

    fun completedMs(): Long = elapsedMs()

    private fun elapsedMs(): Long = (elapsedRealtimeMs() - dispatchMs).coerceAtLeast(0L)
}
