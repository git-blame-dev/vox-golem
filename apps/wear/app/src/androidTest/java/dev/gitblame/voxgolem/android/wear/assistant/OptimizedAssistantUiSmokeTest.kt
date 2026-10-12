package dev.gitblame.voxgolem.android.wear.assistant

import android.app.Activity
import android.content.ComponentName
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.view.WindowManager
import android.view.accessibility.AccessibilityNodeInfo
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class OptimizedAssistantUiSmokeTest {
    @Test(timeout = 180_000) fun optInOptimizedAppSavesOneReplyThroughNativeUiOnly() {
        assumeTrue("Live provider execution requires explicit opt-in",
            InstrumentationRegistry.getArguments().getString("liveProvider") == "true")
        assertTrue("This live-provider UI smoke is emulator-only",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish")
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val component = ComponentName("dev.gitblame.voxgolem.android",
            "dev.gitblame.voxgolem.android.wear.assistant.AssistantActivity")
        val launch = Intent().setComponent(component).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val prompt = "Reply exactly: OPT_WEAR_OK"
        var originalKeepScreenOn = false
        ActivityScenario.launch<Activity>(launch).use { scenario ->
            try {
                scenario.onActivity { activity ->
                    assertEquals(component, activity.componentName)
                    originalKeepScreenOn = activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON != 0
                    activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                }
                await("The optimized app must render its native chat without the relocated New target", 65_000) {
                    val visible = nodes().filter { it.isVisibleToUser }
                    visible.any { it.contentDescription?.toString()?.startsWith("Mic:") == true ||
                        it.contentDescription?.toString() == "Send draft" } &&
                        visible.none { it.contentDescription?.toString() == "New chat" || it.text?.toString() == "New chat" }
                }
                openSettings()
                click("New chat")
                await("New chat must expose an empty conversation rather than previous user content") {
                    nodes().any { it.isVisibleToUser && it.text?.toString() == "No messages yet. Ask a question." }
                }
                instrumentation.uiAutomation.waitForIdle(250, 5_000)
                openSettings()
                click("Keyboard")
                var editor: AccessibilityNodeInfo? = null
                await("The existing native editor must expose its platform EditText") {
                    editor = nodes().firstOrNull { it.isVisibleToUser && it.className?.toString() == "android.widget.EditText" }
                    editor != null
                }
                assertTrue("The new synthetic chat must not prefill a previous draft", editor!!.text.isNullOrEmpty())
                val textArguments = Bundle().apply {
                    putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, prompt)
                }
                assertTrue("The native editor must accept the full synthetic prompt through ACTION_SET_TEXT",
                    editor!!.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, textArguments))
                await("Native Editable must retain the entire synthetic prompt before confirmation") {
                    editor = nodes().firstOrNull { it.isVisibleToUser && it.className?.toString() == "android.widget.EditText" }
                    editor?.text?.toString() == prompt
                }
                assertTrue("The native IME Done action must confirm only the owned draft",
                    editor!!.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.id))
                await("Confirmation must return to the main draft without automatically sending") {
                    val visible = nodes().filter { it.isVisibleToUser }
                    visible.any { it.text?.toString() == prompt } &&
                        visible.none { it.text?.toString() == "Settings" } &&
                        visible.any { it.contentDescription?.toString() == "Send draft" }
                }
                SystemClock.sleep(500)
                val draftUi = nodes().filter { it.isVisibleToUser }
                assertTrue("The synthetic draft must remain unsent until the explicit Send edge action",
                    draftUi.any { it.text?.toString() == prompt } &&
                        draftUi.any { it.contentDescription?.toString() == "Send draft" } &&
                        draftUi.none { it.text?.toString() == "OPT_WEAR_OK" || isUnsettled(it) })
                click("Send draft", description = true)
                await("The optimized native UI must show the exact settled synthetic reply within ninety seconds", 90_000) {
                    val visible = nodes().filter { it.isVisibleToUser }
                    visible.any { it.text?.toString()?.trimEnd('\r', '\n') == "OPT_WEAR_OK" } &&
                        visible.none(::isUnsettled) &&
                        visible.any { it.contentDescription?.toString()?.startsWith("Mic:") == true } &&
                        visible.none { it.contentDescription?.toString() == "Send draft" }
                }
            } finally {
                scenario.onActivity { activity ->
                    if (originalKeepScreenOn) activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                    else activity.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                }
            }
        }
        instrumentation.sendStatus(0, Bundle().apply {
            putString("stream", "\nOPT_WEAR_OK | PASS optimized-ui-only new-chat=created draft=explicit-send reply=settled mic-edge=returned\n")
        })
    }

    private fun isUnsettled(node: AccessibilityNodeInfo): Boolean =
        node.contentDescription?.toString() == "Stop microphone, response and playback"

    private fun openSettings() {
        var action: Pair<AccessibilityNodeInfo, Int>? = null
        await("Chat exposes the Settings navigation accessibility action") {
            action = nodes().filter { it.isVisibleToUser }.firstNotNullOfOrNull { node ->
                node.actionList.firstOrNull { it.label?.toString() == "Open Settings" }?.let { node to it.id }
            }
            action != null
        }
        assertTrue(action!!.first.performAction(action!!.second))
        await("Settings is visible") { nodes().any { it.isVisibleToUser && it.text?.toString() == "Settings" } }
    }

    private fun nodes(): List<AccessibilityNodeInfo> {
        val result = mutableListOf<AccessibilityNodeInfo>()
        fun visit(node: AccessibilityNodeInfo) {
            result += node
            for (index in 0 until node.childCount) node.getChild(index)?.let(::visit)
        }
        InstrumentationRegistry.getInstrumentation().uiAutomation.rootInActiveWindow?.let(::visit)
        return result
    }

    private fun click(label: String, description: Boolean = false) {
        var target: AccessibilityNodeInfo? = null
        await("The native $label control must be visible and enabled") {
            target = nodes().firstOrNull { node ->
                node.isVisibleToUser && (if (description) node.contentDescription else node.text)?.toString() == label
            }?.let { node ->
                var ancestor = node
                while (!ancestor.isClickable && ancestor.parent != null) ancestor = ancestor.parent
                ancestor.takeIf { it.isClickable && it.isEnabled }
            }
            if (target == null && label in listOf("Keyboard", "New chat")) nodes().lastOrNull { it.isVisibleToUser && it.isScrollable }
                ?.performAction(AccessibilityNodeInfo.ACTION_SCROLL_FORWARD)
            target != null
        }
        assertTrue("The native $label control must accept its explicit click", target!!.performAction(AccessibilityNodeInfo.ACTION_CLICK))
    }

    private fun await(message: String, timeoutMillis: Long = 15_000, condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + timeoutMillis
        while (!condition()) {
            assertTrue(message, SystemClock.elapsedRealtime() < deadline)
            SystemClock.sleep(50)
        }
    }
}
