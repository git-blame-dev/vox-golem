package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Context
import android.content.SharedPreferences

internal data class WearVoicePreferences(val readRepliesAloud: Boolean = true, val conversationMode: Boolean = true)
internal class WearVoiceSettings(private val prefs: SharedPreferences) {
    constructor(context: Context) : this(context.getSharedPreferences("assistant-runtime", Context.MODE_PRIVATE))
    fun read(): WearVoicePreferences {
        val missing = listOf("read-replies-aloud", "conversation-mode").filterNot(prefs::contains)
        if (missing.isNotEmpty()) {
            val editor = prefs.edit()
            missing.forEach { editor.putBoolean(it, true) }
            check(editor.commit()) { "Could not save initial voice preferences" }
        }
        return current()
    }
    fun current(): WearVoicePreferences = WearVoicePreferences(
        prefs.getBoolean("read-replies-aloud", true), prefs.getBoolean("conversation-mode", true))
    fun setReadRepliesAloud(enabled: Boolean) {
        check(prefs.edit().putBoolean("read-replies-aloud", enabled).commit()) { "Could not save read-replies-aloud" }
    }
    fun setConversationMode(enabled: Boolean) {
        check(prefs.edit().putBoolean("conversation-mode", enabled).commit()) { "Could not save conversation-mode" }
    }
}
