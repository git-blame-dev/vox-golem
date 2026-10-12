package dev.gitblame.voxgolem.android.wear.capture

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.os.VibrationEffect
import android.os.Vibrator
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleOwner
import androidx.lifecycle.LifecycleRegistry
import androidx.lifecycle.lifecycleScope
import dev.gitblame.voxgolem.android.wear.transport.CaptureTransferJob
import dev.gitblame.voxgolem.android.wear.captureHasQuietRecognitionWarning
import dev.gitblame.voxgolem.capture.CaptureStartupTiming
import dev.gitblame.voxgolem.capture.CaptureTiming
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Job
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class CaptureService : Service(), LifecycleOwner {
    private val lifecycleRegistry = LifecycleRegistry(this)
    override val lifecycle: Lifecycle get() = lifecycleRegistry
    private lateinit var repository: CaptureRepository
    internal var preparationDispatcher: CoroutineDispatcher = Dispatchers.IO
    private var preparationJob: Job? = null
    private var attachRepairJob: Job? = null
    private var admissionRecovered = false
    internal lateinit var session: RecordingSession
        private set
    internal var wakeLock: PowerManager.WakeLock? = null
        private set

    override fun onCreate() {
        super.onCreate()
        lifecycleRegistry.currentState = Lifecycle.State.CREATED
        val timing = CaptureStartupTiming.current()
        timing?.mark("service-entry")
        repository = CaptureRepository(this)
        session = RecordingSession(this, repository)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            null -> stopPreparation("service-restarted")
            ACTION_STOP -> stopPreparation(intent.getStringExtra(EXTRA_STOP_REASON) ?: "manual_stop")
            ACTION_ATTACH -> session.active?.let { CaptureSignals.attached(this, it.captureId, session.ready) } ?: run {
                if (!session.retired && !session.preparing && session.prepareRecovery()) recoverBeforeAdmission()
            }
            ACTION_START -> if (session.active == null && !session.retired && !session.preparing) {
                if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                    stopPreparation("permission-denied")
                } else if (session.prepareCapture()) recoverBeforeAdmission()
            }
        }
        return START_NOT_STICKY
    }

    private fun recoverBeforeAdmission() {
        if (admissionRecovered) { completePreparation(); return }
        if (preparationJob?.isActive == true) return
        val owner = session.recoveryOwner()
        preparationJob = launchOwnedOperation {
            try {
                withContext(preparationDispatcher) {
                    val timing = CaptureStartupTiming.current()
                    timing?.mark("admission-recovery-start")
                    repository.recoverForAdmission(owner)
                    check(repository.active() == null) { "Capture recovery left an active record" }
                    timing?.mark("admission-recovery-finished")
                }
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                session.failPreparation("Capture recovery needs attention · audio retained on watch")
                return@launchOwnedOperation
            }
            admissionRecovered = true
            completePreparation()
        }
    }

    private fun completePreparation() {
        if (session.retired) return
        if (session.preparing) {
            if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                session.failPreparation("Microphone permission revoked · recording not started")
            } else session.beginCapture()
        } else if (session.active == null && attachRepairJob?.isActive != true) {
            attachRepairJob = launchOwnedOperation {
                try {
                    withContext(preparationDispatcher) { repository.repairInterruptedAudio { coroutineContext.ensureActive() } }
                    if (session.active == null && !session.preparing && !session.retired) {
                        CaptureSignals.recovered(this@CaptureService)
                        session.stopCapture("recovery-completed")
                    }
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (_: Exception) {
                    if (session.active == null && !session.preparing) session.failPreparation("Capture recovery needs attention · audio retained on watch")
                }
            }
        }
    }

    private fun launchOwnedOperation(
        dispatcher: CoroutineDispatcher = Dispatchers.Main.immediate,
        block: suspend CoroutineScope.() -> Unit,
    ): Job? {
        val hold = session.retainCaptureLifetime() ?: return null
        return try {
            lifecycleScope.launch(dispatcher, start = CoroutineStart.LAZY, block = block).also { job ->
                job.invokeOnCompletion { hold.close() }
                job.start()
            }
        } catch (failure: Throwable) {
            hold.close()
            throw failure
        }
    }

    private fun stopPreparation(reason: String) {
        preparationJob?.cancel()
        attachRepairJob?.cancel()
        session.stopCapture(reason)
    }

    internal fun establishForeground() = startForeground(NOTIFICATION_ID, notification("Preparing microphone"))
    internal fun acquireWakeLock() {
        wakeLock = (getSystemService(POWER_SERVICE) as PowerManager)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "VoxGolem:Capture").apply { acquire(MAX_CAPTURE_MS + 60_000) }
    }
    internal fun retireForeground() {
        stopForeground(STOP_FOREGROUND_REMOVE)
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
    }
    internal fun scheduleRetry(timing: CaptureTiming?) {
        launchOwnedOperation(Dispatchers.IO) {
            timing?.mark("audio-repair-scan-start")
            try {
                repository.repairInterruptedAudio { coroutineContext.ensureActive() }
                timing?.mark("audio-repair-scan-finished")
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (_: Exception) {
                timing?.mark("audio-repair-scan-failed")
            }
            runCatching { CaptureTransferJob.schedule(applicationContext, immediate = true) }
                .onSuccess { timing?.mark("retry-scheduled") }
        }
    }
    internal fun updateNotification(message: String) {
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification(message))
    }
    private fun notification(message: String): Notification {
        val manager = getSystemService(NotificationManager::class.java)
        if (Build.VERSION.SDK_INT >= 26) manager.createNotificationChannel(
            NotificationChannel(CHANNEL_ID, "Voice capture", NotificationManager.IMPORTANCE_LOW))
        return if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_btn_speak_now).setContentTitle("VoxGolem Capture")
            .setContentText(message).setOngoing(true).build()
        else @Suppress("DEPRECATION") Notification.Builder(this).setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentTitle("VoxGolem Capture").setContentText(message).setOngoing(true).build()
    }
    override fun onDestroy() {
        preparationJob?.cancel()
        attachRepairJob?.cancel()
        session.onDestroy()
        lifecycleRegistry.currentState = Lifecycle.State.DESTROYED
        super.onDestroy()
    }
    override fun onBind(intent: Intent?): IBinder? = null
    companion object {
        const val ACTION_START = "dev.gitblame.voxgolem.capture.START"
        const val ACTION_STOP = "dev.gitblame.voxgolem.capture.STOP"
        const val ACTION_ATTACH = "dev.gitblame.voxgolem.capture.ATTACH"
        const val EXTRA_STOP_REASON = "stopReason"
        const val MAX_CAPTURE_MS = 10 * 60 * 1000L
        private const val NOTIFICATION_ID = 4101
        private const val CHANNEL_ID = "capture"
    }
}

