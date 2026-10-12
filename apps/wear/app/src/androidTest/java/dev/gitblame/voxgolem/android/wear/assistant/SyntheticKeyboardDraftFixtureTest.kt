package dev.gitblame.voxgolem.android.wear.assistant

import android.app.Activity
import android.Manifest
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.Rect
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.WindowInsets
import android.view.WindowManager
import android.view.accessibility.AccessibilityNodeInfo
import android.widget.Button
import android.widget.EditText
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.runner.lifecycle.ActivityLifecycleMonitorRegistry
import androidx.test.runner.lifecycle.Stage
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.runtime.AssistantEngine
import dev.gitblame.voxgolem.assistant.runtime.AssistantModels
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.util.UUID
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

@RunWith(AndroidJUnit4::class)
class SyntheticKeyboardDraftFixtureTest {
    @Test(timeout = 120_000) fun keyboardConfirmationRetainsTheEntireLineWithoutSendingAndCancellationKeepsTheDraft() {
        assertTrue("This persistent synthetic UI fixture is emulator-only",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish")
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val target = instrumentation.targetContext.applicationContext
        assertEquals("dev.gitblame.voxgolem.android", target.packageName)
        val engine = AssistantRuntime.get(target)
        val initial = settled { engine.refresh(it) }
        assertTrue("Do not seed over an active or unhealthy runtime", !initial.busy && initial.error == null)
        val title = "Synthetic Wear keyboard ${UUID.randomUUID()}"
        val prefill = "Synthetic keyboard confirmation must retain every word including this complete prefilled sentence "
        var fullLine = prefill
        val discarded = "Synthetic canceled edit must not replace the confirmed draft."
        val windows = mutableMapOf<Activity, Boolean>()
        val fixtureName = "synthetic-ime-${UUID.randomUUID()}"

        AssistantStore(target).use { store ->
            val before = store.snapshot()
            val previousRecords = recordKeys(before.getJSONArray("records"))
            val previousRetired = before.getJSONArray("retired").toString()
            val previousSelection = store.localState()
            val previousConversation = if (previousSelection.isNull("conversation")) null
                else previousSelection.getJSONObject("conversation")
            val previousDraft = store.draft(previousConversation).toString()
            val newDraft = store.draft(null).toString()
            val conversation = store.createConversation(title).getJSONObject("operation").getJSONObject("id")
            assertFixture(settled { engine.selectConversation(conversation.toString(), it) }, conversation)
            assertDraft(settled { engine.setDraft(prefill, it) }, conversation, prefill)

            val launch = Intent(target, AssistantActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            ActivityScenario.launch<AssistantActivity>(launch).use { scenario ->
                try {
                scenario.onActivity {
                    windows[it] = it.window.attributes.flags and WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON != 0
                    it.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                }
                await("The actual assistant activity must resume the synthetic conversation") {
                    engine.snapshot().foreground && matchesFixture(engine.snapshot(), conversation)
                }
                scenario.onActivity { assertEquals(AssistantActivity::class.java, it.javaClass) }
                 openKeyboardFromSettings()
                val editor = resumedEditor()
                instrumentation.runOnMainSync {
                    windows[editor] = editor.window.attributes.flags and WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON != 0
                    editor.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                    val input = editor.findViewById<EditText>(android.R.id.edit)
                    assertTrue("The native editor must prefill the entire synthetic draft", input.text.toString() == prefill)
                    input.setSelection(input.text.length)
                }
                await("Opening Keyboard must show the actual software IME without another editor tap") {
                    var visible = false
                    instrumentation.runOnMainSync { visible = editor.window.decorView.rootWindowInsets?.isVisible(WindowInsets.Type.ime()) == true }
                    visible
                }
                saveScreenshot(File(requireNotNull(target.externalCacheDir), "$fixtureName-visible.png"))
                val width = target.resources.displayMetrics.widthPixels
                val height = target.resources.displayMetrics.heightPixels
                val down = SystemClock.uptimeMillis()
                // The named Wear emulator's verified Gboard X key is in the lower alphabet row.
                listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_UP).forEach { action ->
                    val event = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, width * .278f, height * .75f, 0)
                    try { assertTrue("The real software keyboard must receive its key tap", instrumentation.uiAutomation.injectInputEvent(event, true)) }
                    finally { event.recycle() }
                }
                await("A real software key must append exactly one character to the complete draft") {
                    var typed = ""
                    instrumentation.runOnMainSync { typed = editor.findViewById<EditText>(android.R.id.edit).text.toString() }
                    typed.length == prefill.length + 1
                }
                instrumentation.runOnMainSync {
                    fullLine = editor.findViewById<EditText>(android.R.id.edit).text.toString()
                    assertTrue("The real key must preserve every prefilled word and append X", fullLine.startsWith(prefill) && fullLine.last().equals('x', true))
                }
                instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
                await("Native Back must dismiss only the fullscreen Wear keyboard, not the editor") {
                    var hidden = false
                    instrumentation.runOnMainSync { hidden = !editor.isFinishing && editor.window.decorView.rootWindowInsets?.isVisible(WindowInsets.Type.ime()) == false }
                    hidden
                }
                instrumentation.runOnMainSync {
                    val useText = editor.findViewById<Button>(android.R.id.button1)
                    assertEquals("Use text", useText.text.toString())
                    assertTrue("Use text must be visible after native keyboard dismissal", useText.isShown)
                    assertTrue("The existing Use text button must dispatch confirmation", useText.performClick())
                }
                 await("Use text must return the exact complete draft to the main engine") {
                     matchesFixture(engine.snapshot(), conversation) && engine.snapshot().foreground && engine.snapshot().draft == fullLine
                 }
                 await("Editor confirmation returns to Chat, not the Settings launch page") {
                     currentNodes().none { it.isVisibleToUser && it.text?.toString() == "Settings" } &&
                         currentNodes().any { it.isVisibleToUser && it.contentDescription?.toString() == "Send draft" }
                 }
                assertDraft(settled { engine.refresh(it) }, conversation, fullLine)
                assertNoSend(engine, store, conversation)
                saveScreenshot(File(requireNotNull(target.externalCacheDir), "$fixtureName-confirmed.png"))

                 openKeyboardFromSettings()
                val canceledEditor = resumedEditor()
                instrumentation.runOnMainSync {
                    val input = canceledEditor.findViewById<EditText>(android.R.id.edit)
                    assertTrue("Reopening Keyboard must prefill the previously confirmed complete line", input.text.toString() == fullLine)
                    input.setText(discarded)
                    // Finishing without Use text returns the platform's default RESULT_CANCELED.
                    canceledEditor.finish()
                }
                 await("Canceling the native editor must return to the actual assistant activity") {
                     isResumed(AssistantActivity::class.java) && engine.snapshot().foreground
                 }
                 await("Editor cancellation also returns to Chat with the unsent draft") {
                     currentNodes().none { it.isVisibleToUser && it.text?.toString() == "Settings" } &&
                         currentNodes().any { it.isVisibleToUser && it.contentDescription?.toString() == "Send draft" }
                 }
                assertDraft(settled { engine.refresh(it) }, conversation, fullLine)
                assertNoSend(engine, store, conversation)
                assertTrue("Only the new synthetic conversation's draft may be changed",
                    store.draft(previousConversation).toString() == previousDraft && store.draft(null).toString() == newDraft)
                val after = store.snapshot()
                assertEquals(1, after.getJSONArray("records").length() - before.getJSONArray("records").length())
                assertTrue("All previous immutable history must remain unchanged",
                    recordKeys(after.getJSONArray("records")).entries.containsAll(previousRecords.entries))
                assertTrue("The retired ledger must remain unchanged", after.getJSONArray("retired").toString() == previousRetired)
                } finally {
                    instrumentation.runOnMainSync {
                        windows.forEach { (activity, wasSet) ->
                            if (wasSet) activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                            else activity.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                        }
                    }
                }
            }
        }
        instrumentation.sendStatus(0, Bundle().apply {
            putString("stream", "\n$title | PASS software-ime=visible actual-key=X prefill=exact confirmation=full-line cancellation=preserved auto-send=false screenshots=$fixtureName-{visible,confirmed}.png\n")
        })
    }

