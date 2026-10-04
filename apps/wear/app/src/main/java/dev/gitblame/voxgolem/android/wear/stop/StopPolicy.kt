package dev.gitblame.voxgolem.android.wear.stop

import kotlin.math.acos
import kotlin.math.sqrt

data class StopRequest(val captureId: String, val reason: String)

fun interface StopPolicy {
    fun onManualStop(captureId: String): StopRequest
}

object ManualStopPolicy : StopPolicy {
    override fun onManualStop(captureId: String) = StopRequest(captureId, "manual_stop")
}

/** Relative sample diagnostics only. No threshold is enabled as an arm-down stop without device trials. */
class MotionStopDiagnostics(private val recordSample: (String) -> Unit) {
    private var reference: FloatArray? = null
    private var lastSampleAtMs = 0L

    fun onGravitySample(captureId: String, x: Float, y: Float, z: Float, nowMs: Long) {
        if (nowMs - lastSampleAtMs < 45) return
        lastSampleAtMs = nowMs
        val magnitude = sqrt(x * x + y * y + z * z)
        if (magnitude !in 6f..13f) return
        if (reference == null) reference = floatArrayOf(x / magnitude, y / magnitude, z / magnitude)
        val ref = reference ?: return
        val cosine = ((x / magnitude) * ref[0] + (y / magnitude) * ref[1] + (z / magnitude) * ref[2]).coerceIn(-1f, 1f)
        val degrees = Math.toDegrees(acos(cosine.toDouble())).toFloat()
        recordSample("gravity_angle_degrees=${degrees.toInt()}")
    }

    fun reset() { reference = null; lastSampleAtMs = 0 }
}

class AmbientStopDiagnostics(private val recordEvent: (String) -> Unit) {
    fun onInteractiveChanged(interactive: Boolean, uptimeMs: Long) {
        recordEvent("interactive=$interactive;uptime_ms=$uptimeMs")
    }

    fun onAmbientChanged(ambient: Boolean, uptimeMs: Long) {
        recordEvent("ambient=$ambient;uptime_ms=$uptimeMs")
    }
}

/** A testable candidate detector; a null angle threshold disables automatic stopping. */
class ProvisionalMotionStopPolicy(
    private val loweringAngleDegrees: Float? = null,
    private val referenceDurationMs: Long = 500,
    private val loweringHoldMs: Long = 750,
) {
    private var referenceStartedAt: Long? = null
    private var referenceX = 0.0
    private var referenceY = 0.0
    private var referenceZ = 0.0
    private var referenceCount = 0
    private var referenceReady = false
    private var loweringStartedAt: Long? = null

    init {
        require(loweringAngleDegrees == null || loweringAngleDegrees in 1f..179f)
        require(referenceDurationMs > 0 && loweringHoldMs > 0)
    }

    fun onGravitySample(captureId: String, x: Float, y: Float, z: Float, elapsedMs: Long): StopRequest? {
        val magnitude = kotlin.math.sqrt(x * x + y * y + z * z)
        if (magnitude !in 6f..13f) return null
        if (loweringAngleDegrees == null) return null

        if (!referenceReady) {
            if (referenceCount == 0) referenceStartedAt = elapsedMs
            referenceX += x / magnitude
            referenceY += y / magnitude
            referenceZ += z / magnitude
            referenceCount++
            if (elapsedMs - (referenceStartedAt ?: elapsedMs) < referenceDurationMs) return null
            referenceReady = true
        }

        val baselineMagnitude = kotlin.math.sqrt(referenceX * referenceX + referenceY * referenceY + referenceZ * referenceZ)
        if (baselineMagnitude == 0.0) return null
        val cosine = ((x / magnitude * referenceX + y / magnitude * referenceY + z / magnitude * referenceZ) / baselineMagnitude)
            .coerceIn(-1.0, 1.0)
        val angle = Math.toDegrees(kotlin.math.acos(cosine)).toFloat()
        if (angle >= loweringAngleDegrees) {
            if (loweringStartedAt == null) loweringStartedAt = elapsedMs
            if (elapsedMs - (loweringStartedAt ?: elapsedMs) >= loweringHoldMs) {
                return StopRequest(captureId, "provisional_motion_lowering")
            }
        } else {
            loweringStartedAt = null
        }
        return null
    }

    fun reset() {
        referenceStartedAt = null
        referenceX = 0.0
        referenceY = 0.0
        referenceZ = 0.0
        referenceCount = 0
        referenceReady = false
        loweringStartedAt = null
    }
}
