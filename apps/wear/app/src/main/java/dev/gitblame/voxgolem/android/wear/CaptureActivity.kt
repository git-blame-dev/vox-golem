package dev.gitblame.voxgolem.android.wear

import android.Manifest
import android.app.ActivityOptions
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.Modifier
import androidx.core.content.ContextCompat
import androidx.core.view.doOnPreDraw
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import androidx.wear.compose.material3.MaterialTheme
import dev.gitblame.voxgolem.android.wear.capture.CaptureRepository
import dev.gitblame.voxgolem.android.wear.capture.CaptureService
import dev.gitblame.voxgolem.android.wear.capture.CaptureSignals
import dev.gitblame.voxgolem.android.wear.stop.MotionCaptureController
import dev.gitblame.voxgolem.android.wear.stop.MotionCalibrationStage
import dev.gitblame.voxgolem.android.wear.transport.WearCaptureTransport
import dev.gitblame.voxgolem.capture.CaptureTiming
import dev.gitblame.voxgolem.capture.CaptureStartupTiming
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.Job
import kotlinx.coroutines.CoroutineScope

class CaptureActivity : ComponentActivity() {
    private val repository by lazy { CaptureRepository(this) }
    internal val presentation: CapturePresentation by lazy { CapturePresentation(lifecycleScope, repository, ::presentationChanged, ::returnToWatchFace) }
    internal val motion: MotionCaptureController by lazy {
        MotionCaptureController(this, { presentation.feedback = it }, {
            presentation.calibrationChanged(it)
            updateMotionDiagnostics()
        }, ::stopCapture)
    }
    private var startupTiming: CaptureTiming? = null
    private var captureCommandPending = false
    private var launchGeneration = 0L
    private var captureStartIssued = false
    private var uiInterrupted = false
    internal val preparingCapture: Boolean get() = captureStartIssued && !uiInterrupted &&
        presentation.snapshot.active == null && presentation.preparationReceipt?.launchGeneration == launchGeneration
    private var permissionLaunchGeneration: Long? = null
    private val commandJobs = mutableListOf<Job>()
    internal val commandsPending: Boolean get() = commandJobs.any { it.isActive }
    private fun launchCommand(block: suspend CoroutineScope.() -> Unit) {
        commandJobs.removeAll { it.isCompleted }
        commandJobs += lifecycleScope.launch(block = block)
    }
    private val permission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        val ticket = permissionLaunchGeneration
        permissionLaunchGeneration = null
        if (ticket != launchGeneration || uiInterrupted || isFinishing || isDestroyed) return@registerForActivityResult
        if (granted) requestStart() else presentation.feedback = "Microphone permission is required"
    }
    private val signals = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            presentation.signal(intent?.getStringExtra("state"), intent?.getStringExtra("captureId"),
                intent?.getBooleanExtra("microphoneReady", false) == true, intent?.getStringExtra("message"))
        }
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        presentation.restore(savedInstanceState)
        captureStartIssued = savedInstanceState?.getBoolean("captureStartIssued") ?: false
        launchGeneration = savedInstanceState?.getLong("launchGeneration") ?: 0L
        if (savedInstanceState?.containsKey("permissionLaunchGeneration") == true)
            permissionLaunchGeneration = savedInstanceState.getLong("permissionLaunchGeneration")
        onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
            override fun handleOnBackPressed() {
                if (presentation.snapshot.active != null || captureStartIssued) stopCapture()
                else if (captureCommandPending || presentation.pendingCaptureStart) {
                    cancelPendingCaptureStart()
                    finish()
                }
                else {
                    cancelPendingCaptureStart()
                    launchCommand {
                        if (withContext(Dispatchers.IO) { repository.active() != null }) stopCapture() else finish()
                    }
                }
            }
        })
        if (savedInstanceState == null && intent?.action == Intent.ACTION_MAIN) beginStartupTiming()
        ContextCompat.registerReceiver(this, signals, IntentFilter(CaptureSignals.SIGNAL_ACTION), ContextCompat.RECEIVER_NOT_EXPORTED)
        presentation.refresh(savedInstanceState != null || intent?.action != Intent.ACTION_MAIN)
        setContent {
            WearAppTheme {
                Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) {
                    CaptureSavedExit(presentation.savedExitDeadlineMs) {
                        CaptureBackground(listening = presentation.backgroundListening, saved = presentation.savedFeedbackCaptureId != null)
                        if (presentation.savedFeedbackCaptureId != null) CaptureInputScreen(presentation.savedFeedbackText, false, { stopCapture() })
                        else CaptureScreen(
                            snapshot = presentation.snapshot, feedback = presentation.feedback,
                            terminalSaveFailure = presentation.terminalSaveFailure,
                            motionStopEnabled = motion.thresholdDegrees != null,
                            calibrationPrompt = motion.stage.prompt, calibrationCanMeasure = motion.stage.canMeasure,
                            calibrationSampleCount = motion.sampleCount,
                            onStart = ::requestStart, onStop = { stopCapture() }, stopEnabled = presentation.stopEnabled,
                            onClose = ::finish, onMotionAction = ::onMotionAction, onManualCapture = ::recordWithManualStop,
                            onRetryRecording = ::retryRecording,
                            preparing = preparingCapture,
                            observationError = presentation.observationError,
                        )
                    }
                }
            }
        }
        lifecycleScope.launch {
            repeatOnLifecycle(Lifecycle.State.RESUMED) {
                while (true) { presentation.refresh(false); delay(500) }
            }
        }
        if (savedInstanceState == null && intent?.action == Intent.ACTION_MAIN) handleCaptureLaunch()
        else if (intent?.action == StandaloneCapabilityProbe.ACTION) StandaloneCapabilityProbe(this).run()
        else launchCommand {
            if (withContext(Dispatchers.IO) { repository.active() != null }) {
                if (uiInterrupted || isFinishing) stopCapture() else attachToCaptureService()
            }
        }
    }
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (intent.action == StandaloneCapabilityProbe.ACTION) { StandaloneCapabilityProbe(this).run(); return }
        if (intent.action == Intent.ACTION_MAIN) handleCaptureLaunch()
        presentation.refresh(false)
    }
    internal fun requestStart() {
        if (isFinishing || isDestroyed || motion.stage != MotionCalibrationStage.NONE || captureCommandPending) return
        uiInterrupted = false
        if (!presentation.resumed) { presentation.pendingCaptureStart = true; return }
        presentation.pendingCaptureStart = false
        captureCommandPending = true
        val ticket = launchGeneration
        launchCommand {
            try {
                val active = withContext(Dispatchers.IO) { repository.active() }
                if (ticket != launchGeneration || uiInterrupted || isFinishing || isDestroyed) return@launchCommand
                if (active != null) { attachToCaptureService(); presentation.refresh(); return@launchCommand }
                if (!presentation.resumed) { presentation.pendingCaptureStart = true; return@launchCommand }
                if (ContextCompat.checkSelfPermission(this@CaptureActivity, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) startCaptureOnce()
                else {
                    permissionLaunchGeneration = ticket
                    permission.launch(Manifest.permission.RECORD_AUDIO)
                }
            } finally { captureCommandPending = false }
        }
    }
    private fun handleCaptureLaunch() {
        if (isFinishing || isDestroyed) return
        uiInterrupted = false
        val ticket = ++launchGeneration
        launchCommand {
            val active = withContext(Dispatchers.IO) { repository.active() }
            if (ticket != launchGeneration || uiInterrupted || isFinishing || isDestroyed) return@launchCommand
            if (active == null && (startupTiming == null || startupTiming?.isFinished == true)) beginStartupTiming()
            if (active == null && motion.needsCalibration) {
                startupTiming?.finish("arm-down-setup-required")
                onMotionAction()
            } else requestStart()
        }
    }
    internal fun recordWithManualStop() { motion.resetCalibration(); requestStart() }
    private fun startCaptureOnce() {
        if (startupTiming == null || startupTiming?.isFinished == true) beginStartupTiming()
        startupTiming?.mark("start-command")
        presentation.resetCapture()
        val start = Intent(this, CaptureService::class.java).setAction(CaptureService.ACTION_START)
        try {
            captureStartIssued = true
            presentation.beginPreparation(launchGeneration)
            if (Build.VERSION.SDK_INT >= 26) startForegroundService(start) else startService(start)
        } catch (failure: RuntimeException) {
            presentation.retirePreparation()
            if (Build.VERSION.SDK_INT >= 31 && failure is android.app.ForegroundServiceStartNotAllowedException) {
                captureStartIssued = false
                startupTiming?.finish("foreground-start-rejected")
                presentation.feedback = "Tap Capture to start recording"
                return
            }
            throw failure
        }
        presentation.refresh(false)
    }
    private fun attachToCaptureService() { startService(Intent(this, CaptureService::class.java).setAction(CaptureService.ACTION_ATTACH)) }
    internal fun stopCapture(reason: String = "manual_stop") {
        cancelPendingCaptureStart()
        presentation.requestStop()
        startService(Intent(this, CaptureService::class.java).setAction(CaptureService.ACTION_STOP).putExtra(CaptureService.EXTRA_STOP_REASON, reason))
    }
    private fun cancelPendingCaptureStart() {
        launchGeneration++
        uiInterrupted = true
        permissionLaunchGeneration = null
        presentation.pendingCaptureStart = false
        presentation.retirePreparation()
    }
    private fun presentationChanged() {
        if (isFinishing || isDestroyed) return
        val active = presentation.snapshot.active
        if (startupTiming == null && active != null) {
            startupTiming = CaptureStartupTiming.forCapture(active.captureId)
            startupTiming?.mark("activity-recreated")
            observeNextDraw(startupTiming, "ui-recreated-pre-draw")
        }
        startupTiming?.mark("initial-snapshot")
        if (!active?.previewText.isNullOrBlank()) startupTiming?.mark("preview-observed")
        if (active != null && !presentation.terminalSaveFailure || presentation.savedFeedbackCaptureId != null || motion.stage != MotionCalibrationStage.NONE)
            window.addFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        else window.clearFlags(android.view.WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        updateMotionDiagnostics()
    }
    private fun updateMotionDiagnostics() { motion.update(presentation.resumed, presentation.snapshot.active?.captureId, presentation.microphoneReadyCaptureId) }
    internal fun onMotionAction() { motion.action(); presentation.refresh() }
    private fun beginStartupTiming() {
        captureStartIssued = false
        presentation.resetCapture()
        startupTiming = CaptureStartupTiming.begin(this)
        observeNextDraw(startupTiming, "ui-pre-draw")
    }
    private fun observeNextDraw(timing: CaptureTiming?, stage: String) {
        if (timing == null) return
        window.decorView.doOnPreDraw { timing.mark(stage) }
        window.decorView.invalidate()
    }
    @Suppress("DEPRECATION")
    private fun returnToWatchFace() {
        if (!presentation.resumed || isFinishing) return
        if (Build.VERSION.SDK_INT >= 34) overrideActivityTransition(OVERRIDE_TRANSITION_CLOSE, 0, 0)
        runCatching {
            startActivity(Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_HOME)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_ANIMATION), ActivityOptions.makeCustomAnimation(this, 0, 0).toBundle())
        }
        if (isTaskRoot) finishAndRemoveTask() else finish()
        if (Build.VERSION.SDK_INT < 34) overridePendingTransition(0, 0)
    }
    override fun onSaveInstanceState(outState: Bundle) {
        presentation.save(outState)
        outState.putBoolean("captureStartIssued", captureStartIssued)
        outState.putLong("launchGeneration", launchGeneration)
        permissionLaunchGeneration?.let { outState.putLong("permissionLaunchGeneration", it) }
        super.onSaveInstanceState(outState)
    }
    private fun retryRecording(capture: dev.gitblame.voxgolem.capture.CaptureRecord) {
        presentation.feedback = "Repairing interrupted recording…"
        launchCommand {
            val repaired = withContext(Dispatchers.IO) {
                runCatching { repository.retryInterruptedFinalization(capture.captureId) }.getOrDefault(false).also { repaired ->
                    if (repaired) repository.get(capture.captureId)?.let { WearCaptureTransport(applicationContext).publishPending(it) }
                }
            }
            if (!presentation.resumed) return@launchCommand
            presentation.feedback = if (repaired) "Recovered audio · finalize it on the phone" else "Recording repair needs attention · original audio retained"
            presentation.refresh()
        }
    }
    override fun onResume() {
        super.onResume()
        presentation.resume(); motion.resume()
        val ticket = launchGeneration
        if (presentation.pendingCaptureStart) window.decorView.post {
            if (ticket == launchGeneration && !uiInterrupted && !isFinishing && presentation.resumed && presentation.pendingCaptureStart) requestStart()
        }
        else launchCommand {
            withContext(Dispatchers.IO) {
                if (repository.active() == null) runCatching { WearCaptureTransport(applicationContext).retryPending() }
            }
        }
    }
    override fun onPause() { presentation.pause(); motion.pause(); super.onPause() }
    override fun onStop() {
        if (!isChangingConfigurations) {
            if (presentation.snapshot.active != null || captureStartIssued) stopCapture()
            else cancelPendingCaptureStart()
        }
        super.onStop()
    }
    override fun onDestroy() { presentation.close(); unregisterReceiver(signals); super.onDestroy() }
}
