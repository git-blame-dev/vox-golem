package dev.gitblame.voxgolem.android.wear

import android.Manifest
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.os.Bundle
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureRepository
import dev.gitblame.voxgolem.capture.CaptureStartupTiming
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.CaptureTiming
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLog
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class StartupTimingLifecycleTest {
    @Test fun warmLauncherIntentIncludesSnapshotAndNextDrawMarkers() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        try {
            ShadowLog.clear()
            controller.newIntent(Intent(Intent.ACTION_MAIN))
            awaitCaptureIdle(controller.get())
            controller.get().window.decorView.viewTreeObserver.dispatchOnPreDraw()
            val logs = ShadowLog.getLogsForTag(CaptureTiming.TAG)
            assertTrue(logs.any { it.msg.contains("stage=activity-entry") })
            assertTrue(logs.any { it.msg.contains("stage=initial-snapshot") })
            assertTrue(logs.any { it.msg.contains("stage=ui-pre-draw") })
        } finally {
            CaptureStartupTiming.current()?.finish("synthetic-test-finished")
            controller.pause().stop().destroy()
        }
    }

    @Test fun recreationReattachesTheCaptureTimingBeforeItsFirstPreview() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        val shared = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        shared.save(CaptureRecord(id, createdAtUtc = "2026-10-05T12:30:00Z", captureState = CaptureState.RECORDING, observedFrames = 1))
        val timing = CaptureStartupTiming.begin(context)
        CaptureStartupTiming.bind(id)
        val first = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(first.get())
        val saved = Bundle()
        first.saveInstanceState(saved).pause().stop().destroy()
        val restored = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create(saved).start().resume()
        awaitCaptureIdle(restored.get())
        try {
            assertSame(timing, CaptureStartupTiming.forCapture(id))
            restored.get().window.decorView.viewTreeObserver.dispatchOnPreDraw()
            assertTrue(ShadowLog.getLogsForTag(CaptureTiming.TAG).any { it.msg.contains("stage=ui-recreated-pre-draw") })
            ShadowLog.clear()
            shared.update(id) { it.copy(previewText = "synthetic first preview") }
            context.sendBroadcast(Intent(CaptureSignals.SIGNAL_ACTION).setPackage(context.packageName))
            shadowOf(android.os.Looper.getMainLooper()).idle()
            awaitCaptureIdle(restored.get())
            assertTrue(ShadowLog.getLogsForTag(CaptureTiming.TAG).any { it.msg.contains("stage=preview-observed") })
        } finally {
            timing.finish("synthetic-test-finished")
            shared.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
            restored.pause().stop().destroy()
        }
    }

    @Test fun recreationBeforeServicePersistenceReattachesWhenTheCaptureArrives() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        val shared = CaptureRepository(context)
        val first = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(first.get())
        first.newIntent(Intent(Intent.ACTION_MAIN))
        awaitCaptureIdle(first.get())
        val timing = CaptureStartupTiming.current()!!
        val saved = Bundle()
        first.saveInstanceState(saved).pause().stop().destroy()
        val restored = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create(saved).start().resume()
        val id = UUID.randomUUID().toString()
        try {
            shared.save(CaptureRecord(id, createdAtUtc = "2026-10-05T12:30:00Z", captureState = CaptureState.RECORDING,
                observedFrames = 1, previewText = "synthetic delayed preview"))
            CaptureStartupTiming.bind(id)
            CaptureSignals.ready(context, id)
            shadowOf(android.os.Looper.getMainLooper()).idle()
            awaitCaptureIdle(restored.get())
            restored.get().window.decorView.viewTreeObserver.dispatchOnPreDraw()
            val logs = ShadowLog.getLogsForTag(CaptureTiming.TAG)
            assertSame(timing, CaptureStartupTiming.forCapture(id))
            assertTrue(logs.any { it.msg.contains("stage=ui-recreated-pre-draw") })
            assertTrue(logs.any { it.msg.contains("stage=preview-observed") })
        } finally {
            timing.finish("synthetic-test-finished")
            shared.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
            restored.pause().stop().destroy()
        }
    }
}
