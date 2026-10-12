package dev.gitblame.voxgolem.assistant.phone

import java.util.UUID

/** Process-local deliberate-launch delivery. Never restored from an Activity intent or saved state. */
class ShortcutActionGate {
    private var pending: String? = null
    private var active: String? = null
    private var notified: String? = null
    private val consumed = LinkedHashSet<String>()

    @Synchronized fun offer(token: String): Boolean {
        if (!valid(token) || token in consumed || pending != null || active != null) return false
        pending = token
        return true
    }
    @Synchronized fun consume(): String? {
        val token = pending ?: return null
        pending = null; active = token; consumed += token
        while (consumed.size > 256) consumed.remove(consumed.first())
        return token
    }
    @Synchronized fun active(token: String) = active == token
    @Synchronized fun hasPending() = pending != null
    @Synchronized fun hasAction() = pending != null || active != null
    @Synchronized fun notifyPending(token: String? = null): Boolean {
        val offered = pending ?: return false
        if ((token != null && token != offered) || notified == offered) return false
        notified = offered
        return true
    }
    @Synchronized fun release(token: String) { if (active == token) active = null }
    @Synchronized fun background() { pending = null; active = null; notified = null }

    companion object {
        fun valid(token: String): Boolean = token.length == 36 && runCatching { UUID.fromString(token).let { it.version() == 4 && it.variant() == 2 && it.toString() == token } }.getOrDefault(false)
    }
}
