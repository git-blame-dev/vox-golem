package dev.gitblame.voxgolem.android.wear.stop

import android.content.Context
import kotlin.math.acos
import kotlin.math.sqrt

object MotionCalibrationMath {
    private const val MIN_SAMPLES = 24
    private const val MAX_POSE_DEVIATION_DEGREES = 12.0
    private const val MIN_POSTURE_SEPARATION_DEGREES = 45.0

    /** Returns a user-specific trigger angle only when both measured poses are steady and distinct. */
    fun loweringThresholdDegrees(raisedSamples: List<FloatArray>, loweredSamples: List<FloatArray>): Float? {
        val raised = steadyPose(raisedSamples) ?: return null
        val lowered = steadyPose(loweredSamples) ?: return null
        val separation = angleBetween(raised, lowered)
        if (separation < MIN_POSTURE_SEPARATION_DEGREES) return null
        return (separation * 0.65).toFloat().coerceAtLeast(30f)
    }

    private fun steadyPose(samples: List<FloatArray>): DoubleArray? {
        if (samples.size < MIN_SAMPLES) return null
        val normalized = ArrayList<DoubleArray>(samples.size)
        for (sample in samples) {
            if (sample.size < 3) return null
            val magnitude = sqrt(sample[0] * sample[0] + sample[1] * sample[1] + sample[2] * sample[2])
            if (magnitude !in 6f..13f) return null
            normalized += doubleArrayOf(
                (sample[0] / magnitude).toDouble(),
                (sample[1] / magnitude).toDouble(),
                (sample[2] / magnitude).toDouble(),
            )
        }
        val mean = DoubleArray(3) { axis -> normalized.sumOf { it[axis] } / normalized.size }
        val meanMagnitude = sqrt(mean.sumOf { it * it })
        if (meanMagnitude == 0.0) return null
        val unitMean = DoubleArray(3) { mean[it] / meanMagnitude }
        if (normalized.any { angleBetween(unitMean, it) > MAX_POSE_DEVIATION_DEGREES }) return null
        return unitMean
    }

    private fun angleBetween(first: DoubleArray, second: DoubleArray): Double {
        val dot = (0..2).sumOf { first[it] * second[it] }.coerceIn(-1.0, 1.0)
        return Math.toDegrees(acos(dot))
    }
}

class MotionStopPreferences(context: Context) {
    private val preferences = context.applicationContext.getSharedPreferences("motion-stop", Context.MODE_PRIVATE)

    val thresholdDegrees: Float?
        get() = preferences.getFloat(KEY_THRESHOLD, Float.NaN).takeIf { it.isFinite() && it in 1f..179f }

    fun enable(thresholdDegrees: Float) {
        require(thresholdDegrees in 1f..179f)
        check(preferences.edit().putFloat(KEY_THRESHOLD, thresholdDegrees).commit())
    }

    fun disable() {
        check(preferences.edit().remove(KEY_THRESHOLD).commit())
    }

    private companion object { const val KEY_THRESHOLD = "lowering_threshold_degrees" }
}
