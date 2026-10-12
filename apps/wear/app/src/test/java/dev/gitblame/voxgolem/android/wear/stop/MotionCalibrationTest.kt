package dev.gitblame.voxgolem.android.wear.stop

import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class MotionCalibrationTest {
    private fun pose(x: Float, y: Float, z: Float, count: Int = 30) = List(count) { floatArrayOf(x, y, z) }

    @Test fun stableDistinctPosturesProduceTheSameThresholdAfterRotationAndGravityScaleChange() {
        val first = MotionCalibrationMath.loweringThresholdDegrees(pose(0f, 0f, 9.8f), pose(9.8f, 0f, 0f))
        val rotated = MotionCalibrationMath.loweringThresholdDegrees(pose(0f, 8f, 0f), pose(0f, 0f, -12f))
        assertNotNull(first)
        assertEquals(first!!, rotated!!, 0.001f)
        assertTrue(first in 30f..90f)
        val preferences = MotionStopPreferences(RuntimeEnvironment.getApplication())
        preferences.enable(first)
        assertEquals(first, MotionStopPreferences(RuntimeEnvironment.getApplication()).thresholdDegrees)
        preferences.disable()
        assertNull(MotionStopPreferences(RuntimeEnvironment.getApplication()).thresholdDegrees)
    }

    @Test fun insufficientMalformedOrNonphysicalSamplesCannotEnableAutomaticStop() {
        val raised = pose(0f, 0f, 9.8f)
        val invalidPoses = listOf(
            pose(9.8f, 0f, 0f, 23),
            List(30) { floatArrayOf(9.8f, 0f) },
            pose(0f, 0f, 0f),
            pose(20f, 0f, 0f),
            pose(Float.NaN, 0f, 0f),
            pose(Float.POSITIVE_INFINITY, 0f, 0f),
        )
        for (invalid in invalidPoses) {
            assertNull(MotionCalibrationMath.loweringThresholdDegrees(raised, invalid))
            assertNull(MotionCalibrationMath.loweringThresholdDegrees(invalid, raised))
        }
    }

    @Test fun indistinctOrUnsteadyPosturesAreRejectedEvenWithEnoughSamples() {
        val raised = pose(0f, 0f, 9.8f)
        assertNull(MotionCalibrationMath.loweringThresholdDegrees(raised, raised))
        val moving = pose(9.8f, 0f, 0f).toMutableList().apply { this[15] = floatArrayOf(0f, 0f, 9.8f) }
        assertNull(MotionCalibrationMath.loweringThresholdDegrees(raised, moving))
    }

    @Test fun invalidPersistedThresholdsAreDisabledAndInvalidWritesAreRejected() {
        val context = RuntimeEnvironment.getApplication()
        val preferences = MotionStopPreferences(context)
        val storage = context.getSharedPreferences("motion-stop", 0)
        for (threshold in listOf(Float.NaN, Float.POSITIVE_INFINITY, -1f, 0f, 180f)) {
            assertTrue(storage.edit().putFloat("lowering_threshold_degrees", threshold).commit())
            assertNull(MotionStopPreferences(context).thresholdDegrees)
            assertThrows(IllegalArgumentException::class.java) { preferences.enable(threshold) }
            assertNull(preferences.thresholdDegrees)
        }
    }
}
