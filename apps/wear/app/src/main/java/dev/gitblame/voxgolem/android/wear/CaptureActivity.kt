package dev.gitblame.voxgolem.android.wear

import android.Manifest
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.wear.compose.material.Button
import androidx.wear.compose.material.CompactButton
import androidx.wear.compose.material.MaterialTheme
import androidx.wear.compose.material.Text
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.Lifecycle
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import dev.gitblame.voxgolem.android.wear.capture.CaptureService
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.android.wear.capture.CaptureSnapshot
import dev.gitblame.voxgolem.android.wear.stop.AmbientStopDiagnostics
import dev.gitblame.voxgolem.android.wear.stop.MotionStopDiagnostics
import dev.gitblame.voxgolem.android.wear.stop.MotionCalibrationMath
import dev.gitblame.voxgolem.android.wear.stop.MotionStopPreferences
import dev.gitblame.voxgolem.android.wear.stop.ProvisionalMotionStopPolicy
import dev.gitblame.voxgolem.android.wear.transport.WearCaptureTransport
import dev.gitblame.voxgolem.capture.CaptureState
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

class CaptureActivity : ComponentActivity(), SensorEventListener {
    private val repository by lazy { CaptureRepository(this) }
    private var snapshot by mutableStateOf(CaptureSnapshot(null, emptyList()))
    private var feedback by mutableStateOf("")
    private val ambientDiagnostics = AmbientStopDiagnostics(::storeDiagnostic)
    private val motionDiagnostics = MotionStopDiagnostics(::storeDiagnostic)
    private val motionPreferences by lazy { MotionStopPreferences(this) }
    private var motionThresholdDegrees by mutableStateOf<Float?>(null)
    private var provisionalMotionStopPolicy = ProvisionalMotionStopPolicy()
    private var calibrationStage by mutableStateOf(MotionCalibrationStage.NONE)
    private val calibrationSamples = ArrayList<FloatArray>()
    private var raisedPostureSamples: List<FloatArray>? = null
    private var resumed = false
    private var motionRegistered = false
    private lateinit var sensorManager: SensorManager
    private var gravity: Sensor? = null

