package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.view.WindowManager
import android.view.accessibility.AccessibilityNodeInfo
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import dev.gitblame.voxgolem.assistant.runtime.AssistantModels
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.runtime.ProviderAuthReason
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Real provider/JNI acceptance; the duration is native dispatch-to-text, not render latency. */
@RunWith(AndroidJUnit4::class)
class WatchReplyLatencyTest {
    @Test(timeout = 300_000)
    fun watch7ManualSendRetainsNativeFirstTextMillisecondsAndDisplaysExactLabel() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext.applicationContext
        assumeTrue("Requires the authorized physical Watch7",
            Build.MODEL == "SM-L315F" &&
                context.packageManager.hasSystemFeature(PackageManager.FEATURE_WATCH) &&
                Build.HARDWARE != "ranchu" && Build.HARDWARE != "goldfish")
        assumeTrue("Synthetic live-provider execution requires explicit opt-in",
            InstrumentationRegistry.getArguments().getString("liveProvider") == "true")

        val engine = AssistantRuntime.get(context)
        val initial = settled { engine.refresh(it) }
        // Check before launching: Activity pause/background must not cancel someone else's work.
        assumeTrue("Runtime must be idle with no unsent draft",
            !initial.busy && initial.draft.isBlank() && initial.error == null)
        assumeTrue("Audio resource must be unowned", RecognitionResourceLease.process.current() == null)
        assumeTrue("Saved provider access is not ready", initial.credential?.reason == ProviderAuthReason.READY)
        assumeTrue("Saved model must be a supported alias", initial.model in AssistantModels.aliases)
        val savedVoicePreferences = WearVoiceSettings(context).current()

        val launch = Intent(context, AssistantActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        // Closing the production Activity backgrounds/cancels its owners, without deleting history.
        ActivityScenario.launch<AssistantActivity>(launch).use { scenario ->
            scenario.onActivity { activity ->
                assertEquals(AssistantActivity::class.java, activity.javaClass)
                // Scoped to this window; Scenario.close destroys it without changing settings.
                activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
            }
            await("The production assistant must enter foreground") { engine.snapshot().foreground }
            val ready = settled { engine.refresh(it) }
            assumeTrue("Runtime changed before synthetic conversation creation: " +
                "foreground=${ready.foreground}, busy=${ready.busy}, " +
                "draftBlank=${ready.draft.isBlank()}, errorPresent=${ready.error != null}",
                ready.foreground && !ready.busy && ready.draft.isBlank() && ready.error == null)
            assumeTrue("Capture admission must be connected and audio unowned",
                AssistantRuntime.captureResourceAdmissionReady() && RecognitionResourceLease.process.current() == null)
            assumeTrue("Saved provider access is not ready", ready.credential?.reason == ProviderAuthReason.READY)

            val fresh = settled { engine.newConversation(it) }
            assertTrue("Only a new empty conversation may receive synthetic input",
                fresh.conversation != null && fresh.conversation != initial.conversation &&
                    !fresh.busy && fresh.draft.isBlank() && fresh.selectedHead == null && fresh.error == null)
            val conversation = fresh.conversation
            val prompt = "What is two plus two? Answer briefly."
            val draft = settled { engine.setDraft(prompt, it) }
            assertTrue("The actor must settle the owned draft before manual Send",
                draft.conversation == conversation && draft.draft == prompt && draft.foreground &&
                    !draft.busy && draft.error == null && draft.selectedHead == null)
            assertTrue("Saved model must not change", draft.model == initial.model)

            val armed = AtomicBoolean(false)
            val admission = AtomicReference<Admission?>(null)
            val firstTextMs = AtomicReference<Long?>(null)
            val streamedText = AtomicBoolean(false)
            val duplicate = AtomicBoolean(false)
            val invalidMetric = AtomicBoolean(false)
            val failed = AtomicBoolean(false)
            val authUnavailable = AtomicBoolean(false)
            val completedReply = AtomicBoolean(false)
            val terminal = CountDownLatch(1)

            // Actor callbacks capture the exact admission token before any completion, even if
            // the provider finishes between test-thread polls. Never time the polling loop.
            engine.subscribe { state ->
                if (armed.get() && state.conversation == conversation) {
                    if (state.busy) {
                        val token = state.clientRequestToken
                        val provider = state.providerRequestId
                        if (token.isNullOrBlank() || provider.isNullOrBlank()) {
                            failed.set(true)
                        } else {
                            val identity = Admission(token, provider)
                            admission.compareAndSet(null, identity)
                            if (admission.get() != identity) duplicate.set(true)
                            state.firstTextMs?.let { milliseconds ->
                                firstTextMs.compareAndSet(null, milliseconds)
                                if (milliseconds < 0 || firstTextMs.get() != milliseconds) invalidMetric.set(true)
                            }
                            if (state.provisional.isNotBlank()) streamedText.set(true)
                        }
                    } else if (state.error != null || admission.get() != null) {
                        authUnavailable.set(state.error in setOf("auth_unavailable", "auth_expired", "unauthorized", "forbidden"))
                        failed.set(failed.get() || state.error != null)
                        val identity = admission.get()
                        completedReply.set(identity != null && state.clientRequestToken == identity.token &&
                            state.providerRequestId == identity.provider && state.selectedHead != null &&
                            state.draft.isBlank() && state.error == null)
                        terminal.countDown()
                    }
                }
            }.use {
                // This barrier drains the initial subscription notification before arming Send.
                val beforeSend = settled { engine.refresh(it) }
                assertTrue("Manual Send must still own the settled synthetic draft",
                    beforeSend.conversation == conversation && beforeSend.draft == prompt &&
                        beforeSend.foreground && !beforeSend.busy && beforeSend.error == null &&
                        RecognitionResourceLease.process.current() == null)
                scenario.onActivity { activity ->
                    assertTrue("Saved voice preferences must remain unchanged",
                        activity.voicePreferences == savedVoicePreferences)
                    armed.set(true)
                    activity.sendManually()
                }
                assertTrue("The actual provider request must settle within three minutes",
                    terminal.await(180, TimeUnit.SECONDS))
                assumeTrue("Saved provider access was unavailable or rejected; no latency pass", !authUnavailable.get())
                assertTrue("Actual provider/storage execution must succeed", !failed.get() && completedReply.get())
                assertNotNull("Actor admission must identify the manual request", admission.get())
                assertTrue("Exactly one generation may be admitted", !duplicate.get())
                assertTrue("The actual busy stream must forward nonblank text", streamedText.get())
                val milliseconds = firstTextMs.get()
                assertNotNull("The busy stream must publish native first-text milliseconds", milliseconds)
                val measured = requireNotNull(milliseconds)
                assertTrue("Native first-text milliseconds must be bounded and stable",
                    measured >= 0 && measured < 180_000 && !invalidMetric.get())
                val identity = requireNotNull(admission.get())

                val completed = settled { engine.refresh(it) }
                assertTrue("The terminal projection must retain the same request and duration",
                    completed.conversation == conversation && !completed.busy && completed.error == null &&
                        completed.clientRequestToken == identity.token && completed.providerRequestId == identity.provider &&
                        completed.firstTextMs == measured && completed.model == initial.model)
                await("The production activity must persist the correlated duration") {
                    AssistantReplyMetrics(context).get(identity.provider) == measured
                }
                await("Native history must expose exactly one correlated synthetic completion") {
                    var matched = false
                    scenario.onActivity { activity ->
                        val history = activity.historyState
                        matched = history.conversation == conversation && !history.loading && history.error == null &&
                            history.turns.size == 1 && history.turns.single().requestKey == identity.provider &&
                            history.head == completed.selectedHead
                    }
                    matched
                }
                val expectedLabel = "Assistant · $measured ms"
                val initiallyVisible = hasVisibleExactLabel(expectedLabel)
                instrumentation.sendStatus(0, Bundle().apply {
                    putString("stream", "\nphase=native-history-metric-retained " +
                        "model=${initial.model} firstTextMs=$measured labelInitiallyVisible=$initiallyVisible\n")
                })
                // Following the reply's end can leave its header above the round viewport.
                // Scroll only the owned synthetic chat; do not inspect message contents.
                if (!initiallyVisible) scrollChatBackward()
                await("The completed reply must display its exact millisecond label") {
                    hasVisibleExactLabel(expectedLabel)
                }
                assertTrue("No later admission or metric replacement is allowed", !duplicate.get() && !invalidMetric.get())
                assertTrue("The test must preserve saved voice preferences",
                    WearVoiceSettings(context).current() == savedVoicePreferences)
                instrumentation.sendStatus(0, Bundle().apply {
                    // Model is a checked allowlisted alias; never emit content, IDs or auth metadata.
                    putString("stream", "\nmodel=${initial.model} firstTextMs=$measured\n")
                })
            }
        }
    }

