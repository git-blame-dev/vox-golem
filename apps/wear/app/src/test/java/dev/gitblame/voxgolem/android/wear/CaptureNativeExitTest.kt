package dev.gitblame.voxgolem.android.wear

import android.Manifest
import android.content.Intent
import android.os.Bundle
import android.view.View
import dev.gitblame.voxgolem.android.wear.capture.CaptureService
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
import org.robolectric.util.ReflectionHelpers
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureNativeExitTest {
    private val context = RuntimeEnvironment.getApplication()

    @Test fun homeBeforePostedLauncherStartCannotStartOnReturn() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN)).create().start()
        try {
            val activity = controller.get()
            awaitCaptureIdle(activity)
            activity.setContentView(View(context))
            controller.resume().pause().stop()
            controller.restart().start().resume().visible()
            awaitCaptureIdle(activity)
            assertTrue("Leaving the Note entry cancels its queued start, including the posted resume callback", commands().isEmpty())
            assertTrue(CaptureRepository(context).list().isEmpty())
            activity.requestStart()
            awaitCaptureIdle(activity)
            assertEquals("A deliberate new Capture tap is still supported", listOf(CaptureService.ACTION_START), commands().map { it.action })
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun homeWhilePermissionIsPendingCannotRecordAfterLateGrantOrResume() {
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            val activity = controller.get()
            activity.requestStart()
            awaitCaptureIdle(activity)
            val request = shadowOf(activity).lastRequestedPermission
            controller.pause().stop()
            shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
            assertTrue(activity.activityResultRegistry.dispatchResult(request.requestCode, true))
            activity.setContentView(View(context))
            controller.restart().start().resume().visible()
            awaitCaptureIdle(activity)
            assertTrue("An abandoned permission request cannot become a fresh recording", commands().isEmpty())
            assertTrue(CaptureRepository(context).list().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun backWhilePermissionIsPendingRejectsGrantEvenBeforeStopLifecycle() {
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            val activity = controller.get()
            activity.requestStart()
            awaitCaptureIdle(activity)
            val request = shadowOf(activity).lastRequestedPermission
            activity.onBackPressedDispatcher.onBackPressed()
            awaitCaptureIdle(activity)
            assertTrue(activity.isFinishing)
            shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
            assertTrue(activity.activityResultRegistry.dispatchResult(request.requestCode, true))
            awaitCaptureIdle(activity)
            assertTrue("Finish is already an exit even if onStop has not run yet", commands().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun stopWhilePermissionIsPendingRejectsLateGrantWithoutPretendingSaved() {
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            val activity = controller.get()
            activity.requestStart()
            awaitCaptureIdle(activity)
            val request = shadowOf(activity).lastRequestedPermission
            activity.stopCapture()
            assertEquals(listOf(CaptureService.ACTION_STOP), commands().map { it.action })
            shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
            assertTrue(activity.activityResultRegistry.dispatchResult(request.requestCode, true))
            awaitCaptureIdle(activity)
            assertTrue("Stop retires the pending permission/start intent", commands().isEmpty())
            assertNull(activity.presentation.savedFeedbackCaptureId)
            assertTrue(CaptureRepository(context).list().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun finishingBeforePermissionGrantCannotStartCapture() {
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            val activity = controller.get()
            activity.requestStart()
            awaitCaptureIdle(activity)
            val request = shadowOf(activity).lastRequestedPermission
            activity.finish()
            shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
            assertTrue(activity.activityResultRegistry.dispatchResult(request.requestCode, true))
            awaitCaptureIdle(activity)
            assertTrue(commands().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun homeDuringStartLookupCannotRequeueStartOnResume() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            val activity = controller.get()
            awaitCaptureIdle(activity)
            activity.requestStart()
            controller.pause().stop()
            awaitCaptureIdle(activity)
            activity.setContentView(View(context))
            controller.restart().start().resume().visible()
            awaitCaptureIdle(activity)
            assertTrue("An IO continuation from before exit cannot attach or create another start", commands().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun stopDuringStartLookupCannotIssueThePendingStart() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            val activity = controller.get()
            awaitCaptureIdle(activity)
            activity.requestStart()
            activity.stopCapture()
            awaitCaptureIdle(activity)
            assertEquals(listOf(CaptureService.ACTION_STOP), commands().map { it.action })
            assertNull(activity.presentation.savedFeedbackCaptureId)
            assertTrue(CaptureRepository(context).list().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun configurationReplacementKeepsTheLivePermissionRequestEligible() {
        val original = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        original.get().requestStart()
        awaitCaptureIdle(original.get())
        val request = shadowOf(original.get()).lastRequestedPermission
        val state = Bundle()
        ReflectionHelpers.setField(original.get(), "mChangingConfigurations", true)
        original.saveInstanceState(state).pause().stop().destroy()
        assertTrue(commands().isEmpty())
        val replacement = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create(state).start().resume()
        try {
            shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
            assertTrue(replacement.get().activityResultRegistry.dispatchResult(request.requestCode, true))
            awaitCaptureIdle(replacement.get())
            assertEquals(listOf(CaptureService.ACTION_START), commands().map { it.action })
        } finally { replacement.pause().stop().destroy() }
    }

    @Test fun configurationReplacementAttachesToTheSameCaptureWithoutStopOrStart() {
        val repository = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        repository.save(CaptureRecord(id, createdAtUtc = "2026-10-08T00:00:00Z", captureState = CaptureState.RECORDING))
        val original = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(original.get())
        assertEquals(listOf(CaptureService.ACTION_ATTACH), commands().map { it.action })
        val state = Bundle()
        ReflectionHelpers.setField(original.get(), "mChangingConfigurations", true)
        original.saveInstanceState(state).pause().stop().destroy()
        assertTrue(commands().isEmpty())
        val replacement = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create(state).start().resume()
        try {
            awaitCaptureIdle(replacement.get())
            assertEquals(listOf(CaptureService.ACTION_ATTACH), commands().map { it.action })
            assertEquals(id, replacement.get().presentation.snapshot.active?.captureId)
            assertEquals(CaptureState.RECORDING, repository.get(id)?.captureState)
        } finally {
            repository.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
            replacement.pause().stop().destroy()
        }
    }

    @Test fun pauseOnlyInterruptionDoesNotStopAnAttachedCapture() {
        val repository = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        repository.save(CaptureRecord(id, createdAtUtc = "2026-10-08T00:00:00Z", captureState = CaptureState.RECORDING))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            awaitCaptureIdle(controller.get())
            commands()
            controller.pause().resume()
            awaitCaptureIdle(controller.get())
            assertTrue("Temporary dialogs and canceled dismissal do not commit a stop at onPause", commands().isEmpty())
            assertEquals(CaptureState.RECORDING, repository.get(id)?.captureState)
        } finally {
            repository.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
            controller.pause().stop().destroy()
        }
    }

    private fun commands(): List<Intent> = buildList {
        while (true) add(shadowOf(context).nextStartedService ?: break)
    }
}
