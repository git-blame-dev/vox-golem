package dev.gitblame.voxgolem.assistant.phone

import java.util.UUID

enum class VoiceEntryMode(val wireValue: String) { ASK("ask"), TALK("talk") }

/** Process-local voice-entry delivery. Never restored from a main Activity intent or saved state. */
class ShortcutActionGate {
    private var pending: String? = null
    private var active: String? = null
    private var notified: String? = null
    private var entryMode: VoiceEntryMode? = null
    private val consumed = LinkedHashSet<String>()

    @Synchronized fun offer(token: String, mode: VoiceEntryMode = VoiceEntryMode.ASK): Boolean {
        if (!valid(token) || token in consumed || pending != null || active != null) return false
        pending = token
        entryMode = mode
        return true
    }
    @Synchronized fun consume(): String? {
        val token = pending ?: return null
        pending = null; active = token; consumed += token
        while (consumed.size > 256) consumed.remove(consumed.first())
        return token
    }
    @Synchronized fun active(token: String) = active == token
    @Synchronized fun mode(token: String) = entryMode.takeIf { pending == token || active == token }
    @Synchronized fun permits(token: String, loopMode: Boolean, policy: String): Boolean = active == token && when (entryMode) {
        VoiceEntryMode.ASK -> !loopMode && policy in setOf("single_turn", "stop_send_new")
        VoiceEntryMode.TALK -> loopMode && policy == "repeat_after_reply"
        null -> false
    }
    @Synchronized fun hasPending() = pending != null
    @Synchronized fun hasAction() = pending != null || active != null
    @Synchronized fun notifyPending(token: String? = null): Boolean {
        val offered = pending ?: return false
        if ((token != null && token != offered) || notified == offered) return false
        notified = offered
        return true
    }
    @Synchronized fun release(token: String) { if (active == token) { active = null; entryMode = null } }
    @Synchronized fun background() { pending = null; active = null; notified = null; entryMode = null }

    companion object {
        fun valid(token: String): Boolean = token.length == 36 && runCatching { UUID.fromString(token).let { it.version() == 4 && it.variant() == 2 && it.toString() == token } }.getOrDefault(false)
    }
}
