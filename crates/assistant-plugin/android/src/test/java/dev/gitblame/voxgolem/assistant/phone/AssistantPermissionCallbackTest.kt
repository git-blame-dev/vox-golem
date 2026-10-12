package dev.gitblame.voxgolem.assistant.phone

import android.app.Activity
import app.tauri.plugin.Invoke
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AssistantPermissionCallbackTest {
    private fun invoke(id: Long, replies: MutableList<String>): Invoke {
        val response: (Long, String) -> Unit = { _, data -> replies += data }
        val mapper = Class.forName("com.fasterxml.jackson.databind.ObjectMapper").getDeclaredConstructor().newInstance()
        return Invoke::class.java.declaredConstructors.single().newInstance(id, "start_assistant_voice", 1L, 2L, response, "{}", mapper) as Invoke
    }
    private fun callback(plugin: AssistantPlugin, invoke: Invoke) {
        AssistantPlugin::class.java.getDeclaredMethod("assistantMicGranted", Invoke::class.java).apply { isAccessible = true }.invoke(plugin, invoke)
    }
    @Test fun retiredPermissionCallbackCannotConsumeANewerAdmission() {
        val activity = Robolectric.buildActivity(Activity::class.java).create().get()
        val plugin = AssistantPlugin(activity)
        val replies = mutableListOf<String>()
        val old = invoke(1, replies); val newer = invoke(2, replies)
        val type = AssistantPlugin::class.java.declaredClasses.single { it.simpleName == "PendingVoice" }
        val pending = type.getDeclaredConstructor(java.lang.Long.TYPE, VoiceArgs::class.java, Invoke::class.java, SpeechAdmissionSnapshot::class.java)
            .apply { isAccessible = true }.newInstance(2L, VoiceArgs(), newer,
                SpeechAdmissionSnapshot(SpeechInputOwner("synthetic-current-conversation", "synthetic-current-head"), "Synthetic owned draft"))
        val field = AssistantPlugin::class.java.getDeclaredField("pendingVoice").apply { isAccessible = true }
        field.set(plugin, pending)
        callback(plugin, old)
        assertSame(pending, field.get(plugin))
        assertTrue(replies.single().contains("cancelled"))
    }
    @Test fun aPermissionCallbackAfterCancellationCannotStartOrRestoreAnAdmission() {
        val activity = Robolectric.buildActivity(Activity::class.java).create().get()
        val plugin = AssistantPlugin(activity)
        val replies = mutableListOf<String>()
        callback(plugin, invoke(1, replies))
        assertTrue(replies.single().contains("cancelled"))
        assertNull(AssistantPlugin::class.java.getDeclaredField("pendingVoice").apply { isAccessible = true }.get(plugin))
    }
}
