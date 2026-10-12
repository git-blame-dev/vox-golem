package dev.gitblame.voxgolem.android.wear.assistant

import android.os.Looper
import android.os.Handler
import android.os.SystemClock
import android.view.InputDevice
import android.view.MotionEvent
import android.view.WindowManager
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.recognition.DictationState
import dev.gitblame.voxgolem.assistant.replication.PeerPairingReason
import dev.gitblame.voxgolem.assistant.replication.PeerPairingState
import dev.gitblame.voxgolem.assistant.speech.SpeechPhase
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowSystemClock
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AssistantScreenAwakeTest {
    @Test fun progressDeliveredAfterTheExpiryCallbackCannotMakeAnIdenticalSnapshotLookFresh() {
        var enabled = false
        val policy = AssistantScreenAwake(Handler(Looper.getMainLooper())) { enabled = it }
        val initial = AssistantVoiceState()
        val changed = initial.copy(dictation = DictationState(DictationPhase.LISTENING, "Synthetic queued partial", utterance = 1))
        policy.resume(dev.gitblame.voxgolem.assistant.runtime.AssistantState(), initial)
        val token = policy.token
        val occurredAt = SystemClock.uptimeMillis()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            looper.idleFor(Duration.ofMinutes(2))
            assertFalse(enabled)
            policy.voiceProgress(changed, token, occurredAt)
            assertFalse(enabled)
            policy.voiceProgress(changed, policy.token, SystemClock.uptimeMillis())
            assertFalse(enabled)
            policy.voiceProgress(changed.copy(dictation = changed.dictation.copy(draft = "Synthetic fresh partial")),
                policy.token, SystemClock.uptimeMillis())
            assertTrue(enabled)
        } finally { policy.suspend() }
    }

    private fun awake(activity: AssistantActivity) =
        activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON != 0

    private fun idleUntil(uptime: Long) = Shadows.shadowOf(Looper.getMainLooper())
        .idleFor(Duration.ofMillis((uptime - SystemClock.uptimeMillis()).coerceAtLeast(0)))

    private fun touch(activity: AssistantActivity) {
        val now = SystemClock.uptimeMillis()
        val event = MotionEvent.obtain(now, now, MotionEvent.ACTION_DOWN, 20f, 20f, 0)
        try { activity.dispatchTouchEvent(event) } finally { event.recycle() }
    }

    private fun rotary(activity: AssistantActivity, scroll: Float) {
        val now = SystemClock.uptimeMillis()
        val properties = arrayOf(MotionEvent.PointerProperties().apply { id = 0 })
        val coordinates = arrayOf(MotionEvent.PointerCoords().apply { setAxisValue(MotionEvent.AXIS_SCROLL, scroll) })
        val event = MotionEvent.obtain(now, now, MotionEvent.ACTION_SCROLL, 1, properties, coordinates,
            0, 0, 1f, 1f, 0, 0, InputDevice.SOURCE_ROTARY_ENCODER, 0)
        try { activity.dispatchGenericMotionEvent(event) } finally { event.recycle() }
    }

    @Test fun foregroundWindowExpiresAtExactlyTwoMinutesWithoutFinishingAndResumesFresh() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).create().start()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            assertFalse(awake(activity))
            val resumedAt = SystemClock.uptimeMillis()
            controller.resume().visible(); looper.idle()
            assertTrue(awake(activity))
            idleUntil(resumedAt + 119_999); assertTrue(awake(activity))
            looper.idleFor(Duration.ofMillis(1)); assertFalse(awake(activity))
            assertFalse(activity.isFinishing)
            controller.pause().resume(); looper.idle()
            assertTrue(awake(activity))
            looper.idleFor(Duration.ofMillis(120_000)); assertFalse(awake(activity))
            assertFalse(activity.isFinishing)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun realTouchAndNonzeroRotaryResetTheDeadlineAndTouchCanRearmAfterIdle() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            looper.idleFor(Duration.ofSeconds(90)); touch(activity)
            looper.idleFor(Duration.ofSeconds(90)); assertTrue(awake(activity))
            val rotaryAt = SystemClock.uptimeMillis()
            rotary(activity, 1f)
            idleUntil(rotaryAt + 119_999); assertTrue(awake(activity))
            rotary(activity, 0f)
            looper.idleFor(Duration.ofMillis(1)); assertFalse(awake(activity))
            touch(activity); assertTrue(awake(activity))
            looper.idleFor(Duration.ofSeconds(120)); assertFalse(awake(activity))
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun equalSnapshotsLinkPollingSyncAndCapabilityMetadataNeverRefreshTheWindow() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            looper.idle()
            repeat(3) { index ->
                looper.idleFor(Duration.ofSeconds(30))
                activity.synthetic.publish(activity.synthetic.state)
                activity.audio.publish(activity.audio.state)
                activity.synthetic.publish(activity.synthetic.state.copy(
                    viewJson = "{\"revision\":\"${index + 1}\",\"records\":[],\"conversations\":[],\"pending\":[]}",
                    selectedHead = "{\"replica\":\"synthetic-synced-head\",\"sequence\":\"${index + 1}\"}",
                    providerRequestId = "synthetic-provider-metadata-$index"))
                activity.audio.publish(activity.audio.state.copy(admissionReady = index % 2 == 0,
                    recognitionServiceAvailable = index % 2 != 0))
                activity.linkReads.last()(PeerPairingState(PeerPairingReason.ENROLLED))
                looper.idle()
            }
            looper.idleFor(Duration.ofSeconds(30))
            assertFalse(awake(activity))
            assertFalse(activity.isFinishing)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun meaningfulChatAndSpeechProgressRefreshButRepeatedActiveSnapshotsDoNot() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            looper.idleFor(Duration.ofSeconds(90))
            activity.synthetic.publish(activity.synthetic.state.copy(busy = true, clientRequestToken = "synthetic-awake-request")); looper.idle()
            looper.idleFor(Duration.ofSeconds(90)); assertTrue(awake(activity))
            activity.synthetic.publish(activity.synthetic.state.copy(provisional = "Synthetic streamed answer")); looper.idle()
            looper.idleFor(Duration.ofSeconds(90)); assertTrue(awake(activity))
            activity.synthetic.publish(activity.synthetic.state.copy(busy = false, draft = "")); looper.idle()
            looper.idleFor(Duration.ofSeconds(90)); assertTrue(awake(activity))
            activity.audio.publish(activity.audio.state.copy(dictation = DictationState(DictationPhase.LISTENING,
                "Synthetic speech partial", utterance = 1))); looper.idle()
            looper.idleFor(Duration.ofSeconds(90)); assertTrue(awake(activity))
            activity.audio.publish(activity.audio.state.copy(dictation = activity.audio.state.dictation.copy(draft = "Synthetic longer speech partial"))); looper.idle()
            looper.idleFor(Duration.ofSeconds(90)); assertTrue(awake(activity))
            activity.audio.publish(activity.audio.state.copy(speech = activity.audio.state.speech.copy(phase = SpeechPhase.SPEAKING))); looper.idle()
            looper.idleFor(Duration.ofSeconds(90)); assertTrue(awake(activity))
            activity.audio.publish(activity.audio.state); looper.idle()
            looper.idleFor(Duration.ofSeconds(30)); assertFalse(awake(activity))
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun queuedProgressCannotRearmAfterIdleOrCrossPauseResumeAndCleanupIsImmediate() {
        val controller = Robolectric.buildActivity(SyntheticAssistantActivity::class.java).setup()
        val activity = controller.get()
        val looper = Shadows.shadowOf(Looper.getMainLooper())
        try {
            looper.idle()
            looper.idleFor(Duration.ofSeconds(119))
            activity.audio.publish(activity.audio.state.copy(dictation = DictationState(DictationPhase.LISTENING,
                "Synthetic queued partial", utterance = 1)))
            // Advance the Android clock without executing queued progress until its old deadline has passed.
            ShadowSystemClock.advanceBy(Duration.ofSeconds(1))
            looper.idle()
            assertFalse(awake(activity))
            activity.audio.publish(activity.audio.state); looper.idle()
            assertFalse(awake(activity))
            activity.audio.publish(activity.audio.state.copy(dictation = activity.audio.state.dictation.copy(
                draft = "Synthetic fresh partial after idle"))); looper.idle()
            assertTrue(awake(activity))
            controller.pause(); assertFalse(awake(activity))
            controller.resume(); assertTrue(awake(activity))
            looper.idleFor(Duration.ofSeconds(90))
            activity.synthetic.publish(activity.synthetic.state.copy(draft = "Synthetic queued draft"))
            controller.pause(); assertFalse(awake(activity))
            touch(activity); rotary(activity, 1f); assertFalse(awake(activity))
            controller.resume(); looper.idle()
            looper.idleFor(Duration.ofSeconds(120)); assertFalse(awake(activity))
            controller.pause().stop(); assertFalse(awake(activity))
            controller.start().resume(); assertTrue(awake(activity))
            controller.pause().stop().destroy(); assertFalse(awake(activity))
            looper.idleFor(Duration.ofMinutes(3)); assertFalse(awake(activity))
            assertFalse(activity.isFinishing)
        } finally { if (!activity.isDestroyed) controller.pause().stop().destroy() }
    }
}
