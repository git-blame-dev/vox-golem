package dev.gitblame.voxgolem.android.wear.capture

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.os.SystemClock
import android.os.VibrationEffect
import android.os.Vibrator
import androidx.core.content.ContextCompat
import dev.gitblame.voxgolem.android.wear.transport.WearCaptureTransport
import dev.gitblame.voxgolem.android.wear.transport.CaptureTransferJob
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.PcmRecordingWriter
import dev.gitblame.voxgolem.capture.TransferState
import java.time.Instant
import java.time.ZoneId
import java.util.concurrent.atomic.AtomicBoolean

class CaptureService : Service() {
    private lateinit var repository: CaptureRepository
    private var recorder: AudioRecord? = null
    private var writer: PcmWavWriter? = null
    private var record: CaptureRecord? = null
    private var wakeLock: PowerManager.WakeLock? = null
    private val stopping = AtomicBoolean(false)
    private val finalizing = AtomicBoolean(false)
    @Volatile private var pendingStopReason = "manual_stop"
    @Volatile private var captureThread: Thread? = null
    private var startedElapsed = 0L
    private var warnedLimit = false

    override fun onCreate() {
        super.onCreate()
        repository = CaptureRepository(this)
        repository.recoverInterrupted()
        CaptureTransferJob.schedule(this, immediate = true)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> stopCapture(intent.getStringExtra(EXTRA_STOP_REASON) ?: "manual_stop")
            ACTION_ATTACH -> if (record == null) {
                CaptureSignals.recovered(this)
                stopSelf()
            }
            ACTION_START, null -> if (record == null) beginCapture()
        }
        return START_NOT_STICKY
    }

    private fun beginCapture() {
        if (repository.active() != null || ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            stopSelf(); return
        }
        startForeground(NOTIFICATION_ID, notification("Preparing microphone"))
        val sampleRate = listOf(16_000, 24_000, 48_000).firstOrNull { rate ->
            AudioRecord.getMinBufferSize(rate, CHANNEL, ENCODING) > 0
        } ?: run { fail("No supported PCM input rate"); return }
        val sessionId = java.util.UUID.randomUUID().toString()
        val created = runCatching { repository.create(sampleRate, sessionId) }
            .getOrElse { fail("Storage unavailable"); return }
        val requiredBytes = sampleRate.toLong() * 2 * MAX_CAPTURE_MS / 1_000 + STORAGE_OVERHEAD_BYTES
        if (filesDir.usableSpace < requiredBytes) {
            repository.save(created.copy(captureState = CaptureState.FAILED, lastErrorStage = "storage", lastErrorCode = "insufficient-space"))
            fail("Insufficient free storage"); return
        }
        val minBuffer = AudioRecord.getMinBufferSize(sampleRate, CHANNEL, ENCODING)
        val audioRecord = runCatching {
            AudioRecord.Builder()
                .setAudioSource(MediaRecorder.AudioSource.MIC)
                .setAudioFormat(android.media.AudioFormat.Builder().setSampleRate(sampleRate)
                    .setEncoding(ENCODING).setChannelMask(CHANNEL).build())
                .setBufferSizeInBytes(maxOf(minBuffer * 2, sampleRate / 5 * 2))
                .build()
        }.getOrElse {
            repository.save(created.copy(captureState = CaptureState.FAILED, lastErrorStage = "microphone", lastErrorCode = "unavailable"))
            fail("Microphone unavailable"); return
        }
        if (audioRecord.state != AudioRecord.STATE_INITIALIZED) {
            audioRecord.release()
            repository.save(created.copy(captureState = CaptureState.FAILED, lastErrorStage = "microphone", lastErrorCode = "initialization-failed"))
            fail("Microphone initialization failed"); return
        }
        val diskWriter = runCatching { PcmWavWriter(repository.pcm(created.captureId), repository.audio(created.captureId), sampleRate) }
            .getOrElse {
                audioRecord.release()
                repository.save(created.copy(captureState = CaptureState.FAILED, lastErrorStage = "storage", lastErrorCode = "spool-create-failed"))
                fail("Storage unavailable"); return
            }
        try {
            audioRecord.startRecording()
        } catch (_: Exception) {
            audioRecord.release(); diskWriter.close()
            repository.save(created.copy(captureState = CaptureState.FAILED, lastErrorStage = "microphone", lastErrorCode = "start-failed"))
            fail("Microphone start failed"); return
        }
        recorder = audioRecord
        writer = diskWriter
        record = created.copy(captureState = CaptureState.RECORDING)
        repository.save(record!!)
        wakeLock = (getSystemService(POWER_SERVICE) as PowerManager)
            .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "VoxGolem:Capture").also { it.acquire(MAX_CAPTURE_MS + 60_000) }
        startedElapsed = SystemClock.elapsedRealtime()
        getSystemService(NotificationManager::class.java).notify(NOTIFICATION_ID, notification("Starting microphone"))
        captureThread = Thread({ recordLoop(record!!) }, "watch-capture-${created.captureId}").also { it.start() }
    }

    private fun recordLoop(initial: CaptureRecord) {
        val bytes = ByteArray(3_200)
        var lastCheckpoint = SystemClock.elapsedRealtime()
        var microphoneReady = false
        val transport = WearCaptureTransport(this)
        transport.beginLive(initial)
        try {
            while (!stopping.get()) {
                val elapsed = SystemClock.elapsedRealtime() - startedElapsed
                if (!warnedLimit && elapsed >= MAX_CAPTURE_MS - WARNING_BEFORE_LIMIT_MS) {
                    warnedLimit = true
                    updateNotification("1 minute remaining · finish with Stop")
                }
                if (elapsed >= MAX_CAPTURE_MS) {
                    stopCapture("limit_reached"); break
                }
                val read = recorder?.read(bytes, 0, bytes.size, AudioRecord.READ_BLOCKING) ?: break
                if (read > 0) {
                    val aligned = read - (read % 2)
                    if (!microphoneReady && aligned > 0) {
                        microphoneReady = true
                        updateNotification("Recording · Stop when finished")
                        CaptureSignals.ready(this)
                    }
                    val diskWriter = writer ?: error("Capture writer was unavailable")
                    diskWriter.append(bytes, aligned)
                    transport.offerLive(bytes.copyOf(aligned))
                    val current = record ?: initial
                    if (SystemClock.elapsedRealtime() - lastCheckpoint >= CHECKPOINT_MS) {
                        diskWriter.checkpoint()
                        val frames = diskWriter.frameCount
                        repository.save((repository.get(current.captureId) ?: current).copy(
                            observedFrames = frames,
                            durableFrames = frames,
                        ))
                        lastCheckpoint = SystemClock.elapsedRealtime()
                    }
                } else if (read < 0) {
                    stopCapture("audio_read_error_$read"); break
                }
            }
        } catch (_: Exception) {
            stopCapture("capture_io_error")
        } finally {
            finalizeCapture(pendingStopReason)
            transport.closeLive()
        }
    }

    private fun stopCapture(reason: String) {
        if (!stopping.compareAndSet(false, true)) return
        pendingStopReason = reason
        val initial = record
        val active = initial?.let { repository.get(it.captureId) ?: it }
        if (active == null) { stopSelf(); return }
        repository.save(active.copy(captureState = CaptureState.STOPPING, stopReason = reason))
        updateNotification("Saving recording")
        runCatching { recorder?.stop() }
        recorder?.release(); recorder = null
        if (captureThread == null) finalizeCapture(reason)
    }

    private fun finalizeCapture(reason: String) {
        if (!finalizing.compareAndSet(false, true)) return
        val initial = record
        val active = initial?.let { repository.get(it.captureId) ?: it }
        if (active == null) { stopSelf(); return }
        val successful = runCatching {
            val manifest = writer?.finish() ?: error("Capture writer was unavailable")
            writer = null
            val finished = finishDate(Instant.now(), ZoneId.systemDefault())
            active.copy(
                    captureState = completedCaptureState(reason),
                finishedAtUtc = finished.instant.toString(), finishedLocalDate = finished.localDate, timezoneId = finished.timezone,
                observedFrames = manifest.frameCount, durableFrames = manifest.frameCount,
                localAudioPath = repository.audio(active.captureId).absolutePath,
                finalizedByteLength = manifest.wavByteLength, sha256 = manifest.sha256,
                stopReason = reason,
                lastErrorStage = if (reason == "limit_reached") "capture-limit" else active.lastErrorStage,
                lastErrorCode = if (reason == "limit_reached") "limit-reached-review" else active.lastErrorCode,
            )
        }.getOrElse { error ->
            runCatching { writer?.close() }
            writer = null
            val now = finishDate(Instant.now(), ZoneId.systemDefault())
            active.copy(
                captureState = CaptureState.INTERRUPTED,
                finishedAtUtc = now.instant.toString(), finishedLocalDate = now.localDate, timezoneId = now.timezone,
                observedFrames = maxOf(active.observedFrames, active.durableFrames),
                stopReason = reason,
                localAudioPath = repository.pcm(active.captureId).absolutePath,
                lastErrorStage = "capture-finalize", lastErrorCode = error.message ?: "wav-finalize-failed",
            )
        }
        repository.save(successful)
        if (successful.localAudioPath == repository.audio(successful.captureId).absolutePath) repository.deletePcm(successful.captureId)
        if (successful.captureState == CaptureState.SAVED) CaptureSignals.saved(this)
        else CaptureSignals.failed(this, successful.lastErrorCode ?: "Capture interrupted; audio retained on watch")
        if (successful.finalizedByteLength != null && repository.audio(successful.captureId).isFile) {
            WearCaptureTransport(this).publishPending(successful)
        }
        stopForeground(STOP_FOREGROUND_REMOVE)
        wakeLock?.let { if (it.isHeld) it.release() }; wakeLock = null
        record = null
        stopSelf()
    }

    private fun fail(message: String) {
        CaptureSignals.failed(this, message)
        stopForeground(STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun updateNotification(message: String) {
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
        if (record != null && !stopping.get()) stopCapture("service_destroyed")
        super.onDestroy()
    }

    override fun onBind(intent: Intent?): IBinder? = null

    companion object {
        const val ACTION_START = "dev.gitblame.voxgolem.capture.START"
        const val ACTION_STOP = "dev.gitblame.voxgolem.capture.STOP"
        const val ACTION_ATTACH = "dev.gitblame.voxgolem.capture.ATTACH"
        const val EXTRA_STOP_REASON = "stopReason"
        const val MAX_CAPTURE_MS = 10 * 60 * 1000L
        private const val CHECKPOINT_MS = 1_000L
        private const val WARNING_BEFORE_LIMIT_MS = 60_000L
        private const val STORAGE_OVERHEAD_BYTES = 4L * 1024 * 1024
        private const val NOTIFICATION_ID = 4101
        private const val CHANNEL_ID = "capture"
        private const val CHANNEL = android.media.AudioFormat.CHANNEL_IN_MONO
        private const val ENCODING = android.media.AudioFormat.ENCODING_PCM_16BIT
    }
}

object CaptureSignals {
    private const val ACTION = "dev.gitblame.voxgolem.capture.STATE"
    fun ready(context: Context) { vibrate(context, longArrayOf(0, 80)); signal(context, "ready") }
    fun saved(context: Context) { vibrate(context, longArrayOf(0, 55, 70, 140)); signal(context, "saved") }
    fun failed(context: Context, message: String) {
        vibrate(context, longArrayOf(0, 200, 90, 200)); signal(context, "failed", message)
    }
    fun recovered(context: Context) = signal(context, "recovered", "Previous capture recovered; microphone was not restarted")
    private fun vibrate(context: Context, pattern: LongArray) {
        runCatching {
            (context.getSystemService(Context.VIBRATOR_SERVICE) as Vibrator)
                .vibrate(VibrationEffect.createWaveform(pattern, -1))
        }
    }
    private fun signal(context: Context, value: String, message: String? = null) {
        context.sendBroadcast(Intent(ACTION).setPackage(context.packageName).putExtra("state", value).putExtra("message", message))
    }
    const val SIGNAL_ACTION = ACTION
}
