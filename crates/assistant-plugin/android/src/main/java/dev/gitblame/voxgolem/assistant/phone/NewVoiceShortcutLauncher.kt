package dev.gitblame.voxgolem.assistant.phone

import android.app.Activity
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.ShortcutInfo
import android.content.pm.ShortcutManager
import android.graphics.drawable.Icon
import android.os.Build
import android.os.Bundle
import androidx.annotation.RequiresApi
import org.json.JSONObject
import java.util.UUID
import dev.gitblame.voxgolem.assistant.speech.VoiceLaunchProvenance

object NewVoiceShortcutLauncher {
    const val DISPATCH_ACTION = "dev.gitblame.voxgolem.NEW_VOICE_SHORTCUT"
    const val MAIN_ACTION = "dev.gitblame.voxgolem.NEW_VOICE_CHAT"
    const val TOKEN_EXTRA = "voxgolem.newVoiceToken"
    const val NOTIFICATION_EVENT = "voxgolem-new-voice-chat"
    private const val SHORTCUT_ID = "voxgolem-new-voice-chat"
    private const val SHORT_LABEL = "Ask"
    private const val LONG_LABEL = "Ask in current chat"
    val actions = ShortcutActionGate()
    @Volatile private var requested = false
    private var confirmationPending = false
    private var confirmationNotified = false
    @Synchronized fun requireConfirmation() { confirmationPending = true; confirmationNotified = false }
    @Synchronized fun hasConfirmation() = confirmationPending
    @Synchronized fun notifyConfirmation(): Boolean {
        if (!confirmationPending || confirmationNotified) return false
        confirmationNotified = true; return true
    }
    @Synchronized fun takeConfirmation(): Boolean {
        val pending = confirmationPending
        confirmationPending = false; confirmationNotified = false; return pending
    }
    @Synchronized fun cancelEntries() { confirmationPending = false; confirmationNotified = false; actions.background() }

    fun status(context: Context): JSONObject {
        if (Build.VERSION.SDK_INT < 26) return JSONObject().put("supported", false).put("pinned", false).put("requested", false)
        val manager = context.getSystemService(ShortcutManager::class.java)
        val supported = manager?.isRequestPinShortcutSupported == true
        val existing = if (supported) manager!!.pinnedShortcuts.firstOrNull { it.id == SHORTCUT_ID } else null
        val pinned = existing != null
        if (existing != null && (existing.shortLabel.toString() != SHORT_LABEL || existing.longLabel?.toString() != LONG_LABEL)) {
            runCatching { manager!!.updateShortcuts(listOf(shortcut(context))) }
        }
        return JSONObject().put("supported", supported).put("pinned", pinned).put("requested", requested && !pinned)
    }
    fun pin(context: Context): JSONObject {
        if (Build.VERSION.SDK_INT < 26) throw AssistantPhoneFailure("unavailable", "Pinned shortcuts require Android 8 or later")
        val manager = context.getSystemService(ShortcutManager::class.java)
        if (!manager.isRequestPinShortcutSupported) throw AssistantPhoneFailure("unavailable", "This launcher does not support pinned shortcuts")
        val info = shortcut(context)
        val confirmed = PendingIntent.getBroadcast(context, 0, Intent(context, NewVoiceShortcutPinnedReceiver::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE)
        requested = manager.requestPinShortcut(info, confirmed.intentSender)
        manager.updateShortcuts(listOf(info))
        if (!requested) throw AssistantPhoneFailure("unavailable", "The launcher did not accept the pin request")
        return status(context)
    }
    fun confirmed() { requested = false }
    @RequiresApi(25)
    private fun shortcut(context: Context): ShortcutInfo = ShortcutInfo.Builder(context, SHORTCUT_ID)
        .setShortLabel(SHORT_LABEL).setLongLabel(LONG_LABEL).setIcon(icon(context))
        .setIntent(Intent(context, NewVoiceShortcutActivity::class.java).setAction(DISPATCH_ACTION)).build()
    private fun icon(context: Context): Icon {
        val resource = context.resources.getIdentifier("voxgolem_new_voice_shortcut", "drawable", context.packageName)
        if (resource == 0) throw AssistantPhoneFailure("unavailable", "The generated voice shortcut artwork is missing from this build")
        return Icon.createWithResource(context, resource)
    }
}

/** No microphone, provider, history mutation or replay from saved state/Recents. */
open class NewVoiceShortcutActivity : Activity() {
    protected open fun launchOrigin() = VoiceLaunchProvenance.initial(this)
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null && intent.action == NewVoiceShortcutLauncher.DISPATCH_ACTION && intent.data == null &&
            intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY == 0) {
            if (launchOrigin() == VoiceLaunchProvenance.Origin.UNVERIFIED) {
                NewVoiceShortcutLauncher.requireConfirmation()
                packageManager.getLaunchIntentForPackage(packageName)?.let { main ->
                    main.action = NewVoiceShortcutLauncher.MAIN_ACTION
                    main.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
                    startActivity(main)
                }
                finish(); return
            }
            val token = UUID.randomUUID().toString()
            if (NewVoiceShortcutLauncher.actions.offer(token)) {
                packageManager.getLaunchIntentForPackage(packageName)?.let { main ->
                    main.action = NewVoiceShortcutLauncher.MAIN_ACTION
                    main.putExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA, token)
                    main.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
                    startActivity(main)
                } ?: NewVoiceShortcutLauncher.actions.background()
            } else packageManager.getLaunchIntentForPackage(packageName)?.let { startActivity(it) }
        }
        finish()
    }
}

class NewVoiceShortcutPinnedReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) { NewVoiceShortcutLauncher.confirmed() }
}