    @Test(timeout = 180_000) fun liveProviderTwoTurnRepliesAreDurableAndCausallyLinked() {
        assumeTrue("Live provider execution requires explicit opt-in",
            InstrumentationRegistry.getArguments().getString("liveProvider") == "true")
        assertTrue("This persistent live-provider fixture is emulator-only",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish")
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val target = instrumentation.targetContext.applicationContext
        assertEquals("dev.gitblame.voxgolem.android", target.packageName)
        val engine = AssistantRuntime.get(target)
        val initial = settled { engine.refresh(it) }
        assertTrue("Do not send over an active or unhealthy runtime", !initial.busy && initial.error == null)
        val title = "Synthetic Wear live WATCH_EMULATOR_OK ${UUID.randomUUID()}"
        AssistantStore(target).use { store ->
            val before = store.snapshot()
            val oldRecords = recordKeys(before.getJSONArray("records"))
            val oldRetired = before.getJSONArray("retired").toString()
            val local = store.localState()
            val oldConversation = if (local.isNull("conversation")) null else local.getJSONObject("conversation")
            val oldDraft = store.draft(oldConversation).toString()
            val newDraft = store.draft(null).toString()
            val conversation = store.createConversation(title).getJSONObject("operation").getJSONObject("id")
            assertFixture(settled { engine.selectConversation(conversation.toString(), it) }, conversation)
            val launch = Intent(target, AssistantActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            ActivityScenario.launch<AssistantActivity>(launch).use { scenario ->
                scenario.onActivity { assertEquals(AssistantActivity::class.java, it.javaClass) }
                await("The actual main assistant must resume before live input") {
                    matchesFixture(engine.snapshot(), conversation) && engine.snapshot().foreground
                }
                val first = sendLiveUiTurn(engine, store, conversation, null, "Reply exactly: WATCH_EMULATOR_OK")
                val firstId = first.getJSONObject("operation").getJSONObject("id")
                val second = sendLiveUiTurn(engine, store, conversation, firstId, "Repeat your previous reply exactly.")
                val secondId = second.getJSONObject("operation").getJSONObject("id")
                assertTrue("The two durable completion IDs must be distinct", !sameId(firstId, secondId))
                val firstRequest = first.getJSONObject("operation").getJSONObject("body").getJSONObject("request")
                val secondRequest = second.getJSONObject("operation").getJSONObject("body").getJSONObject("request")
                assertTrue("The two actual provider requests must have distinct causal identities", !sameId(firstRequest, secondRequest))
                assertTrue("The second saved completion must include exactly the first completion in its pinned context",
                    second.getJSONObject("operation").getJSONObject("body").getJSONObject("provenance")
                        .getJSONArray("context").let { it.length() == 1 && sameId(it.getJSONObject(0), firstId) })
                AssistantStore(target).use { restored ->
                    val turns = restored.context(conversation, secondId).getJSONArray("turns")
                    assertEquals(2, turns.length())
                    assertTrue("Both canonical replies must restore from the actual SQLite store",
                        turns.getJSONObject(0).getString("body_key") == first.getString("body_key") &&
                            turns.getJSONObject(1).getString("body_key") == second.getString("body_key"))
                }
                assertTrue("Live sends must preserve the previous conversation and new-conversation drafts",
                    store.draft(oldConversation).toString() == oldDraft && store.draft(null).toString() == newDraft)
                val after = store.snapshot()
                assertTrue("Every previous canonical record must remain unchanged",
                    recordKeys(after.getJSONArray("records")).entries.containsAll(oldRecords.entries))
                assertTrue("The previous retired ledger must remain unchanged", after.getJSONArray("retired").toString() == oldRetired)
                assertTrue("The fixture must remain identifiably titled for normal synchronization",
                    (0 until after.getJSONArray("records").length()).any { index ->
                        val operation = after.getJSONArray("records").getJSONObject(index).getJSONObject("operation")
                        sameId(operation.getJSONObject("id"), conversation) && operation.getJSONObject("body").getString("title") == title
                    })
            }
        }
        instrumentation.sendStatus(0, Bundle().apply {
            putString("stream", "\n$title | PASS marker=WATCH_EMULATOR_OK saved=2 request-correlation=proven parent=first-completion durability=restored\n")
        })
    }

    @Test(timeout = 120_000) fun settingsModelRecreationAndNativeBackPreserveTheSyntheticDraft() {
        assertTrue("This persistent settings fixture is emulator-only",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish")
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val target = instrumentation.targetContext.applicationContext
        val engine = AssistantRuntime.get(target)
        val initial = settled { engine.refresh(it) }
        assertTrue("Do not seed over an active or unhealthy runtime", !initial.busy && initial.error == null)
        val originalModel = initial.model
        val alternative = AssistantModels.aliases.first { it != originalModel }
        val fixtureName = "synthetic-settings-${UUID.randomUUID()}"
        val title = "Synthetic Wear settings $fixtureName"
        val draft = "Synthetic settings draft must survive model selection, recreation and native Back."
        val permission = target.checkSelfPermission(Manifest.permission.RECORD_AUDIO)
        val windows = mutableMapOf<Activity, Boolean>()
        val screenshotDirectory = requireNotNull(target.externalCacheDir)
        val geometry = target.resources.displayMetrics
        val configuration = target.resources.configuration
        var micEnabled = false
        var isolatedGlyphs = 0
        AssistantStore(target).use { store ->
            val before = store.snapshot()
            val oldRecords = recordKeys(before.getJSONArray("records"))
            val local = store.localState()
            val oldConversation = if (local.isNull("conversation")) null else local.getJSONObject("conversation")
            val oldDraft = store.draft(oldConversation).toString()
            val newDraft = store.draft(null).toString()
            val conversation = store.createConversation(title).getJSONObject("operation").getJSONObject("id")
            assertFixture(settled { engine.selectConversation(conversation.toString(), it) }, conversation)
            val launch = Intent(target, AssistantActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            ActivityScenario.launch<AssistantActivity>(launch).use { scenario ->
                try {
                    scenario.onActivity { activity ->
                        windows[activity] = activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON != 0
                        activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                    }
                    await("The real main activity must foreground the synthetic conversation") {
                        matchesFixture(engine.snapshot(), conversation) && engine.snapshot().foreground
                    }
                     await("The SDK capability must render a microphone edge action without starting capture") {
                        currentNodes().any { it.isVisibleToUser && it.contentDescription?.toString()?.startsWith("Mic:") == true }
                    }
                    val mic = currentNodes().first { it.isVisibleToUser && it.contentDescription?.toString()?.startsWith("Mic:") == true }
                    micEnabled = clickableNode(mic).isEnabled
                     assertPolarBounds(mic.contentDescription.toString(), geometry.widthPixels, geometry.heightPixels, configuration.isScreenRound)
                    assertDraft(settled { engine.setDraft(draft, it) }, conversation, draft)
                     await("The synthetic draft must render explicit Send, without any relocated controls") {
                         currentNodes().any { it.isVisibleToUser && it.contentDescription?.toString() == "Send draft" } &&
                             currentNodes().none { it.isVisibleToUser &&
                                 (it.contentDescription?.toString() in listOf("New chat", "Chats", "Keyboard: edit draft", "Settings") ||
                                     it.text?.toString() in listOf("New chat", "Chats", "Keyboard")) }
                     }
                     isolatedGlyphs = assertPolarBounds("Send draft", geometry.widthPixels, geometry.heightPixels, configuration.isScreenRound)
                    saveScreenshot(File(screenshotDirectory, "$fixtureName-main.png"))
                     openSettings()
                    val settingsBefore = inspectSettings()
                    if (!micEnabled) assertTrue("Settings must explain the SDK-disabled microphone's Keyboard fallback",
                        settingsBefore.first.any { it.contains("Native dictation is unavailable. Keyboard remains available.") })
                     assertNotNull("The persisted Speak replies choice must be observable", settingsBefore.second)
                    tapSettingsModel(alternative)
                    await("Model selection must update the actual engine without sending") { engine.snapshot().model == alternative }
                    assertDraft(settled { engine.refresh(it) }, conversation, draft)
                    saveScreenshot(File(screenshotDirectory, "$fixtureName-settings.png"))
                    scenario.recreate()
                    scenario.onActivity { activity ->
                        windows[activity] = windows.values.first()
                        activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                    }
                    assertEquals(alternative, settled { engine.refresh(it) }.model)
                    await("The recreated actual settings UI must retain the selected alias") {
                        currentNodes().any { it.isVisibleToUser && it.text?.toString() == "✓ $alternative" }
                    }
                    saveScreenshot(File(screenshotDirectory, "$fixtureName-recreated.png"))
                    val settingsAfter = inspectSettings()
                    assertTrue("Model selection and recreation must not toggle local speech", settingsAfter.second == settingsBefore.second)
                    instrumentation.sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
                     await("Native Back must return to the real main Send target") {
                         instrumentation.uiAutomation.rootInActiveWindow?.let { findFooter(it, "Send draft") } != null &&
                            currentNodes().none { it.isVisibleToUser && it.text?.toString() == "Settings" }
                    }
                    assertDraft(settled { engine.refresh(it) }, conversation, draft)
                    assertTrue("Settings and native Back must preserve the synthetic root selection", engine.snapshot().selectedHead == null)
                    assertTrue("Native Back must preserve the selected alias until explicit restoration", engine.snapshot().model == alternative)
                    assertNoSend(engine, store, conversation)
                    assertEquals(permission, target.checkSelfPermission(Manifest.permission.RECORD_AUDIO))
                    assertTrue("Previously selected and fresh drafts must remain unchanged",
                        store.draft(oldConversation).toString() == oldDraft && store.draft(null).toString() == newDraft)
                    val after = store.snapshot()
                    assertTrue("Every prior history record and retired entry must remain unchanged",
                        recordKeys(after.getJSONArray("records")).entries.containsAll(oldRecords.entries) &&
                            after.getJSONArray("retired").toString() == before.getJSONArray("retired").toString())
                } finally {
                    try {
                        assertEquals(originalModel, settled { engine.selectModel(originalModel, it) }.model)
                    } finally {
                        instrumentation.runOnMainSync {
                            windows.forEach { (activity, wasSet) ->
                                if (wasSet) activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                                else activity.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
                            }
                        }
                    }
                }
            }
        }
        instrumentation.sendStatus(0, Bundle().apply {
            putString("stream", "\n$title | PASS draft=preserved model=recreated-and-restored native-back=main mic-enabled=$micEnabled " +
                "round=${configuration.isScreenRound} font-scale=${configuration.fontScale} density=${geometry.density} " +
                "isolated-24dp-glyphs=$isolatedGlyphs native-edge-shape=not-proven-by-accessibility screenshots=$screenshotDirectory/$fixtureName-{main,settings,recreated}.png\n")
        })
    }

    private fun currentNodes(): List<AccessibilityNodeInfo> {
        val nodes = mutableListOf<AccessibilityNodeInfo>()
        fun collect(node: AccessibilityNodeInfo) {
            nodes += node
            for (index in 0 until node.childCount) node.getChild(index)?.let(::collect)
        }
        InstrumentationRegistry.getInstrumentation().uiAutomation.rootInActiveWindow?.let(::collect)
        return nodes
    }

    private fun clickableNode(node: AccessibilityNodeInfo): AccessibilityNodeInfo {
        var target = node
        while (!target.isClickable && target.className?.toString() != "android.widget.Button" && target.parent != null) target = target.parent
        return target
    }

    private fun assertPolarBounds(middle: String, width: Int, height: Int, round: Boolean): Int {
        val density = InstrumentationRegistry.getInstrumentation().targetContext.resources.displayMetrics.density
        var isolatedGlyphs = 0
        await("The native edge action must expand at latest before its geometry is inspected") {
            currentNodes().firstOrNull { it.isVisibleToUser && it.contentDescription?.toString() == middle }?.let { node ->
                val bounds = Rect().also { clickableNode(node).getBoundsInScreen(it) }
                bounds.height() / density >= 45f
            } == true
        }
        assertTrue("New chat belongs only in Settings", currentNodes().none { it.isVisibleToUser &&
            (it.contentDescription?.toString() == "New chat" || it.text?.toString() == "New chat") })
        val bounds = listOf(middle).map { description ->
            val node = currentNodes().first { it.isVisibleToUser && it.contentDescription?.toString() == description }
            val target = clickableNode(node)
            if (node != target) {
                val glyph = Rect().also { node.getBoundsInScreen(it) }
                assertEquals("An independently exposed ExtraSmall edge glyph uses the recommended 24dp", 24f, glyph.width() / density, 1f / density)
                assertEquals("An independently exposed ExtraSmall edge glyph uses the recommended 24dp", 24f, glyph.height() / density, 1f / density)
                if (round) {
                    val radius = minOf(width, height) / 2f
                    for (x in listOf(glyph.left, glyph.right)) for (y in listOf(glyph.top, glyph.bottom)) {
                        assertTrue("The visible icon, not the curved button's full-width bounding box, stays on the face",
                            kotlin.math.hypot(x - width / 2f, y - height / 2f) <= radius + 1f)
                    }
                }
                isolatedGlyphs++
            }
            Rect().also { target.getBoundsInScreen(it) }.also { rect ->
                assertTrue("The edge action exposes positive on-screen bounds", !rect.isEmpty && Rect(0, 0, width, height).contains(rect))
                assertTrue("The native edge action uses the screen width", rect.width() >= width - 2)
                assertTrue("ExtraSmall exposes its 46dp surface / 52dp padded size with framework touch-target expansion",
                    rect.height() / density in 45f..53f)
                assertEquals("The primary edge action is centered", width / 2f, rect.exactCenterX(), 1f)
                assertTrue("The edge action's accessible bounds hug the bottom; painted shape needs separate acceptance",
                    height - rect.bottom <= 4f * density)
            }
        }
        assertEquals("Only the primary edge action remains", 1, bounds.distinct().size)
        assertTrue("Primary stays below chat", bounds.single().exactCenterY() > height * .75f)
        return isolatedGlyphs
    }

    private fun inspectSettings(): Pair<Set<String>, Boolean?> {
        val labels = mutableSetOf<String>()
        var speechChecked: Boolean? = null
        val forbidden = Regex("\\b(import|credentials?|access|authentication|authorization)\\b", RegexOption.IGNORE_CASE)
        await("The entire actual Settings list must be inspected within sixty-five seconds", 65_000) {
            val nodes = currentNodes().filter { it.isVisibleToUser }
            val texts = nodes.mapNotNull { it.text?.toString() }
            assertTrue("Web access is policy, not provider credential or Access management",
                texts.none { forbidden.containsMatchIn(it.replace("Web access", "")) })
            labels += texts
            nodes.firstOrNull { it.isCheckable && it.text?.toString()?.contains("Speak replies") == true }?.let { speechChecked = it.isChecked }
            val scroll = nodes.lastOrNull { it.isScrollable }
            val moved = scroll?.performAction(AccessibilityNodeInfo.ACTION_SCROLL_FORWARD) == true
            if (moved) SystemClock.sleep(250)
            !moved && labels.any { it.contains("Recheck connection") }
        }
        return labels to speechChecked
    }

    private fun tapSettingsModel(alias: String) {
        await("The existing model alias must be selected through Settings within sixty-five seconds", 65_000) {
            val nodes = currentNodes().filter { it.isVisibleToUser }
            val model = nodes.firstOrNull { it.text?.toString() == alias }
            if (model != null) {
                val target = clickableNode(model)
                target.isEnabled && target.performAction(AccessibilityNodeInfo.ACTION_CLICK)
            } else {
                nodes.lastOrNull { it.isScrollable }?.performAction(AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD)
                SystemClock.sleep(250)
                false
            }
        }
    }

    private fun saveScreenshot(file: File) {
        val bitmap = requireNotNull(InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot())
        try {
            file.outputStream().use { assertTrue("The synthetic runtime screenshot must be saved", bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)) }
        } finally { bitmap.recycle() }
    }

    private fun sendLiveUiTurn(engine: AssistantEngine, store: AssistantStore, conversation: JSONObject,
        parent: JSONObject?, prompt: String): JSONObject {
         openKeyboardFromSettings()
        val editor = resumedEditor()
        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            val input = editor.findViewById<EditText>(android.R.id.edit)
            input.setText(prompt)
            assertTrue("The owned editor must retain the complete synthetic prompt", input.text.toString() == prompt)
            assertTrue("The existing Use text button must confirm the live draft",
                editor.findViewById<Button>(android.R.id.button1).performClick())
        }
        await("Send must wait for the complete draft in the foreground main engine") {
            val state = engine.snapshot()
            matchesFixture(state, conversation) && state.foreground && !state.busy && state.draft == prompt
        }
        val ready = settled { engine.refresh(it) }
        assertDraft(ready, conversation, prompt)
        assertTrue("The live request must be pinned to the expected saved parent",
            if (parent == null) ready.selectedHead == null else ready.selectedHead?.let { sameId(JSONObject(it), parent) } == true)
        val providerId = AtomicReference<String>()
        val clientToken = AtomicReference<String>()
        engine.subscribe { state ->
            if (matchesFixture(state, conversation) && state.busy) {
                providerId.set(state.providerRequestId)
                clientToken.set(state.clientRequestToken)
            }
        }.use {
            tapFooter("Send draft")
            await("The actual native provider actor must save a complete reply within ninety seconds", 90_000) {
                val state = engine.snapshot()
                assertTrue("The opted-in live request must settle without a provider or storage error", state.error == null)
                assertTrue("Live execution must never change the selected synthetic conversation", matchesFixture(state, conversation))
                providerId.get() != null && !state.busy && state.draft.isEmpty() && state.selectedHead != null
            }
        }
        val completed = settled { engine.refresh(it) }
        assertFixture(completed, conversation)
        val head = JSONObject(completed.selectedHead!!)
        val turns = store.context(conversation, head).getJSONArray("turns")
        assertEquals(if (parent == null) 1 else 2, turns.length())
        val record = turns.getJSONObject(turns.length() - 1)
        val operation = record.getJSONObject("operation")
        val body = operation.getJSONObject("body")
        val request = body.getJSONObject("request")
        assertTrue("The native provider request ID must correlate with the durable request identity",
            providerId.get() == "${request.getString("replica")}:${request.getString("sequence")}" && clientToken.get() != null &&
                engine.clientTokenForDurableRequest(request.getString("replica"), request.getString("sequence")) == clientToken.get())
        assertTrue("The saved head and completion must identify the same new operation",
            sameId(operation.getJSONObject("id"), head) && !sameId(request, head))
        assertTrue("The stored completion must belong to the same synthetic conversation", sameId(body.getJSONObject("conversation"), conversation))
        assertTrue("The stored prompt must match the full owned-editor input", body.getString("prompt") == prompt)
        assertTrue("The core parent must be the preceding saved completion, or null for the first turn",
            if (parent == null) body.isNull("parent") else sameId(body.getJSONObject("parent"), parent))
        val answer = body.getJSONObject("answer")
        assertTrue("The actual saved provider answer must be exactly the synthetic marker, allowing trailing newlines only",
            answer.getString("kind") == "text" && answer.getString("text").trimEnd('\r', '\n') == "WATCH_EMULATOR_OK")
        assertTrue("Successful live sends must clear only their synthetic conversation draft", store.draft(conversation).getString("text").isEmpty())
        return record
    }

    private fun sameId(first: JSONObject, second: JSONObject) =
        first.getString("replica") == second.getString("replica") && first.getString("sequence") == second.getString("sequence")

    private fun openSettings() {
        var action: Pair<AccessibilityNodeInfo, Int>? = null
        await("Chat exposes its Settings navigation action") {
            action = currentNodes().filter { it.isVisibleToUser }.firstNotNullOfOrNull { node ->
                node.actionList.firstOrNull { it.label?.toString() == "Open Settings" }?.let { node to it.id }
            }
            action != null
        }
        assertTrue("Open Settings must navigate without editing or sending", action!!.first.performAction(action!!.second))
        await("Settings is visible") { currentNodes().any { it.isVisibleToUser && it.text?.toString() == "Settings" } }
    }

    private fun openKeyboardFromSettings() {
        openSettings()
        var keyboard: AccessibilityNodeInfo? = null
        await("Keyboard is near the start of Settings") {
            keyboard = currentNodes().firstOrNull { it.isVisibleToUser && it.text?.toString() == "Keyboard" }
            if (keyboard == null) currentNodes().lastOrNull { it.isVisibleToUser && it.isScrollable }
                ?.performAction(AccessibilityNodeInfo.ACTION_SCROLL_FORWARD)
            keyboard != null
        }
        val target = clickableNode(keyboard!!)
        assertTrue("Keyboard stays available without native recognition capability", target.isEnabled)
        assertTrue("Keyboard launches the owned editor", target.performAction(AccessibilityNodeInfo.ACTION_CLICK))
    }

    private fun tapFooter(description: String) {
        val automation = InstrumentationRegistry.getInstrumentation().uiAutomation
        var footer: AccessibilityNodeInfo? = null
        await("The actual native $description footer must become visible and enabled") {
            footer = automation.rootInActiveWindow?.let { findFooter(it, description) }
            footer != null
        }
        var target = footer!!
        while (!target.isClickable && target.parent != null) target = target.parent
        assertTrue("The actual $description footer must accept its accessibility click",
            target.isEnabled && target.performAction(AccessibilityNodeInfo.ACTION_CLICK))
    }

    private fun findFooter(node: AccessibilityNodeInfo, description: String): AccessibilityNodeInfo? {
        if (node.isVisibleToUser && node.isEnabled && node.contentDescription?.toString() == description) return node
        for (index in 0 until node.childCount) {
            val child = node.getChild(index) ?: continue
            findFooter(child, description)?.let { return it }
        }
        return null
    }

    private fun resumedEditor(): AssistantTextEntryActivity {
        val activity = AtomicReference<AssistantTextEntryActivity>()
        await("Keyboard must directly open the existing native AssistantTextEntryActivity") {
            InstrumentationRegistry.getInstrumentation().runOnMainSync {
                activity.set(ActivityLifecycleMonitorRegistry.getInstance().getActivitiesInStage(Stage.RESUMED)
                    .filterIsInstance<AssistantTextEntryActivity>().singleOrNull())
            }
            activity.get() != null
        }
        return activity.get()
    }

    private fun isResumed(type: Class<out Activity>): Boolean {
        var resumed = false
        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            resumed = ActivityLifecycleMonitorRegistry.getInstance().getActivitiesInStage(Stage.RESUMED).any { it.javaClass == type }
        }
        return resumed
    }

    private fun assertNoSend(engine: AssistantEngine, store: AssistantStore, conversation: JSONObject) {
        val state = engine.snapshot()
        assertFixture(state, conversation)
        assertTrue("Editor confirmation or cancellation must never submit a provider request",
            !state.busy && state.clientRequestToken == null && state.providerRequestId == null)
        assertEquals(0, store.context(conversation, null).getJSONArray("turns").length())
        assertTrue("The native confirmed draft must remain durable",
            store.draft(conversation).getString("text") == state.draft)
    }

    private fun matchesFixture(state: AssistantState, conversation: JSONObject): Boolean = state.conversation?.let {
        val id = JSONObject(it)
        id.getString("replica") == conversation.getString("replica") && id.getString("sequence") == conversation.getString("sequence")
    } == true

    private fun assertFixture(state: AssistantState, conversation: JSONObject) {
        assertTrue("The runtime must still own only the new synthetic conversation selection",
            matchesFixture(state, conversation) && !state.busy && state.error == null)
    }

    private fun assertDraft(state: AssistantState, conversation: JSONObject, expected: String) {
        assertFixture(state, conversation)
        assertTrue("The engine must retain the exact full synthetic draft", state.draft == expected)
    }

    private fun recordKeys(records: JSONArray): Map<String, String> = (0 until records.length()).associate { index ->
        val record = records.getJSONObject(index)
        val id = record.getJSONObject("operation").getJSONObject("id")
        "${id.getString("replica")}/${id.getString("sequence")}" to record.getString("body_key")
    }

    private fun settled(start: ((AssistantState) -> Unit) -> Unit): AssistantState {
        val state = AtomicReference<AssistantState>()
        val latch = CountDownLatch(1)
        start { state.set(it); latch.countDown() }
        assertTrue("The runtime must settle within fifteen seconds", latch.await(15, TimeUnit.SECONDS))
        return state.get()
    }

    private fun await(message: String, timeoutMillis: Long = 15_000, condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + timeoutMillis
        while (!condition()) {
            assertTrue(message, SystemClock.elapsedRealtime() < deadline)
            SystemClock.sleep(50)
        }
    }
}