    private val permission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) startCaptureOnce() else feedback = "Microphone permission is required"
    }

    private val signals = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            val state = intent?.getStringExtra("state")
            if (state == null) { refresh(); return }
            feedback = when (state) {
                "ready" -> "Microphone ready"
                "saved" -> "Saved on watch · sync pending"
                "recovered" -> intent?.getStringExtra("message") ?: "Previous capture recovered"
                "failed" -> intent?.getStringExtra("message") ?: "Capture needs attention"
                else -> "Capture needs attention"
            }
            refresh()
            if (state == "saved") window.decorView.postDelayed({
                if (repository.active() == null) finish()
            }, 1_200)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        sensorManager = getSystemService(SENSOR_SERVICE) as SensorManager
        gravity = sensorManager.getDefaultSensor(Sensor.TYPE_GRAVITY)
        motionThresholdDegrees = motionPreferences.thresholdDegrees
        provisionalMotionStopPolicy = ProvisionalMotionStopPolicy(motionThresholdDegrees)
        ContextCompat.registerReceiver(this, signals, IntentFilter(CaptureSignals.SIGNAL_ACTION), ContextCompat.RECEIVER_NOT_EXPORTED)
        refresh()
        setContent {
            MaterialTheme {
                Box(Modifier.fillMaxSize()) {
                    CaptureScreen(
                        snapshot = snapshot,
                        feedback = feedback,
                        motionStopEnabled = motionThresholdDegrees != null,
                        calibrationPrompt = calibrationStage.prompt,
                        calibrationCanMeasure = calibrationStage.canMeasure,
                        calibrationSampleCount = calibrationSamples.size,
                        onStart = ::requestStart,
                        onStop = ::stopCapture,
                        onClose = ::finish,
                        onMotionAction = ::onMotionAction,
                        onRetryRecording = ::retryRecording,
                    )
                }
            }
        }
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.RESUMED) {
                while (true) { refresh(); delay(500) }
            }
        }
        // Only a fresh, deliberate launcher invocation starts capture. Resume/recreation only observes.
        if (savedInstanceState == null && intent?.action == Intent.ACTION_MAIN) requestStart()
        else if (repository.active() != null) attachToCaptureService()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (intent.action == Intent.ACTION_MAIN) requestStart()
        refresh()
    }

    private fun requestStart() {
        if (repository.active() != null) { attachToCaptureService(); refresh(); return }
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) {
            startCaptureOnce()
        } else permission.launch(Manifest.permission.RECORD_AUDIO)
    }

    private fun startCaptureOnce() {
        if (repository.active() != null) { attachToCaptureService(); refresh(); return }
        val start = Intent(this, CaptureService::class.java).setAction(CaptureService.ACTION_START)
        if (Build.VERSION.SDK_INT >= 26) startForegroundService(start) else startService(start)
        feedback = "Preparing microphone…"
        refresh()
    }

    private fun attachToCaptureService() {
        startService(Intent(this, CaptureService::class.java).setAction(CaptureService.ACTION_ATTACH))
    }

    private fun stopCapture() {
        startService(Intent(this, CaptureService::class.java).setAction(CaptureService.ACTION_STOP))
        feedback = "Saving recording…"
    }

    private fun refresh() {
        snapshot = CaptureSnapshot(repository.active(), repository.records().take(12))
        updateMotionDiagnostics()
    }

    private fun retryRecording(capture: dev.gitblame.voxgolem.capture.CaptureRecord) {
        feedback = "Repairing interrupted recording…"
        lifecycleScope.launch(Dispatchers.IO) {
            val repaired = runCatching { repository.retryInterruptedFinalization(capture.captureId) }.getOrDefault(false)
            if (repaired) repository.get(capture.captureId)?.let { WearCaptureTransport(this@CaptureActivity).publishPending(it) }
            withContext(Dispatchers.Main) {
                feedback = if (repaired) "Recovered audio · finalize it on the phone" else "Recording repair needs attention · original audio retained"
                refresh()
            }
        }
    }

    override fun onResume() {
        super.onResume()
        WearCaptureTransport(this).retryPending()
        resumed = true
        ambientDiagnostics.onInteractiveChanged(true, SystemClock.elapsedRealtime())
        updateMotionDiagnostics()
    }

    override fun onPause() {
        resumed = false
        if (calibrationStage.isSampling) {
            calibrationStage = MotionCalibrationStage.NONE
            calibrationSamples.clear()
            raisedPostureSamples = null
        }
        updateMotionDiagnostics()
        ambientDiagnostics.onInteractiveChanged(false, SystemClock.elapsedRealtime())
        sensorManager.unregisterListener(this)
        super.onPause()
    }

    override fun onDestroy() {
        unregisterReceiver(signals)
        super.onDestroy()
    }

    override fun onSensorChanged(event: SensorEvent) {
        if (event.sensor.type == Sensor.TYPE_GRAVITY && calibrationStage.isSampling) {
            calibrationSamples += event.values.copyOf()
            if (calibrationSamples.size >= CALIBRATION_SAMPLE_COUNT) finishPostureMeasurement()
            return
        }
        val captureId = snapshot.active?.captureId
        if (captureId != null && event.sensor.type == Sensor.TYPE_GRAVITY) {
            motionDiagnostics.onGravitySample(captureId, event.values[0], event.values[1], event.values[2], SystemClock.elapsedRealtime())
            val stopRequest = provisionalMotionStopPolicy.onGravitySample(
                captureId, event.values[0], event.values[1], event.values[2], SystemClock.elapsedRealtime(),
            )
            if (stopRequest != null) {
                startService(Intent(this, CaptureService::class.java).setAction(CaptureService.ACTION_STOP)
                    .putExtra(CaptureService.EXTRA_STOP_REASON, stopRequest.reason))
            }
        }
    }
    override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) = Unit

    private fun storeDiagnostic(entry: String) {
        runCatching { openFileOutput("stop-policy-diagnostics.log", MODE_APPEND).bufferedWriter().use { it.appendLine(entry) } }
    }

    private fun updateMotionDiagnostics() {
        val shouldRegister = resumed && (snapshot.active != null || calibrationStage.isSampling) && gravity != null
        if (shouldRegister && !motionRegistered) {
            motionDiagnostics.reset()
            provisionalMotionStopPolicy.reset()
            sensorManager.registerListener(this, gravity, 50_000)
            motionRegistered = true
        } else if (!shouldRegister && motionRegistered) {
            sensorManager.unregisterListener(this)
            motionRegistered = false
            motionDiagnostics.reset()
            provisionalMotionStopPolicy.reset()
        }
    }

    private fun onMotionAction() {
        when (calibrationStage) {
            MotionCalibrationStage.NONE -> {
                if (motionThresholdDegrees != null) {
                    motionPreferences.disable()
                    motionThresholdDegrees = null
                    provisionalMotionStopPolicy = ProvisionalMotionStopPolicy()
                    feedback = "Arm-down stopping disabled · manual Stop remains available"
                    storeDiagnostic("motion_stop_disabled;uptime_ms=${SystemClock.elapsedRealtime()}")
                } else {
                    raisedPostureSamples = null
                    calibrationStage = MotionCalibrationStage.RAISED_READY
                    feedback = "First hold your usual raised dictation posture still"
                }
            }
            MotionCalibrationStage.RAISED_READY -> startPostureMeasurement(MotionCalibrationStage.MEASURE_RAISED)
            MotionCalibrationStage.LOWERED_READY -> startPostureMeasurement(MotionCalibrationStage.MEASURE_LOWERED)
            else -> Unit
        }
        updateMotionDiagnostics()
    }

    private fun startPostureMeasurement(stage: MotionCalibrationStage) {
        calibrationSamples.clear()
        calibrationStage = stage
        feedback = "Keep this posture steady while the watch measures"
        updateMotionDiagnostics()
    }

    private fun finishPostureMeasurement() {
        when (calibrationStage) {
            MotionCalibrationStage.MEASURE_RAISED -> {
                raisedPostureSamples = calibrationSamples.toList()
                calibrationSamples.clear()
                calibrationStage = MotionCalibrationStage.LOWERED_READY
                feedback = "Now hold the deliberate lowered posture you intend to use"
            }
            MotionCalibrationStage.MEASURE_LOWERED -> {
                val threshold = raisedPostureSamples?.let {
                    MotionCalibrationMath.loweringThresholdDegrees(it, calibrationSamples)
                }
                if (threshold == null) {
                    motionPreferences.disable()
                    motionThresholdDegrees = null
                    provisionalMotionStopPolicy = ProvisionalMotionStopPolicy()
                    feedback = "Postures were not distinct and steady; motion stop stays off"
                    storeDiagnostic("motion_calibration_rejected;manual_stop_only")
                } else {
                    motionPreferences.enable(threshold)
                    motionThresholdDegrees = threshold
                    provisionalMotionStopPolicy = ProvisionalMotionStopPolicy(threshold)
                    feedback = "Personalized motion stop enabled · physical verification still required"
                    storeDiagnostic("motion_calibration_saved;threshold_degrees=${threshold.toInt()};physical_verification=pending")
                }
                raisedPostureSamples = null
                calibrationSamples.clear()
                calibrationStage = MotionCalibrationStage.NONE
            }
            else -> Unit
        }
        updateMotionDiagnostics()
    }

    private enum class MotionCalibrationStage(val prompt: String?, val isSampling: Boolean) {
        NONE(null, false),
        RAISED_READY("Hold the raised dictation posture still, then measure it", false),
        MEASURE_RAISED("Measuring raised posture…", true),
        LOWERED_READY("Hold the deliberate lowered posture still, then measure it", false),
        MEASURE_LOWERED("Measuring lowered posture…", true);

        val canMeasure: Boolean get() = this == RAISED_READY || this == LOWERED_READY
    }

    private companion object { const val CALIBRATION_SAMPLE_COUNT = 30 }
}

