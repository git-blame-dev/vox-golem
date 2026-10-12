package dev.gitblame.voxgolem.android.wear

import android.Manifest
import android.content.Intent
import android.os.Bundle
import android.os.Looper
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
class CaptureLifecycleGatesTest {
    private val context = RuntimeEnvironment.getApplication()

    @Test fun permissionDenialNeverStartsAServiceOrCreatesCaptureMetadata() {
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            action(controller.get(), "requestStart")
            val request = shadowOf(controller.get()).lastRequestedPermission
            assertArrayEquals(arrayOf(Manifest.permission.RECORD_AUDIO), request.requestedPermissions)
            assertTrue(controller.get().activityResultRegistry.dispatchResult(request.requestCode, false))
            assertTrue(commands().isEmpty())
            assertTrue(CaptureRepository(context).list().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun permissionGrantWhilePausedWaitsForPostedResumedForegroundStart() {
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            action(controller.get(), "requestStart")
            val request = shadowOf(controller.get()).lastRequestedPermission
            controller.pause()
            shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
            assertTrue(controller.get().activityResultRegistry.dispatchResult(request.requestCode, true))
            shadowOf(Looper.getMainLooper()).idle()
            assertTrue(commands().isEmpty())
            controller.get().setContentView(View(context))
            controller.resume()
            assertTrue(commands().isEmpty())
            controller.visible()
            shadowOf(Looper.getMainLooper()).idle()
            awaitCaptureIdle(controller.get())
            assertEquals(listOf(CaptureService.ACTION_START), commands().map { it.action })
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun pauseBeforePostedLauncherStartCancelsForegroundStartUntilNextResume() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN)).create().start()
        try {
            controller.get().setContentView(View(context))
            controller.resume().pause().visible()
            shadowOf(Looper.getMainLooper()).idle()
            assertTrue(commands().isEmpty())
            controller.resume()
            shadowOf(Looper.getMainLooper()).idle()
            awaitCaptureIdle(controller.get())
            assertEquals(listOf(CaptureService.ACTION_START), commands().map { it.action })
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun recreatedLauncherWithoutAnActiveCaptureOnlyObserves() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN))
            .create(Bundle()).start().resume()
        try {
            shadowOf(Looper.getMainLooper()).idle()
            assertTrue(commands().isEmpty())
            assertTrue(CaptureRepository(context).list().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun configurationStopDoesNotStopCaptureButRealUiInterruptionDoes() {
        val shared = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        shared.save(CaptureRecord(id, createdAtUtc = "2026-10-05T12:30:00Z", captureState = CaptureState.RECORDING))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        try {
            assertEquals(listOf(CaptureService.ACTION_ATTACH), commands().map { it.action })
            ReflectionHelpers.setField(controller.get(), "mChangingConfigurations", true)
            controller.pause().stop()
            assertTrue(commands().isEmpty())
            ReflectionHelpers.setField(controller.get(), "mChangingConfigurations", false)
            controller.restart().start().resume().pause()
            assertFalse(controller.get().isChangingConfigurations)
            assertNotNull(shared.active())
            controller.stop()
            val stop = commands().single()
            assertEquals(CaptureService.ACTION_STOP, stop.action)
            assertEquals("manual_stop", stop.getStringExtra(CaptureService.EXTRA_STOP_REASON))
        } finally {
            shared.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
            controller.destroy()
        }
    }

    @Test fun serviceStartWithoutPermissionCannotCreateAudioOrEnterForeground() {
        val controller = Robolectric.buildService(CaptureService::class.java).create()
        try {
            assertEquals(android.app.Service.START_NOT_STICKY,
                controller.get().onStartCommand(Intent(CaptureService.ACTION_START), 0, 1))
            assertTrue(CaptureRepository(context).list().isEmpty())
            assertTrue(shadowOf(controller.get()).isStoppedBySelf)
            assertNull(shadowOf(controller.get()).lastForegroundNotification)
        } finally { controller.destroy() }
    }

    @Test @Config(shadows = [RejectedForegroundContext::class])
    fun foregroundRestrictionKeepsTheActivityUsableWithoutInventingARecordingOrSavedState() {
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        try {
            controller.get().requestStart()
            awaitCaptureIdle(controller.get())
            assertEquals("Tap Capture to start recording", controller.get().presentation.feedback)
            assertTrue(CaptureRepository(context).list().isEmpty())
            assertNull(controller.get().presentation.savedFeedbackCaptureId)
            assertFalse(controller.get().isFinishing)
        } finally { controller.pause().stop().destroy() }
    }

    private fun action(activity: CaptureActivity, name: String) {
        check(name == "requestStart")
        activity.requestStart()
        awaitCaptureIdle(activity)
    }

    private fun commands(): List<Intent> = buildList {
        while (true) add(shadowOf(context).nextStartedService ?: break)
    }
}

@org.robolectric.annotation.Implements(className = "android.app.ContextImpl", isInAndroidSdk = false)
class RejectedForegroundContext {
    @org.robolectric.annotation.Implementation
    fun startForegroundService(intent: Intent): android.content.ComponentName {
        throw android.app.ForegroundServiceStartNotAllowedException("Synthetic background admission rejection: ${intent.action}")
    }
}
