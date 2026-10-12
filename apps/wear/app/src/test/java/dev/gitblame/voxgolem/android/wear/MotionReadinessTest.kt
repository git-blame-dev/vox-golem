package dev.gitblame.voxgolem.android.wear

import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.hardware.Sensor
import android.hardware.SensorManager
import dev.gitblame.voxgolem.android.wear.capture.CaptureService
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.android.wear.stop.MotionStopPreferences
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
import org.robolectric.shadows.ShadowSensor
import org.robolectric.shadows.ShadowSensorManager
import org.robolectric.shadows.ShadowSystemClock
import org.robolectric.shadows.ShadowLog
import java.time.Duration
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class MotionReadinessTest {
    @Test fun loweringIsInertBeforeActualReadinessThenRoutesOneStopForTheActiveCapture() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE.inv()
        ShadowLog.clear()
        val shared = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        shared.save(CaptureRecord(id, createdAtUtc = "2026-10-05T12:30:00Z", captureState = CaptureState.RECORDING))
        MotionStopPreferences(context).enable(40f)
        val manager = shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager)
        val sensor = ShadowSensor.newInstance(Sensor.TYPE_GRAVITY)
        manager.addSensor(sensor)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        try {
            samples(manager, sensor, 0f, 9.8f, 14)
            samples(manager, sensor, 9.8f, 0f, 18)
            assertTrue(startedStops(context).isEmpty())
            CaptureSignals.ready(context, id)
            shadowOf(android.os.Looper.getMainLooper()).idle()
            awaitCaptureIdle(controller.get())
            samples(manager, sensor, 0f, 9.8f, 14)
            samples(manager, sensor, 9.8f, 0f, 22)
            val stops = startedStops(context)
            assertEquals(1, stops.size)
            assertEquals("provisional_motion_lowering", stops.single().getStringExtra(CaptureService.EXTRA_STOP_REASON))
            assertTrue("Optimized builds must not emit per-sample experimental diagnostics", ShadowLog.getLogsForTag("VoxGolemStopPolicy").isEmpty())
            assertFalse(context.getFileStreamPath("stop-policy-diagnostics.log").exists())
        } finally {
            shared.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
            controller.pause().stop().destroy()
        }
    }

    @Test fun staleReadinessCannotArmTheCurrentCapture() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        ShadowLog.clear()
        val shared = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        shared.save(CaptureRecord(id, createdAtUtc = "2026-10-05T12:30:00Z", captureState = CaptureState.RECORDING))
        MotionStopPreferences(context).enable(40f)
        val manager = shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager)
        val sensor = ShadowSensor.newInstance(Sensor.TYPE_GRAVITY)
        manager.addSensor(sensor)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        try {
            CaptureSignals.ready(context, UUID.randomUUID().toString())
            shadowOf(android.os.Looper.getMainLooper()).idle()
            awaitCaptureIdle(controller.get())
            samples(manager, sensor, 0f, 9.8f, 14)
            samples(manager, sensor, 9.8f, 0f, 22)
            assertTrue(startedStops(context).isEmpty())
            assertTrue("Debug builds retain experimental gravity diagnostics", ShadowLog.getLogsForTag("VoxGolemStopPolicy").any {
                it.msg.startsWith("gravity_angle_degrees=")
            })
            assertFalse("Diagnostics must not perform synchronous per-sample file writes", context.getFileStreamPath("stop-policy-diagnostics.log").exists())
        } finally {
            shared.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
            controller.pause().stop().destroy()
        }
    }

    @Test fun acquisitionGapsAndBackwardSamplesBreakHoldsEvenWhenDeliveryLooksContinuous() {
        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        shared.save(CaptureRecord(id, createdAtUtc = "2026-10-05T12:30:00Z", captureState = CaptureState.RECORDING))
        MotionStopPreferences(context).enable(40f)
        val manager = shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager)
        val sensor = ShadowSensor.newInstance(Sensor.TYPE_GRAVITY)
        manager.addSensor(sensor)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        awaitCaptureIdle(controller.get())
        try {
            CaptureSignals.ready(context, id)
            shadowOf(android.os.Looper.getMainLooper()).idle()
            awaitCaptureIdle(controller.get())
            samples(manager, sensor, 0f, 9.8f, 14)
            val start = android.os.SystemClock.elapsedRealtime()
            repeat(13) { index ->
                ShadowSystemClock.advanceBy(Duration.ofMillis(50))
                event(manager, sensor, 9.8f, 0f, (start + (index + 1) * 10) * 1_000_000L)
            }
            samples(manager, sensor, 9.8f, 0f, 5)
            assertTrue("Acquisition gap was treated as a continuous lowering hold", startedStops(context).isEmpty())
            samples(manager, sensor, 9.8f, 0f, 8)
            ShadowSystemClock.advanceBy(Duration.ofMillis(50))
            event(manager, sensor, 9.8f, 0f, android.os.SystemClock.elapsedRealtimeNanos() - 500_000_000L)
            samples(manager, sensor, 9.8f, 0f, 4)
            assertTrue("Backward acquisition timestamp did not interrupt the hold", startedStops(context).isEmpty())
        } finally {
            shared.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
            controller.pause().stop().destroy()
        }
    }

    private fun samples(manager: ShadowSensorManager, sensor: Sensor, x: Float, z: Float, count: Int) {
        repeat(count) {
            ShadowSystemClock.advanceBy(Duration.ofMillis(50))
            event(manager, sensor, x, z, android.os.SystemClock.elapsedRealtimeNanos())
        }
    }

    @Test fun preparingAttachmentCannotArmListeningAndManualStopImmediatelyDisablesListeningProjection() {
        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        shared.save(CaptureRecord(id, createdAtUtc = "2026-10-05T12:30:00Z", captureState = CaptureState.RECORDING))
        MotionStopPreferences(context).enable(40f)
        val manager = shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager)
        val sensor = ShadowSensor.newInstance(Sensor.TYPE_GRAVITY)
        manager.addSensor(sensor)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        fun listening(): Boolean { awaitCaptureIdle(controller.get()); return controller.get().presentation.backgroundListening }
        try {
            CaptureSignals.attached(context, id, false)
            shadowOf(android.os.Looper.getMainLooper()).idle()
            assertFalse(listening())
            samples(manager, sensor, 0f, 9.8f, 14)
            samples(manager, sensor, 9.8f, 0f, 22)
            assertTrue(startedStops(context).isEmpty())
            CaptureSignals.attached(context, id, true)
            shadowOf(android.os.Looper.getMainLooper()).idle()
            assertTrue(listening())
            controller.pause()
            assertFalse(listening())
            controller.resume()
            assertTrue(listening())
            controller.get().stopCapture()
            assertFalse(listening())
            assertEquals(CaptureService.ACTION_STOP, startedStops(context).single().action)
        } finally {
            shared.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
            controller.pause().stop().destroy()
        }
    }

    private fun event(manager: ShadowSensorManager, sensor: Sensor, x: Float, z: Float, timestamp: Long) {
        val sample = ShadowSensorManager.createSensorEvent(3)
        sample.sensor = sensor
        sample.timestamp = timestamp
        sample.values[0] = x; sample.values[1] = 0f; sample.values[2] = z
        manager.sendSensorEventToListeners(sample)
    }

    private fun startedStops(context: android.app.Application): List<Intent> {
        val result = mutableListOf<Intent>()
        while (true) {
            val next = shadowOf(context).nextStartedService ?: break
            if (next.action == CaptureService.ACTION_STOP) result += next
        }
        return result
    }
}
