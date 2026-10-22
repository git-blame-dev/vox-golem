package dev.gitblame.voxgolem.assistant.phone

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.ShortcutManager
import android.os.Build
import android.os.Bundle
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import java.util.UUID

object NewVoiceShortcutLauncher {
    const val MAIN_ACTION = "dev.gitblame.voxgolem.NEW_VOICE_CHAT"
    const val TOKEN_EXTRA = "voxgolem.newVoiceToken"
    const val NOTIFICATION_EVENT = "voxgolem-new-voice-chat"
    private const val RETIRED_PINNED_SHORTCUT_ID = "voxgolem-new-voice-chat"
    val actions = ShortcutActionGate()
    fun cancelEntries() { actions.background() }

    fun entryMode(context: Context, intent: Intent): VoiceEntryMode? {
        if (intent.data != null || intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0) return null
        val component = intent.component ?: return null
        if (component.packageName != context.packageName) return null
        if (intent.action != Intent.ACTION_MAIN || !intent.hasCategory(Intent.CATEGORY_LAUNCHER)) return null
        return when (component.className) {
            "${context.packageName}.AssistantVoiceEntry" -> VoiceEntryMode.ASK
            "${context.packageName}.AssistantConversationEntry" -> VoiceEntryMode.TALK
            else -> null
        }
    }

    // With multiple launcher aliases, getLaunchIntentForPackage may resolve back to a voice dispatcher.
    fun mainIntent(context: Context): Intent = Intent(Intent.ACTION_MAIN)
        .setClassName(context.packageName, "${context.packageName}.MainActivity")
        .addCategory(Intent.CATEGORY_LAUNCHER)
        .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)

    /** Home-screen pinning is no longer offered; disable any shortcut pinned by an earlier build so it cannot dispatch a dead action. */
    fun retirePinnedShortcut(context: Context) {
        if (Build.VERSION.SDK_INT < 25) return
        runCatching {
            context.getSystemService(ShortcutManager::class.java)
                ?.disableShortcuts(listOf(RETIRED_PINNED_SHORTCUT_ID), "Use the Vox Ask launcher entry instead")
        }
    }
}

/** Explicit exported voice entries may be invoked by other apps; microphone permission still applies. */
open class NewVoiceShortcutActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        reportVoiceEntryStage(this, Stage.ENTRY_REQUESTED)
        val mode = if (savedInstanceState == null) NewVoiceShortcutLauncher.entryMode(this, intent) else null
        if (mode != null) {
            val token = UUID.randomUUID().toString()
            if (NewVoiceShortcutLauncher.actions.offer(token, mode)) {
                reportVoiceEntryStage(this, Stage.ENTRY_OFFERED)
                try {
                    val main = NewVoiceShortcutLauncher.mainIntent(this)
                    main.action = NewVoiceShortcutLauncher.MAIN_ACTION
                    main.putExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA, token)
                    startActivity(main)
                } catch (error: RuntimeException) { NewVoiceShortcutLauncher.cancelEntries(); throw error }
            } else {
                reportVoiceEntryStage(this, Stage.ENTRY_BUSY)
                startActivity(NewVoiceShortcutLauncher.mainIntent(this))
            }
        } else reportVoiceEntryStage(this, Stage.ENTRY_REJECTED)
        finish()
    }
}
