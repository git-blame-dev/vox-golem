package dev.gitblame.voxgolem.android.wear

import android.content.ComponentName
import android.content.Intent
import dev.gitblame.voxgolem.android.wear.assistant.AssistantActivity
import dev.gitblame.voxgolem.android.wear.assistant.AssistantTextEntryActivity
import dev.gitblame.voxgolem.android.wear.speechtest.NativeTtsHostActivity
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Robolectric
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class LauncherManifestTest {
    @Test fun onlyTheFourUserEntryPointsAreLaunchableAndKeyboardRemainsInternal() {
        val context = RuntimeEnvironment.getApplication()
        val launch = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER).setPackage(context.packageName)
        val entries = context.packageManager.queryIntentActivities(launch, 0).map { it.activityInfo.name }.toSet()
        assertEquals(setOf(CaptureActivity::class.java.name, AssistantActivity::class.java.name,
            AssistantActivity.VOICE_ENTRY, AssistantActivity.CONVERSATION_ENTRY), entries)
        val editor = context.packageManager.getActivityInfo(ComponentName(context, AssistantTextEntryActivity::class.java), 0)
        assertFalse(editor.exported)
    }

    @Test fun nativeTtsRegressionHostIsInternalAndCanStartWithoutTheDiagnosticFeature() {
        val context = RuntimeEnvironment.getApplication()
        val host = context.packageManager.getActivityInfo(ComponentName(context, NativeTtsHostActivity::class.java), 0)
        assertFalse(host.exported)
        val controller = Robolectric.buildActivity(NativeTtsHostActivity::class.java).setup()
        try {
            assertFalse(controller.get().isFinishing)
            assertFalse(controller.get().isDestroyed)
        } finally { controller.pause().stop().destroy() }
    }
}
