package dev.gitblame.voxgolem.assistant.phone

import android.content.Intent
import android.content.ComponentName
import android.content.IntentFilter
import android.content.Context
import android.content.ContextWrapper
import android.content.IntentSender
import android.content.pm.ShortcutInfo
import android.content.pm.ShortcutManager
import android.content.res.Resources
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
import dev.gitblame.voxgolem.assistant.speech.VoiceLaunchProvenance

class SyntheticHomeVoiceShortcutActivity : NewVoiceShortcutActivity() {
    override fun launchOrigin() = VoiceLaunchProvenance.Origin.DEFAULT_HOME
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class NewVoiceShortcutLauncherTest {
    @After fun retireLaunch() { NewVoiceShortcutLauncher.cancelEntries(); NewVoiceShortcutLauncher.confirmed() }

    @Implements(ShortcutManager::class)
    class PinManager {
        var supported = true
        var pinned = emptyList<ShortcutInfo>()
        var offered: ShortcutInfo? = null
        var confirmation: IntentSender? = null
        val updates = mutableListOf<ShortcutInfo>()
        @Implementation(minSdk = 26) fun isRequestPinShortcutSupported() = supported
        @Implementation(minSdk = 25) fun getPinnedShortcuts() = pinned
        @Implementation(minSdk = 26) fun requestPinShortcut(info: ShortcutInfo, sender: IntentSender?): Boolean {
            offered = info; confirmation = sender; return true
        }
        @Implementation(minSdk = 25) fun updateShortcuts(values: List<ShortcutInfo>): Boolean { updates += values; return true }
    }

    private fun contextWithSyntheticBuildArtwork(): Context {
        val application = RuntimeEnvironment.getApplication()
        val original = application.resources
        val resources = object : Resources(original.assets, original.displayMetrics, original.configuration) {
            override fun getIdentifier(name: String?, defType: String?, defPackage: String?): Int =
                if (name == "voxgolem_new_voice_shortcut" && defType == "drawable") android.R.drawable.ic_input_add else 0
        }
        return object : ContextWrapper(application) { override fun getResources() = resources }
    }

    @Test @Config(sdk = [24]) fun preAndroidEightReportsUnsupportedWithoutLegacyPinSideEffects() {
        val status = NewVoiceShortcutLauncher.status(RuntimeEnvironment.getApplication())
        assertFalse(status.getBoolean("supported")); assertFalse(status.getBoolean("pinned"))
        try { NewVoiceShortcutLauncher.pin(RuntimeEnvironment.getApplication()); fail("must not use a legacy pin broadcast") }
        catch (failure: AssistantPhoneFailure) { assertEquals("unavailable", failure.code) }
    }

    @Test fun recreatedDispatcherCannotMintAnotherLaunchOrStartRecognition() {
        val intent = Intent(RuntimeEnvironment.getApplication(), NewVoiceShortcutActivity::class.java).setAction(NewVoiceShortcutLauncher.DISPATCH_ACTION)
        val controller = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java, intent).create(Bundle())
        assertTrue(controller.get().isFinishing)
        assertNull(NewVoiceShortcutLauncher.actions.consume())
        controller.destroy()
    }
    @Test fun missingGeneratedArtworkFailsInsteadOfDrawingAnotherLogoPalette() {
        val method = NewVoiceShortcutLauncher::class.java.getDeclaredMethod("icon", android.content.Context::class.java).apply { isAccessible = true }
        try { method.invoke(NewVoiceShortcutLauncher, RuntimeEnvironment.getApplication()); fail("the generated build asset is required") }
        catch (failure: java.lang.reflect.InvocationTargetException) {
            assertEquals("unavailable", (failure.cause as AssistantPhoneFailure).code)
        }
    }
    @Test @Config(sdk = [26], shadows = [PinManager::class]) fun pinRequestAndCallbackCannotFakeTheOsPinnedStatus() {
        val context = contextWithSyntheticBuildArtwork()
        val manager = context.getSystemService(ShortcutManager::class.java)
        val shadow = org.robolectric.shadow.api.Shadow.extract<PinManager>(manager)
        val requested = NewVoiceShortcutLauncher.pin(context)
        assertTrue(requested.getBoolean("supported")); assertTrue(requested.getBoolean("requested")); assertFalse(requested.getBoolean("pinned"))
        val info = shadow.offered!!
        assertEquals("Ask", info.shortLabel); assertEquals("Ask in current chat", info.longLabel)
        assertEquals(NewVoiceShortcutLauncher.DISPATCH_ACTION, info.intent!!.action)
        assertEquals(NewVoiceShortcutActivity::class.java.name, info.intent!!.component!!.className)
        assertNotNull(shadow.confirmation)
        NewVoiceShortcutPinnedReceiver().onReceive(context, Intent())
        assertFalse(NewVoiceShortcutLauncher.status(context).getBoolean("pinned"))
        shadow.pinned = listOf(info)
        assertTrue(NewVoiceShortcutLauncher.status(context).getBoolean("pinned"))
    }
    @Test @Config(sdk = [26], shadows = [PinManager::class]) fun unsupportedLauncherDoesNotReceiveAPinRequest() {
        val context = contextWithSyntheticBuildArtwork()
        val shadow = org.robolectric.shadow.api.Shadow.extract<PinManager>(context.getSystemService(ShortcutManager::class.java))
        shadow.supported = false
        try { NewVoiceShortcutLauncher.pin(context); fail("unsupported launcher must not be asked to pin") }
        catch (failure: AssistantPhoneFailure) { assertEquals("unavailable", failure.code) }
        assertNull(shadow.offered)
    }
    @Test @Config(sdk = [26], shadows = [PinManager::class]) fun oldPinnedLabelIsUpdatedWithoutChangingItsStableIdOrRePinning() {
        val context = contextWithSyntheticBuildArtwork()
        val shadow = org.robolectric.shadow.api.Shadow.extract<PinManager>(context.getSystemService(ShortcutManager::class.java))
        shadow.pinned = listOf(ShortcutInfo.Builder(context, "voxgolem-new-voice-chat").setShortLabel("Voice chat")
            .setLongLabel("New voice chat").setIntent(Intent(context, NewVoiceShortcutActivity::class.java).setAction(NewVoiceShortcutLauncher.DISPATCH_ACTION)).build())
        assertTrue(NewVoiceShortcutLauncher.status(context).getBoolean("pinned"))
        assertNull(shadow.offered)
        val updated = shadow.updates.single()
        assertEquals("voxgolem-new-voice-chat", updated.id)
        assertEquals("Ask", updated.shortLabel)
        assertEquals(NewVoiceShortcutActivity::class.java.name, updated.intent!!.component!!.className)
    }
    @Test fun deliberateDispatchMintsFreshTokensAndRapidTapsOnlyReopenTheSameMainApp() {
        val application = RuntimeEnvironment.getApplication()
        val main = ComponentName(application.packageName, android.app.Activity::class.java.name)
        shadowOf(application.packageManager).addActivityIfNotPresent(main)
        shadowOf(application.packageManager).addIntentFilterForActivity(main, IntentFilter(Intent.ACTION_MAIN).apply { addCategory(Intent.CATEGORY_LAUNCHER) })
        assertNotNull(application.packageManager.getLaunchIntentForPackage(application.packageName))
        fun dispatch(): Intent {
            val controller = Robolectric.buildActivity(SyntheticHomeVoiceShortcutActivity::class.java,
                Intent(application, NewVoiceShortcutActivity::class.java).setAction(NewVoiceShortcutLauncher.DISPATCH_ACTION)).create()
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
            Intent().setAction(NewVoiceShortcutLauncher.DISPATCH_ACTION).addFlags(Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY),
            Intent().setAction("synthetic.foreign.ACTION").putExtra("prompt", "Ignored synthetic incoming text"),
        )) {
            val controller = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java, intent).create()
            assertTrue(controller.get().isFinishing)
            assertNull(NewVoiceShortcutLauncher.actions.consume())
            controller.destroy()
        }
    }

    @Test fun freshExpectedActionFromUnverifiedOriginNavigatesWithoutMintingAnAutomaticVoiceToken() {
        val application = RuntimeEnvironment.getApplication()
        val main = ComponentName(application.packageName, android.app.Activity::class.java.name)
        shadowOf(application.packageManager).addActivityIfNotPresent(main)
        shadowOf(application.packageManager).addIntentFilterForActivity(main, IntentFilter(Intent.ACTION_MAIN).apply { addCategory(Intent.CATEGORY_LAUNCHER) })
        val controller = Robolectric.buildActivity(NewVoiceShortcutActivity::class.java,
            Intent(application, NewVoiceShortcutActivity::class.java).setAction(NewVoiceShortcutLauncher.DISPATCH_ACTION)
                .putExtra("trusted", true).putExtra(Intent.EXTRA_REFERRER_NAME, "android-app://synthetic.home")).create()
        try {
            val navigation = shadowOf(controller.get()).nextStartedActivity
            assertEquals(main, navigation.component)
            assertFalse(navigation.hasExtra(NewVoiceShortcutLauncher.TOKEN_EXTRA))
            assertNull(NewVoiceShortcutLauncher.actions.consume())
            assertFalse(NewVoiceShortcutLauncher.actions.hasAction())
            assertTrue(NewVoiceShortcutLauncher.takeConfirmation())
            assertFalse(NewVoiceShortcutLauncher.takeConfirmation())
        } finally { controller.destroy() }
    }
}
