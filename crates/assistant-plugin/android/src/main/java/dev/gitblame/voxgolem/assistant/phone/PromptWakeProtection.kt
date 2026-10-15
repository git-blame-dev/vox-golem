package dev.gitblame.voxgolem.assistant.phone

import android.os.Handler
import android.os.HandlerThread

/** Bounded platform leases, renewed only while the exact request awaits worker-retired settlement. */
internal class PromptWakeProtection(private val scheduler: Handler,
    private val acquire: (Long) -> Unit, private val release: () -> Unit,
    private val ownedThread: HandlerThread? = null) : AutoCloseable {
    private constructor(thread: HandlerThread, acquire: (Long) -> Unit, release: () -> Unit) :
        this(Handler(thread.looper), acquire, release, thread)
    constructor(acquire: (Long) -> Unit, release: () -> Unit) :
        this(HandlerThread("assistant-request-wake").apply { start() }, acquire, release)
    private var owner: String? = null
    private var renewal: Runnable? = null
    private var closing = false
    @Synchronized fun begin(requestId: String) {
        check(owner == null && !closing)
        owner = requestId
        renew(requestId)
    }
    @Synchronized private fun renew(requestId: String) {
        if (owner != requestId) return
        runCatching { acquire(LEASE_MS) }
        val next = Runnable { renew(requestId) }
        renewal = next
        scheduler.postDelayed(next, RENEW_MS)
    }
    @Synchronized fun retired(requestId: String) {
        if (owner != requestId) return
        owner = null
        renewal?.let(scheduler::removeCallbacks); renewal = null
        runCatching { release() }
        if (closing) ownedThread?.quitSafely()
    }
    /** Activity destruction does not release a lease whose provider still owns physical work. */
    @Synchronized override fun close() { closing = true; if (owner == null) ownedThread?.quitSafely() }
    companion object { const val LEASE_MS = 60_000L; const val RENEW_MS = 30_000L }
}
