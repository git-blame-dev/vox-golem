package dev.gitblame.voxgolem.android.wear.stop

import android.content.Context
import android.content.pm.ApplicationInfo
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.os.SystemClock
import android.util.Log
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

internal class MotionCaptureController(
    context: Context,
    private val feedback: (String) -> Unit,
    private val calibrationChanged: (Boolean) -> Unit,
    private val stop: (String) -> Unit,
) : SensorEventListener {
    private val manager = context.getSystemService(Context.SENSOR_SERVICE) as SensorManager
    private val gravity = manager.getDefaultSensor(Sensor.TYPE_GRAVITY)
    private val preferences = MotionStopPreferences(context)
    private val diagnosticsEnabled = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0
    private val diagnostics = MotionStopDiagnostics(::diagnostic)
    private val ambient = AmbientStopDiagnostics(::diagnostic)
    var thresholdDegrees by mutableStateOf(preferences.thresholdDegrees)
        private set
    private var policy = ProvisionalMotionStopPolicy(thresholdDegrees)
    var stage by mutableStateOf(MotionCalibrationStage.NONE)
        private set
    private val samples = mutableStateListOf<FloatArray>()
    val sampleCount: Int get() = samples.size
    val needsCalibration: Boolean get() = gravity != null && thresholdDegrees == null && stage == MotionCalibrationStage.NONE
    private var raised: List<FloatArray>? = null
    private var samplesAfterMs = 0L
    private var registered = false
    private var captureId: String? = null

    fun update(resumed: Boolean, activeId: String?, readyId: String?) {
        captureId = activeId
        val shouldRegister = resumed && (activeId != null || stage.isSampling) && gravity != null
        if (shouldRegister && !registered) {
            diagnostics.reset(); policy.reset()
            manager.registerListener(this, gravity, 50_000)
            registered = true
        } else if (!shouldRegister && registered) {
            manager.unregisterListener(this)
            registered = false
            diagnostics.reset(); policy.reset()
        }
        if (shouldRegister && activeId != null && readyId == activeId) policy.onMicrophoneReady(activeId, SystemClock.elapsedRealtime())
    }
    fun resume() = ambient.onInteractiveChanged(true, SystemClock.elapsedRealtime())
    fun pause() {
        if (stage.isSampling) resetCalibration()
        update(false, null, null)
        ambient.onInteractiveChanged(false, SystemClock.elapsedRealtime())
    }
    fun resetCalibration() {
        stage = MotionCalibrationStage.NONE
        samples.clear(); raised = null; samplesAfterMs = 0L
        calibrationChanged(false)
    }
    fun action() {
        when (stage) {
            MotionCalibrationStage.NONE -> if (thresholdDegrees != null) {
                preferences.disable(); thresholdDegrees = null; policy = ProvisionalMotionStopPolicy()
                feedback("Arm-down stopping disabled · manual Stop remains available")
                diagnostic("motion_stop_disabled;uptime_ms=${SystemClock.elapsedRealtime()}")
            } else {
                raised = null; stage = MotionCalibrationStage.RAISED_READY
                feedback("Arm-down setup · microphone is off")
            }
            MotionCalibrationStage.RAISED_READY -> measure(MotionCalibrationStage.MEASURE_RAISED)
            MotionCalibrationStage.LOWERED_READY -> measure(MotionCalibrationStage.MEASURE_LOWERED)
            else -> Unit
        }
        calibrationChanged(stage != MotionCalibrationStage.NONE)
    }
    private fun measure(next: MotionCalibrationStage) {
        samples.clear(); stage = next
        samplesAfterMs = SystemClock.elapsedRealtime() + 2_000
        feedback("Two seconds to get into position, then hold steady")
    }
    override fun onSensorChanged(event: SensorEvent) {
        if (event.sensor.type != Sensor.TYPE_GRAVITY) return
        if (stage.isSampling) {
            if (SystemClock.elapsedRealtime() < samplesAfterMs) return
            if (samples.isEmpty()) feedback("Measuring posture · keep it steady")
            samples += event.values.copyOf()
            if (samples.size >= 30) finishMeasurement()
            return
        }
        val id = captureId ?: return
        if (diagnosticsEnabled) diagnostics.onGravitySample(id, event.values[0], event.values[1], event.values[2], SystemClock.elapsedRealtime())
        policy.onGravitySample(id, event.values[0], event.values[1], event.values[2], event.timestamp / 1_000_000L)?.let { stop(it.reason) }
    }
    override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) = Unit
    private fun finishMeasurement() {
        when (stage) {
            MotionCalibrationStage.MEASURE_RAISED -> {
                raised = samples.toList(); samples.clear(); stage = MotionCalibrationStage.LOWERED_READY
                feedback("Now hold the deliberate lowered posture you intend to use")
            }
            MotionCalibrationStage.MEASURE_LOWERED -> {
                val threshold = raised?.let { MotionCalibrationMath.loweringThresholdDegrees(it, samples) }
                if (threshold == null) {
                    preferences.disable(); thresholdDegrees = null; policy = ProvisionalMotionStopPolicy()
                    feedback("Postures were not distinct and steady; motion stop stays off")
                    diagnostic("motion_calibration_rejected;manual_stop_only")
                } else {
                    preferences.enable(threshold); thresholdDegrees = threshold; policy = ProvisionalMotionStopPolicy(threshold)
                    feedback("Arm-down stop enabled · raise to speaking posture before starting capture")
                    diagnostic("motion_calibration_saved;threshold_degrees=${threshold.toInt()};physical_verification=pending")
                }
                raised = null; samples.clear(); stage = MotionCalibrationStage.NONE
            }
            else -> Unit
        }
        calibrationChanged(stage != MotionCalibrationStage.NONE)
    }
    private fun diagnostic(entry: String) { if (diagnosticsEnabled) Log.d("VoxGolemStopPolicy", entry) }
}

internal enum class MotionCalibrationStage(val prompt: String?, val isSampling: Boolean) {
    NONE(null, false),
    RAISED_READY("Hold the raised dictation posture still, then measure it", false),
    MEASURE_RAISED("Measuring raised posture…", true),
    LOWERED_READY("Hold the deliberate lowered posture still, then measure it", false),
    MEASURE_LOWERED("Measuring lowered posture…", true);
    val canMeasure: Boolean get() = this == RAISED_READY || this == LOWERED_READY
}