    private data class Admission(val token: String, val provider: String)

    private fun hasVisibleExactLabel(expectedLabel: String): Boolean {
        // Compare only the fixed metric label in the owned synthetic chat; never dump text.
        return findAppNode {
            it.isVisibleToUser && it.text?.toString() == expectedLabel
        } != null
    }

    private fun scrollChatBackward(): Boolean = findAppNode { it.isScrollable }
        ?.performAction(AccessibilityNodeInfo.ACTION_SCROLL_BACKWARD) == true

    private fun findAppNode(matches: (AccessibilityNodeInfo) -> Boolean): AccessibilityNodeInfo? {
        val root = InstrumentationRegistry.getInstrumentation().uiAutomation.rootInActiveWindow ?: return null
        val pending = ArrayDeque<AccessibilityNodeInfo>()
        pending.add(root)
        var inspected = 0
        while (pending.isNotEmpty() && inspected++ < 256) {
            val node = pending.removeFirst()
            if (node.packageName?.toString() != "dev.gitblame.voxgolem.android") continue
            if (matches(node)) return node
            for (index in 0 until node.childCount) node.getChild(index)?.let(pending::addLast)
        }
        return null
    }

    private fun settled(start: ((AssistantState) -> Unit) -> Unit): AssistantState {
        val value = AtomicReference<AssistantState>()
        val latch = CountDownLatch(1)
        start { value.set(it); latch.countDown() }
        assertTrue("The native actor must settle within fifteen seconds", latch.await(15, TimeUnit.SECONDS))
        return value.get()
    }

    private fun await(message: String, condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + 15_000
        while (!condition()) {
            assertTrue(message, SystemClock.elapsedRealtime() < deadline)
            SystemClock.sleep(50)
        }
    }
}