object CaptureSignals {
    private const val ACTION = "dev.gitblame.voxgolem.capture.STATE"
    fun ready(context: Context, captureId: String? = null) { vibrate(context, longArrayOf(0, 80)); signal(context, "ready", captureId = captureId) }
    fun attached(context: Context, captureId: String, microphoneReady: Boolean) {
        context.sendBroadcast(Intent(ACTION).setPackage(context.packageName).putExtra("state", "attached")
            .putExtra("captureId", captureId).putExtra("microphoneReady", microphoneReady))
    }
    fun saved(context: Context, captureId: String) { signal(context, "saved", captureId = captureId) }
    fun completed(context: Context, captureId: String) { vibrate(context, longArrayOf(0, 2_000)); signal(context, "completed", captureId = captureId) }
    fun attention(context: Context, message: String, captureId: String? = null) {
        if (!captureHasQuietRecognitionWarning(message)) vibrate(context, longArrayOf(0, 100, 100, 100, 100, 100))
        signal(context, "failed", message, captureId)
    }
    fun failed(context: Context, message: String, captureId: String? = null) = attention(context, message, captureId)
    fun recovered(context: Context) = signal(context, "recovered", "Previous capture recovered; microphone was not restarted")
    private fun vibrate(context: Context, pattern: LongArray) {
        runCatching { context.getSystemService(Vibrator::class.java).vibrate(VibrationEffect.createWaveform(pattern, -1)) }
    }
    private fun signal(context: Context, value: String, message: String? = null, captureId: String? = null) {
        context.sendBroadcast(Intent(ACTION).setPackage(context.packageName).putExtra("state", value).putExtra("message", message).putExtra("captureId", captureId))
    }
    const val SIGNAL_ACTION = ACTION
}
