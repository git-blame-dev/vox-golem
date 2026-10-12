package dev.gitblame.voxgolem.android.wear

import android.content.Intent
import android.os.Bundle
import android.os.SystemClock
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureRepository
import dev.gitblame.voxgolem.capture.CaptureState
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.util.UUID
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureDismissalTest {
    @Test fun localSaveShowsBoundedFeedbackThenDismissesWithoutWaitingForFinalTranscript() {
        val context = RuntimeEnvironment.getApplication()
        val repository = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        repository.save(CaptureRecord(id, createdAtUtc = "2026-10-04T12:30:00Z", captureState = CaptureState.RECORDING))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        val activity = controller.get()
        awaitCaptureIdle(activity)
        repository.update(id) { it.copy(captureState = CaptureState.SAVED) }
        CaptureSignals.saved(activity, id)
        shadowOf(android.os.Looper.getMainLooper()).idle()
        awaitCaptureIdle(activity)
        assertFalse(activity.isFinishing)
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(1_000))
        CaptureSignals.saved(activity, id)
        shadowOf(android.os.Looper.getMainLooper()).idle()
        assertFalse(activity.isFinishing)
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(2_749))
        assertNull(exitDeadline(activity))
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(1))
        val deadline = exitDeadline(activity)
        assertEquals(SystemClock.elapsedRealtime() + 250L, deadline)
        CaptureSignals.saved(activity, id)
        shadowOf(android.os.Looper.getMainLooper()).idle()
        assertEquals(deadline, exitDeadline(activity))
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(249))
        assertFalse(activity.isFinishing)
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(1))
        assertTrue(activity.isFinishing)
        val home = shadowOf(activity).nextStartedActivity
        assertTrue(home.hasCategory(Intent.CATEGORY_HOME))
        assertTrue(home.flags and Intent.FLAG_ACTIVITY_NO_ANIMATION != 0)
        assertNull(shadowOf(activity).nextStartedActivity)
        controller.pause().stop().destroy()
    }

    @Test fun pausedSavedFeedbackDoesNotDismissUntilVisibleAgain() {
        val context = RuntimeEnvironment.getApplication()
        val repository = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        repository.save(CaptureRecord(id, createdAtUtc = "2026-10-04T12:30:00Z", captureState = CaptureState.RECORDING))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        repository.update(id) { it.copy(captureState = CaptureState.SAVED) }
        CaptureSignals.saved(controller.get(), id)
        shadowOf(android.os.Looper.getMainLooper()).idle()
        awaitCaptureIdle(controller.get())
        controller.pause()
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofSeconds(4))
        assertFalse(controller.get().isFinishing)
        assertNull(exitDeadline(controller.get()))
        assertNull(shadowOf(controller.get()).nextStartedActivity)
        controller.resume()
        shadowOf(android.os.Looper.getMainLooper()).idle()
        awaitCaptureIdle(controller.get())
        assertTrue(controller.get().isFinishing)
        controller.pause().stop().destroy()
    }

    @Test fun failedSaveKeepsRecoveryScreenOpen() {
        val context = RuntimeEnvironment.getApplication()
        val repository = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        repository.save(CaptureRecord(id, createdAtUtc = "2026-10-04T12:30:00Z", captureState = CaptureState.RECORDING))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        repository.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
        CaptureSignals.failed(controller.get(), "Synthetic save failure")
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofSeconds(5))
        assertFalse(controller.get().isFinishing)
        assertNull(exitDeadline(controller.get()))
        assertNull(savedFeedbackCaptureId(controller.get()))
        assertNull(shadowOf(controller.get()).nextStartedActivity)
        controller.pause().stop().destroy()
    }

    @Test fun failedMetadataCommitDoesNotPretendSavedOrKeepTheScreenAwakeForever() {
        val context = RuntimeEnvironment.getApplication()
        val repository = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        repository.save(CaptureRecord(id, createdAtUtc = "2026-10-04T12:30:00Z", captureState = CaptureState.RECORDING))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        repository.update(id) { it.copy(captureState = CaptureState.STOPPING) }
        CaptureSignals.failed(controller.get(), "Recording retained · synthetic metadata commit failure")
        awaitCaptureIdle(controller.get())
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofSeconds(2))
        assertFalse(controller.get().isFinishing)
        assertEquals(CaptureState.STOPPING, repository.get(id)?.captureState)
        assertEquals(0, controller.get().window.attributes.flags and android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        controller.pause().stop().destroy()
    }

    @Test fun pauseDuringShrinkCancelsExitAndCannotSendHiddenActivityHome() {
        val controller = savedActivity()
        val activity = controller.get()
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(3_800))
        assertNotNull(exitDeadline(activity))
        controller.pause()
        assertNull(exitDeadline(activity))
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofSeconds(5))
        assertFalse(activity.isFinishing)
        assertNull(shadowOf(activity).nextStartedActivity)
        controller.resume()
        shadowOf(android.os.Looper.getMainLooper()).idle()
        awaitCaptureIdle(activity)
        assertTrue(activity.isFinishing)
        assertTrue(shadowOf(activity).nextStartedActivity.hasCategory(Intent.CATEGORY_HOME))
        assertNull(shadowOf(activity).nextStartedActivity)
        controller.pause().stop().destroy()
    }

    @Test fun resumeInsideShrinkWindowUsesRemainingTimeNotANewAnimationDeadline() {
        val controller = savedActivity()
        val activity = controller.get()
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(3_800))
        val deadline = exitDeadline(activity)
        controller.pause()
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(75))
        controller.resume()
        shadowOf(android.os.Looper.getMainLooper()).idle()
        assertEquals(deadline, exitDeadline(activity))
        assertEquals(0.5f, captureSavedExitProgress(deadline!!, SystemClock.elapsedRealtime()), 0.0001f)
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(124))
        assertFalse(activity.isFinishing)
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(1))
        assertTrue(activity.isFinishing)
        controller.pause().stop().destroy()
    }

    @Test fun recreationDuringShrinkPreservesSavedDeadlineAndOnlyReplacementReturnsHome() {
        val controller = savedActivity()
        val original = controller.get()
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(3_800))
        val deadline = exitDeadline(original)
        val state = Bundle()
        controller.saveInstanceState(state).pause().stop().destroy()
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(50))
        val replacement = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW))
            .create(state).start().resume()
        shadowOf(android.os.Looper.getMainLooper()).idle()
        assertEquals(deadline, exitDeadline(replacement.get()))
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(149))
        assertFalse(replacement.get().isFinishing)
        assertNull(shadowOf(original).nextStartedActivity)
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofMillis(1))
        assertTrue(replacement.get().isFinishing)
        assertFalse(original.isFinishing)
        assertTrue(shadowOf(replacement.get()).nextStartedActivity.hasCategory(Intent.CATEGORY_HOME))
        assertNull(shadowOf(original).nextStartedActivity)
        replacement.pause().stop().destroy()
    }

    @Test fun savedSignalWithoutDurableSavedStateDoesNotStartConfirmationOrExit() {
        val repository = CaptureRepository(RuntimeEnvironment.getApplication())
        val id = UUID.randomUUID().toString()
        repository.save(CaptureRecord(id, createdAtUtc = "2026-10-04T12:30:00Z", captureState = CaptureState.STOPPING))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        CaptureSignals.saved(controller.get(), id)
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofSeconds(5))
        assertNull(savedFeedbackCaptureId(controller.get()))
        assertNull(exitDeadline(controller.get()))
        assertFalse(controller.get().isFinishing)
        assertNull(shadowOf(controller.get()).nextStartedActivity)
        repository.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
        controller.pause().stop().destroy()
    }

    @Test @Config(sdk = [33]) fun preAndroid14StillRequestsUnanimatedHomeAndCloses() {
        val controller = savedActivity()
        shadowOf(android.os.Looper.getMainLooper()).idleFor(Duration.ofSeconds(4))
        assertTrue(controller.get().isFinishing)
        val home = shadowOf(controller.get()).nextStartedActivity
        assertTrue(home.hasCategory(Intent.CATEGORY_HOME))
        assertTrue(home.flags and Intent.FLAG_ACTIVITY_NO_ANIMATION != 0)
        controller.pause().stop().destroy()
    }

    private fun savedActivity(): org.robolectric.android.controller.ActivityController<CaptureActivity> {
        val repository = CaptureRepository(RuntimeEnvironment.getApplication())
        val id = UUID.randomUUID().toString()
        repository.save(CaptureRecord(id, createdAtUtc = "2026-10-04T12:30:00Z", captureState = CaptureState.RECORDING))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        repository.update(id) { it.copy(captureState = CaptureState.SAVED) }
        CaptureSignals.saved(controller.get(), id)
        shadowOf(android.os.Looper.getMainLooper()).idle()
        awaitCaptureIdle(controller.get())
        return controller
    }

    private fun exitDeadline(activity: CaptureActivity): Long? {
        awaitCaptureIdle(activity)
        return activity.presentation.savedExitDeadlineMs
    }

    private fun savedFeedbackCaptureId(activity: CaptureActivity): String? {
        awaitCaptureIdle(activity)
        return activity.presentation.savedFeedbackCaptureId
    }
}
