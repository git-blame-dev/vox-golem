package dev.gitblame.voxgolem.assistant.phone

import android.content.Intent
import android.content.ComponentName
import android.content.IntentFilter
import android.content.pm.ShortcutManager
import android.os.Bundle
import org.junit.After
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class NewVoiceShortcutLauncherTest {
    @After fun retireLaunch() { NewVoiceShortcutLauncher.cancelEntries() }

    @Implements(ShortcutManager::class)
    class DisableManager {
        val disabled = mutableListOf<List<String>>()
        @Implementation(minSdk = 25) fun disableShortcuts(ids: List<String>, message: CharSequence?) { disabled += ids }
    }

    @Test @Config(sdk = [26], shadows = [DisableManager::class]) fun earlierPinnedShortcutIsDisabledByStableId() {
        val context = RuntimeEnvironment.getApplication()
        NewVoiceShortcutLauncher.retirePinnedShortcut(context)
        val shadow = org.robolectric.shadow.api.Shadow.extract<DisableManager>(context.getSystemService(ShortcutManager::class.java))
        assertEquals(listOf(listOf("voxgolem-new-voice-chat")), shadow.disabled)
    }
    @Test @Config(sdk = [24]) fun preAndroidSevenRetirementIsANoOp() { NewVoiceShortcutLauncher.retirePinnedShortcut(RuntimeEnvironment.getApplication()) }

    @Test fun recreatedDispatcherCannotMintAnotherLaunchOrStartRecognition() {
        val application = RuntimeEnvironment.getApplication()
        val intent = Intent(Intent.ACTION_MAIN).setClassName(application.packageName, "${application.packageName}.AssistantVoiceEntry").addCategory(Intent.CATEGORY_LAUNCHER)
        val controller = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java, intent).create(Bundle())
        assertTrue(controller.get().isFinishing)
        assertNull(NewVoiceShortcutLauncher.actions.consume())
        controller.destroy()
    }
    @Test fun launcherAliasesStartVoiceWithoutCallerConfirmationAndAlwaysRouteToChatActivity() {
        val application = RuntimeEnvironment.getApplication()
        // Several launcher components exist; package launch-intent resolution must not choose Ask again.
        val alias = ComponentName(application.packageName, "${application.packageName}.AssistantVoiceEntry")
        shadowOf(application.packageManager).addActivityIfNotPresent(alias)
        shadowOf(application.packageManager).addIntentFilterForActivity(alias, IntentFilter(Intent.ACTION_MAIN).apply { addCategory(Intent.CATEGORY_LAUNCHER) })
        val controller = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java,
            Intent(Intent.ACTION_MAIN).setComponent(alias).addCategory(Intent.CATEGORY_LAUNCHER)).create()
        try {
            val navigation = shadowOf(controller.get()).nextStartedActivity
            assertNotNull(navigation)
            assertEquals(ComponentName(application.packageName, "${application.packageName}.MainActivity"), navigation.component)
            val token = navigation.getStringExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA)!!
            assertEquals(token, NewVoiceShortcutLauncher.actions.consume())
            assertEquals(VoiceEntryMode.ASK, NewVoiceShortcutLauncher.actions.mode(token))
        } finally { controller.destroy() }
    }
    @Test fun deliberateAliasLaunchMintsFreshTokensAndRapidTapsOnlyReopenTheSameMainApp() {
        val application = RuntimeEnvironment.getApplication()
        val main = ComponentName(application.packageName, "${application.packageName}.MainActivity")
        shadowOf(application.packageManager).addActivityIfNotPresent(main)
        shadowOf(application.packageManager).addIntentFilterForActivity(main, IntentFilter(Intent.ACTION_MAIN).apply { addCategory(Intent.CATEGORY_LAUNCHER) })
        assertNotNull(application.packageManager.getLaunchIntentForPackage(application.packageName))
        val ASK_ALIAS = Intent(Intent.ACTION_MAIN).setClassName(application.packageName, "${application.packageName}.AssistantVoiceEntry").addCategory(Intent.CATEGORY_LAUNCHER)
        fun dispatch(): Intent {
            val controller = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java,
                ASK_ALIAS).create()
            assertTrue(controller.get().isFinishing)
            return shadowOf(controller.get()).nextStartedActivity.also { controller.destroy() }
        }
        val first = dispatch()
        val token = first.getStringExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA)!!
        assertTrue(ShortcutActionGate.valid(token)); assertEquals(main, first.component)
        assertEquals(NewVoiceShortcutLauncher.MAIN_ACTION, first.action)
        assertEquals(token, NewVoiceShortcutLauncher.actions.consume()); assertNull(NewVoiceShortcutLauncher.actions.consume())
        assertFalse(dispatch().hasExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA))
        NewVoiceShortcutLauncher.actions.release(token)
        val next = dispatch().getStringExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA)!!
        assertNotEquals(token, next); assertEquals(next, NewVoiceShortcutLauncher.actions.consume())
    }

    @Test fun recentsReplayAndForeignActionsCannotMintLaunchTokens() {
        for (intent in listOf(
            Intent(Intent.ACTION_MAIN).setClassName(RuntimeEnvironment.getApplication().packageName, "${RuntimeEnvironment.getApplication().packageName}.AssistantVoiceEntry").addCategory(Intent.CATEGORY_LAUNCHER).addFlags(Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY),
            Intent().setAction("synthetic.foreign.ACTION").putExtra("prompt", "Ignored synthetic incoming text"),
        )) {
            val controller = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java, intent).create()
            assertTrue(controller.get().isFinishing)
            assertNull(NewVoiceShortcutLauncher.actions.consume())
            controller.destroy()
        }
    }

    @Test fun talkAliasCarriesRepeatPolicyAndDoesNotReplacePendingAsk() {
        val application = RuntimeEnvironment.getApplication()
        val talk = Intent(Intent.ACTION_MAIN).setClassName(application.packageName, "${application.packageName}.AssistantConversationEntry")
            .addCategory(Intent.CATEGORY_LAUNCHER).putExtra("mode", "ask").putExtra("prompt", "Ignored synthetic text")
        val controller = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java, talk).create()
        val token = shadowOf(controller.get()).nextStartedActivity.getStringExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA)!!
        assertEquals(VoiceEntryMode.TALK, NewVoiceShortcutLauncher.actions.mode(token))
        controller.destroy()
        NewVoiceShortcutLauncher.cancelEntries()
        val askToken = "33333333-3333-4333-8333-333333333333"
        assertTrue(NewVoiceShortcutLauncher.actions.offer(askToken))
        val duplicate = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java, talk).create()
        assertFalse(shadowOf(duplicate.get()).nextStartedActivity.hasExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA))
        assertEquals(askToken, NewVoiceShortcutLauncher.actions.consume())
        assertEquals(VoiceEntryMode.ASK, NewVoiceShortcutLauncher.actions.mode(askToken))
        duplicate.destroy()
    }
    @Test fun malformedOrReplayedLauncherAliasesCannotOfferVoice() {
        val application = RuntimeEnvironment.getApplication()
        val valid = Intent(Intent.ACTION_MAIN).setClassName(application.packageName, "${application.packageName}.AssistantConversationEntry")
            .addCategory(Intent.CATEGORY_LAUNCHER)
        val malformed = listOf(
            Intent(valid).apply { removeCategory(Intent.CATEGORY_LAUNCHER) },
            Intent(valid).setAction("synthetic.foreign.ACTION"),
            Intent(valid).setData(android.net.Uri.parse("https://synthetic.example/voice")),
            Intent(valid).addFlags(Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY),
            Intent(valid).setClassName(application.packageName, "${application.packageName}.MainActivity"),
            Intent(valid).setClassName("synthetic.foreign", "synthetic.foreign.AssistantVoiceEntry"),
        )
        for (intent in malformed) {
            val controller = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java, intent).create()
            assertNull(shadowOf(controller.get()).nextStartedActivity)
            assertNull(NewVoiceShortcutLauncher.actions.consume())
            controller.destroy()
        }
        val restored = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java, valid).create(Bundle())
        assertNull(NewVoiceShortcutLauncher.actions.consume())
        assertNull(shadowOf(restored.get()).nextStartedActivity)
        restored.destroy()
    }
}