@Composable
private fun CaptureScreen(
    snapshot: CaptureSnapshot,
    feedback: String,
    onStart: () -> Unit,
    onStop: () -> Unit,
    onClose: () -> Unit,
    motionStopEnabled: Boolean,
    calibrationPrompt: String?,
    calibrationCanMeasure: Boolean,
    calibrationSampleCount: Int,
    onMotionAction: () -> Unit,
    onRetryRecording: (dev.gitblame.voxgolem.capture.CaptureRecord) -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().padding(horizontal = 12.dp, vertical = 10.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text("VoxGolem", style = MaterialTheme.typography.title3, textAlign = TextAlign.Center)
        Text(
            text = when (snapshot.active?.captureState) {
                CaptureState.RECORDING -> "Recording · transcription pending"
                CaptureState.STOPPING -> "Saving recording…"
                null -> feedback.ifBlank { "Ready for voice capture" }
                else -> feedback
            },
            modifier = Modifier.padding(vertical = 8.dp), textAlign = TextAlign.Center,
        )
        if (snapshot.active != null) {
            LazyColumn(Modifier.weight(1f).fillMaxWidth()) {
                item { Text(snapshot.active.previewText ?: "Speak after microphone-ready feedback", textAlign = TextAlign.Center, modifier = Modifier.padding(vertical = 4.dp)) }
            }
            Button(onClick = onStop) {
                Text("Stop")
            }
            Text(
                if (motionStopEnabled) "Personalized motion stop · verify on this watch"
                else "Manual Stop · physical stop calibration pending",
                textAlign = TextAlign.Center,
                style = MaterialTheme.typography.body2,
            )
            return@Column
        } else {
            Button(onClick = onStart) {
                Text("Capture")
            }
        }
        if (calibrationPrompt != null) {
            Text(calibrationPrompt, modifier = Modifier.padding(vertical = 4.dp), textAlign = TextAlign.Center)
            Text("$calibrationSampleCount / 30 samples", style = MaterialTheme.typography.body2)
            if (calibrationCanMeasure) CompactButton(onClick = onMotionAction) { Text("Measure posture") }
        } else if (snapshot.active == null) {
            CompactButton(onClick = onMotionAction) {
                Text(if (motionStopEnabled) "Disable motion stop" else "Calibrate arm-down stop")
            }
        }
        if (feedback.startsWith("Saved")) CompactButton(onClick = onClose) { Text("Done") }
        Text("Recent captures", modifier = Modifier.padding(top = 8.dp))
        LazyColumn(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally) {
            items(snapshot.recent.take(5), key = { it.captureId }) { item ->
                Text(
                    text = "${item.finishedLocalDate ?: item.startedAtUtc?.take(10) ?: item.createdAtUtc.take(10)} · ${item.captureState.name.lowercase()} · ${item.transferState.name.lowercase()}",
                    modifier = Modifier.padding(vertical = 3.dp), textAlign = TextAlign.Center,
                    style = MaterialTheme.typography.body2,
                )
                item.finalText?.let { transcript ->
                    Text(transcript, modifier = Modifier.padding(bottom = 4.dp), textAlign = TextAlign.Center, style = MaterialTheme.typography.body2)
                }
                if (item.captureState == CaptureState.INTERRUPTED && item.finalizedByteLength == null) {
                    CompactButton(onClick = { onRetryRecording(item) }) { Text("Retry recording repair") }
                }
            }
        }
    }
}
