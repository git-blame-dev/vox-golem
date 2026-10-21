package dev.gitblame.voxgolem.android.wear.stop

import android.content.Context
import android.content.pm.ApplicationInfo
import android.hardware.Sensor
import android.hardware.SensorManager
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowSensor

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class MotionCaptureSubscriptionTest {
    @Test fun disabledOptimizedCaptureDoesNotSubscribeButCalibrationStillDoes() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE.inv()
        MotionStopPreferences(context).disable()
        val manager = shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager)
        manager.addSensor(ShadowSensor.newInstance(Sensor.TYPE_GRAVITY))
        val owner = MotionCaptureController(context, {}, {}, {})
        try {
            owner.update(true, "00000000-0000-4000-8000-000000000001", null)
            assertFalse(manager.hasListener(owner))
            owner.action()
            owner.action()
            owner.update(true, null, null)
            assertTrue(manager.hasListener(owner))
            owner.resetCalibration()
            owner.update(true, null, null)
            assertFalse(manager.hasListener(owner))
        } finally { owner.pause() }
    }

    @Test fun enabledStoppingAndExplicitDebugDiagnosticsRetainTheirSubscriptions() {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE.inv()
        MotionStopPreferences(context).enable(40f)
        val manager = shadowOf(context.getSystemService(Context.SENSOR_SERVICE) as SensorManager)
        manager.addSensor(ShadowSensor.newInstance(Sensor.TYPE_GRAVITY))
        val owner = MotionCaptureController(context, {}, {}, {})
        try {
            owner.update(true, "00000000-0000-4000-8000-000000000002", null)
            assertTrue(manager.hasListener(owner))
            owner.action()
            owner.update(true, "00000000-0000-4000-8000-000000000002", null)
            assertFalse(manager.hasListener(owner))
        } finally { owner.pause() }

        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        val diagnosticOwner = MotionCaptureController(context, {}, {}, {})
        try {
            diagnosticOwner.update(true, "00000000-0000-4000-8000-000000000003", null)
            assertTrue(manager.hasListener(diagnosticOwner))
            diagnosticOwner.pause()
            assertFalse(manager.hasListener(diagnosticOwner))
        } finally { diagnosticOwner.pause() }
    }
}
