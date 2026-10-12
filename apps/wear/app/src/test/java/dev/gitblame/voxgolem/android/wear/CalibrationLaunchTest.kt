package dev.gitblame.voxgolem.android.wear

import android.Manifest
import android.content.Context
import android.content.Intent
import android.hardware.Sensor
import android.hardware.SensorManager
import dev.gitblame.voxgolem.android.wear.capture.CaptureService
import dev.gitblame.voxgolem.android.wear.stop.MotionStopPreferences
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
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CalibrationLaunchTest {
    @Test fun unconfiguredLauncherMakesCalibrationReachableBeforeOpeningTheMicrophone() {
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        MotionStopPreferences(context).disable()
        val sensor = ShadowSensor.newInstance(Sensor.TYPE_GRAVITY)
        val manager = shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager)
        manager.addSensor(sensor)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN)).create().start().resume()
        try {
            assertEquals("RAISED_READY", stage(controller.get()))
            assertTrue(startCommands().isEmpty())
            action(controller.get(), "onMotionAction")
            samples(manager, sensor, 0f, 9.8f, 30)
            assertEquals("MEASURE_RAISED", stage(controller.get()))
            ShadowSystemClock.advanceBy(Duration.ofSeconds(2))
            samples(manager, sensor, 0f, 9.8f, 30)
            assertEquals("LOWERED_READY", stage(controller.get()))
            action(controller.get(), "onMotionAction")
            ShadowSystemClock.advanceBy(Duration.ofSeconds(2))
            samples(manager, sensor, 9.8f, 0f, 30)
            assertEquals("NONE", stage(controller.get()))
            assertNotNull(MotionStopPreferences(context).thresholdDegrees)
            assertTrue("Calibration must not start capture with the hand still lowered", startCommands().isEmpty())
            action(controller.get(), "requestStart")
            assertEquals(1, startCommands().size)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun manualOptionExplicitlySkipsSetupWithoutEnablingMotion() {
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        MotionStopPreferences(context).disable()
        shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager).addSensor(ShadowSensor.newInstance(Sensor.TYPE_GRAVITY))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN)).create().start().resume()
        try {
            assertEquals("RAISED_READY", stage(controller.get()))
            action(controller.get(), "recordWithManualStop")
            assertEquals("NONE", stage(controller.get()))
            assertNull(MotionStopPreferences(context).thresholdDegrees)
            assertEquals(1, startCommands().size)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun calibratedLauncherStillStartsCaptureDirectly() {
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        MotionStopPreferences(context).enable(40f)
        shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager).addSensor(ShadowSensor.newInstance(Sensor.TYPE_GRAVITY))
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN)).create().start()
        try {
            assertEquals("NONE", stage(controller.get()))
            assertTrue("The launcher must wait until resumed before starting a foreground microphone service", startCommands().isEmpty())
            // This lifecycle fixture has no packaged Compose resources, but needs an attached decor view.
            controller.get().setContentView(android.view.View(context))
            controller.resume()
            assertTrue("The start is posted after resume for foreground eligibility", startCommands().isEmpty())
            controller.visible()
            shadowOf(android.os.Looper.getMainLooper()).idle()
            awaitCaptureIdle(controller.get())
            assertEquals(1, startCommands().size)
            controller.pause().resume()
            shadowOf(android.os.Looper.getMainLooper()).idle()
            assertTrue("An ordinary resume must not create another capture", startCommands().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    private fun stage(activity: CaptureActivity): String {
        awaitCaptureIdle(activity)
        return activity.motion.stage.toString()
    }

    @Test fun calibrationBlocksStartRequestsAndPauseDiscardsAnIncompleteMeasurement() {
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        MotionStopPreferences(context).disable()
        val sensor = ShadowSensor.newInstance(Sensor.TYPE_GRAVITY)
        val manager = shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager)
        manager.addSensor(sensor)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN)).create().start().resume()
        awaitCaptureIdle(controller.get())
        try {
            action(controller.get(), "requestStart")
            assertTrue(startCommands().isEmpty())
            action(controller.get(), "onMotionAction")
            ShadowSystemClock.advanceBy(Duration.ofSeconds(2))
            samples(manager, sensor, 0f, 9.8f, 10)
            controller.pause().resume()
            samples(manager, sensor, 9.8f, 0f, 30)
            assertEquals("NONE", stage(controller.get()))
            assertNull(MotionStopPreferences(context).thresholdDegrees)
            assertTrue(startCommands().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun indistinctMeasuredPosturesKeepMotionDisabledAndNeverStartMicrophone() {
        val context = RuntimeEnvironment.getApplication()
        MotionStopPreferences(context).disable()
        val sensor = ShadowSensor.newInstance(Sensor.TYPE_GRAVITY)
        val manager = shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager)
        manager.addSensor(sensor)
        val controller = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN)).create().start().resume()
        awaitCaptureIdle(controller.get())
        try {
            repeat(2) {
                action(controller.get(), "onMotionAction")
                ShadowSystemClock.advanceBy(Duration.ofSeconds(2))
                samples(manager, sensor, 0f, 9.8f, 30)
            }
            assertEquals("NONE", stage(controller.get()))
            assertNull(MotionStopPreferences(context).thresholdDegrees)
            assertTrue(startCommands().isEmpty())
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun recreatedActivityReloadsSavedMotionThresholdWithoutStartingCapture() {
        val context = RuntimeEnvironment.getApplication()
        MotionStopPreferences(context).enable(55f)
        val saved = android.os.Bundle()
        val first = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_VIEW)).create().start().resume()
        first.saveInstanceState(saved).pause().stop().destroy()
        MotionStopPreferences(context).enable(65f)
        val restored = Robolectric.buildActivity(CaptureActivity::class.java, Intent(Intent.ACTION_MAIN)).create(saved).start().resume()
        try {
            val threshold = restored.get().motion.thresholdDegrees
            assertEquals(65f, threshold)
            assertEquals("NONE", stage(restored.get()))
            assertTrue(startCommands().isEmpty())
        } finally { restored.pause().stop().destroy() }
    }

    private fun action(activity: CaptureActivity, name: String) {
        when (name) {
            "onMotionAction" -> activity.onMotionAction()
            "requestStart" -> activity.requestStart()
            "recordWithManualStop" -> activity.recordWithManualStop()
            else -> error(name)
        }
        awaitCaptureIdle(activity)
    }

    private fun samples(manager: ShadowSensorManager, sensor: Sensor, x: Float, z: Float, count: Int) {
        repeat(count) {
            ShadowSystemClock.advanceBy(Duration.ofMillis(50))
            val event = ShadowSensorManager.createSensorEvent(3)
            event.sensor = sensor; event.timestamp = android.os.SystemClock.elapsedRealtimeNanos()
            event.values[0] = x; event.values[1] = 0f; event.values[2] = z
            manager.sendSensorEventToListeners(event)
        }
    }

    private fun startCommands(): List<Intent> {
        val result = mutableListOf<Intent>()
        while (true) {
            val next = shadowOf(RuntimeEnvironment.getApplication()).nextStartedService ?: break
            if (next.action == CaptureService.ACTION_START) result += next
        }
        return result
    }
}
