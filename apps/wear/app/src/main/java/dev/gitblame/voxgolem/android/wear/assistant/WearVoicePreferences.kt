package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Context
import android.content.SharedPreferences

internal data class WearVoicePreferences(val readRepliesAloud: Boolean = true, val conversationMode: Boolean = true)

// Local presentation intent ordering, shared across Activity owners. Not a domain clock.
internal object WearVoiceIntentOrder {
    private val ordinal = java.util.concurrent.atomic.AtomicLong()
    @Volatile var seeded = false
        private set
    fun next(): Long = ordinal.incrementAndGet()
    fun restore(value: Long): Long {
        ordinal.accumulateAndGet(value) { current, restored -> maxOf(current, restored) }
        return value
    }
    fun seed(value: Long) {
        restore(value)
        seeded = true
    }
}

internal class WearVoiceSettings(private val prefs: SharedPreferences) {
    private var acknowledged: WearVoicePreferences? = null
    var saveFailed: Boolean = false
        private set
    var successfulModeOrdinal: Long = 0L
        private set
    companion object {
        internal const val ModeOrdinalKey = "voice-mode-intent-ordinal"
        private var production: WearVoiceSettings? = null
        @Synchronized fun production(context: Context): WearVoiceSettings = production
            ?: WearVoiceSettings(context.applicationContext).also { production = it }
    }
    private constructor(context: Context) : this(context.getSharedPreferences("assistant-runtime", Context.MODE_PRIVATE))
    fun read(): WearVoicePreferences {
        acknowledged?.let { return it }
        val values = current()
        val modeOrdinal = prefs.getLong(ModeOrdinalKey, 0L).coerceAtLeast(0L)
        WearVoiceIntentOrder.seed(modeOrdinal)
        successfulModeOrdinal = modeOrdinal
        acknowledged = values
        return values
    }
    private fun current(): WearVoicePreferences = WearVoicePreferences(
        prefs.getBoolean("read-replies-aloud", true), prefs.getBoolean("conversation-mode", true))
    fun setReadRepliesAloud(enabled: Boolean) {
        save(read().copy(readRepliesAloud = enabled))
    }
    fun setConversationMode(enabled: Boolean, intentOrdinal: Long? = null) {
        val values = read()
        save(values.copy(conversationMode = enabled), intentOrdinal ?: WearVoiceIntentOrder.next())
    }
    private fun save(candidate: WearVoicePreferences, modeOrdinal: Long = successfulModeOrdinal) {
        val durableOrdinal = maxOf(successfulModeOrdinal, modeOrdinal)
        saveFailed = true
        check(prefs.edit().putBoolean("read-replies-aloud", candidate.readRepliesAloud)
            .putBoolean("conversation-mode", candidate.conversationMode)
            .putLong(ModeOrdinalKey, durableOrdinal).commit()) { "Could not save voice preferences" }
        acknowledged = candidate
        successfulModeOrdinal = durableOrdinal
        saveFailed = false
    }
}

// Process-wide ordering also covers an Activity replacement while a durable commit is in flight.
internal object WearVoiceSettingsIo {
    const val Capacity = 8
    val executor = java.util.concurrent.ThreadPoolExecutor(1, 1, 0L,
        java.util.concurrent.TimeUnit.MILLISECONDS, java.util.concurrent.ArrayBlockingQueue<Runnable>(Capacity),
        { work -> Thread(work, "wear-voice-settings").apply { isDaemon = true } },
        java.util.concurrent.ThreadPoolExecutor.AbortPolicy())
}
