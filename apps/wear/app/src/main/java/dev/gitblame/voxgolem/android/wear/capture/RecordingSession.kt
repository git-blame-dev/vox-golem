package dev.gitblame.voxgolem.android.wear.capture

import android.Manifest
import android.content.pm.PackageManager
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Process
import android.os.SystemClock
import androidx.core.content.ContextCompat
import dev.gitblame.voxgolem.android.wear.transport.WearCaptureTransport
import dev.gitblame.voxgolem.android.wear.transport.LiveCaptureHandle
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.CaptureTiming
import dev.gitblame.voxgolem.capture.CaptureStartupTiming
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import java.time.Instant
import java.time.ZoneId
import java.util.concurrent.atomic.AtomicBoolean

internal class RecordingSession(private val service: CaptureService, private val repository: CaptureRepository) {
    private var liveTransport: LiveCaptureHandle? = null
    @Volatile private var recorder: AudioRecord? = null
    private var microphoneLease: RecognitionResourceLease.Token? = null
    private var captureLifetime: CaptureLifetime.Owner? = null
    private var writer: PcmWavWriter? = null
    @Volatile private var record: CaptureRecord? = null
    private val stopping = AtomicBoolean(false)
    private val finalizing = AtomicBoolean(false)
    @Volatile private var pendingStopReason = "manual_stop"
    @Volatile private var captureThread: Thread? = null
    private var startedElapsed = 0L
    private var warnedLimit = false
    @Volatile private var microphoneReady = false
    private var timing: CaptureTiming? = CaptureStartupTiming.current()
    var preparing = false
        private set

    val active: CaptureRecord? get() = record
    val ready: Boolean get() = microphoneReady
    val retired: Boolean get() = stopping.get() || finalizing.get()

    fun prepareCapture(): Boolean {
        if (record != null || retired || preparing) return false
        try { service.establishForeground() }
        catch (_: RuntimeException) {
            fail("Microphone foreground start was rejected · recording not started")
            return false
        }
        timing?.mark("foreground-established")
        if (!prepareRecovery()) return false
        RecognitionResourceLease.process.retryFailedRetirement()
        microphoneLease = RecognitionResourceLease.process.tryAcquire(RecognitionResourceLease.Kind.WATCH_NOTE_AUDIO)
        if (microphoneLease == null) {
            fail("Microphone is in use by assistant · stop assistant listening first")
            return false
        }
        preparing = true
        return true
    }

    fun prepareRecovery(): Boolean {
        if (retired || record != null) return false
        if (captureLifetime != null) return true
        captureLifetime = CaptureLifetime.tryAcquire()
        if (captureLifetime == null) {
            fail("Previous capture is still saving or recovering · try again after it finishes")
            return false
        }
        return true
    }

    fun retainCaptureLifetime(): AutoCloseable? = synchronized(stopping) {
        if (retired) null else captureLifetime?.retain()
    }

    fun recoveryOwner(): CaptureLifetime.Owner = checkNotNull(captureLifetime)

    fun bindCaptureLifetime(captureId: String) { checkNotNull(captureLifetime).bindCapture(captureId) }

    private fun releaseCaptureLifetime() = synchronized(stopping) {
        captureLifetime?.close()
        captureLifetime = null
    }

    fun beginCapture() {
        if (record != null || retired || !preparing) return
        preparing = false
        val sampleRate = listOf(16_000, 24_000, 48_000).firstOrNull { rate ->
            AudioRecord.getMinBufferSize(rate, CHANNEL, ENCODING) > 0
        } ?: run { fail("No supported PCM input rate"); return }
        val sessionId = java.util.UUID.randomUUID().toString()
        val created = runCatching { repository.create(sampleRate, sessionId) }
            .getOrElse { failure ->
                fail(if (failure is IllegalArgumentException) "Speech language unavailable · recording not started" else "Storage unavailable")
                return
            }
        bindCaptureLifetime(created.captureId)
        val requiredBytes = sampleRate.toLong() * 2 * MAX_CAPTURE_MS / 1_000 + STORAGE_OVERHEAD_BYTES
        CaptureStartupTiming.bind(created.captureId)
        if (service.filesDir.usableSpace < requiredBytes) {
            failStart(created, "storage", "insufficient-space", "Insufficient free storage"); return
        }
        val diskWriter = runCatching { PcmWavWriter(repository.pcm(created.captureId), repository.audio(created.captureId), sampleRate) }
            .getOrElse {
                failStart(created, "storage", "spool-create-failed", "Storage unavailable"); return
            }
        writer = diskWriter
        runCatching {
            liveTransport = WearCaptureTransport(service).beginLive(created)
        }.onFailure {
            runCatching { liveTransport?.abort() }
            liveTransport = null
        }
        val minBuffer = AudioRecord.getMinBufferSize(sampleRate, CHANNEL, ENCODING)
        if (ContextCompat.checkSelfPermission(service, Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            failStart(created, "microphone", "permission-revoked", "Microphone permission revoked · recording not started"); return
        }
        val audioRecord = try {
            AudioRecord.Builder()
                .setAudioSource(MediaRecorder.AudioSource.MIC)
                .setAudioFormat(android.media.AudioFormat.Builder().setSampleRate(sampleRate)
                    .setEncoding(ENCODING).setChannelMask(CHANNEL).build())
                .setBufferSizeInBytes(maxOf(minBuffer * 2, sampleRate / 5 * 2))
                .build()
        } catch (_: SecurityException) {
            failStart(created, "microphone", "permission-revoked", "Microphone permission revoked · recording not started"); return
        } catch (_: Throwable) {
            failStart(created, "microphone", "unavailable", "Microphone unavailable"); return
        }
        recorder = audioRecord
        if (audioRecord.state != AudioRecord.STATE_INITIALIZED) {
            failStart(created, "microphone", "initialization-failed", "Microphone initialization failed"); return
        }
        try {
            audioRecord.startRecording()
            timing?.mark("mic-start-returned")
        } catch (_: Exception) {
            failStart(created, "microphone", "start-failed", "Microphone start failed"); return
        }
        val recording = created.copy(captureState = CaptureState.RECORDING)
        try {
            repository.save(recording)
            record = recording
            service.acquireWakeLock()
            startedElapsed = SystemClock.elapsedRealtime()
            updateNotification("Starting microphone")
            captureThread = Thread({ recordLoop(recording) }, "watch-capture-${created.captureId}")
            captureThread!!.start()
        } catch (_: Exception) {
            failStart(created, "capture-start", "setup-failed", "Capture start failed · audio retained on watch")
        }
    }

    private fun recordLoop(initial: CaptureRecord) {
        runCatching { Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO) }
        val bytes = ByteArray(3_200)
        val firstReadBytes = (initial.format.sampleRate / 50 * 2).coerceIn(2, bytes.size)
        var lastCheckpoint = SystemClock.elapsedRealtime()
        val transport = liveTransport
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
                val read = recorder?.read(bytes, 0, if (microphoneReady) bytes.size else firstReadBytes, AudioRecord.READ_BLOCKING) ?: break
                if (read > 0) {
                    val aligned = read - (read % 2)
                    if (aligned == 0) continue
                    if (!microphoneReady) timing?.mark("first-audio-buffer")
                    val diskWriter = writer ?: error("Capture writer was unavailable")
                    diskWriter.append(bytes, aligned)
                    transport?.offer(bytes.copyOf(aligned))
                    synchronized(stopping) {
                        if (!microphoneReady && !stopping.get()) {
                            microphoneReady = true
                            updateNotification("Recording · Stop when finished")
                            CaptureSignals.ready(service, initial.captureId)
                            timing?.mark("ready-cue-issued")
                            service.scheduleRetry(timing)
                        }
                    }
                    val current = record ?: initial
                    if (!stopping.get() && SystemClock.elapsedRealtime() - lastCheckpoint >= CHECKPOINT_MS) {
                        diskWriter.checkpoint()
                        val frames = diskWriter.frameCount
                        synchronized(stopping) {
                            if (!stopping.get()) repository.save((repository.get(current.captureId) ?: current).copy(
                                observedFrames = frames,
                                durableFrames = frames,
                            ))
                        }
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
        }
    }

    fun stopCapture(reason: String) {
        synchronized(stopping) {
            if (stopping.get()) return
            timing?.mark("stop-requested")
            pendingStopReason = reason
            stopping.set(true)
            preparing = false
            val current = record
            if (current == null) {
                releaseMicrophone()
                service.retireForeground()
                timing?.finish("capture-start-cancelled")
                stopSelf()
                releaseCaptureLifetime()
                return
            }
            runCatching { updateNotification("Saving recording") }
            runCatching { recorder?.stop() }
            runCatching {
                val active = repository.get(current.captureId) ?: current
                repository.save(active.copy(captureState = CaptureState.STOPPING, stopReason = reason))
            }
            if (captureThread == null) finalizeCapture(reason)
        }
    }

    private fun finalizeCapture(reason: String) {
        synchronized(stopping) {
            if (!finalizing.compareAndSet(false, true)) return
        }
        releaseMicrophone()
        runCatching { liveTransport?.finish() }
        liveTransport = null
        val initial = record
        val active = initial?.let { runCatching { repository.get(it.captureId) }.getOrNull() ?: it }
        if (active == null) {
            runCatching { writer?.close() }
            writer = null
            service.retireForeground()
            stopSelf()
            releaseCaptureLifetime()
            return
        }
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
        }.getOrElse {
            runCatching { writer?.close() }
            writer = null
            val now = finishDate(Instant.now(), ZoneId.systemDefault())
            active.copy(
                captureState = CaptureState.INTERRUPTED,
                finishedAtUtc = now.instant.toString(), finishedLocalDate = now.localDate, timezoneId = now.timezone,
                observedFrames = maxOf(active.observedFrames, active.durableFrames),
                stopReason = reason,
                localAudioPath = repository.pcm(active.captureId).absolutePath,
                lastErrorStage = "capture-finalize", lastErrorCode = "wav-finalize-failed",
            )
        }
        try {
            repository.save(successful)
            timing?.finish(if (successful.captureState == CaptureState.SAVED) "audio-saved" else "capture-interrupted")
            if (successful.localAudioPath == repository.audio(successful.captureId).absolutePath) repository.deletePcm(successful.captureId)
            if (successful.captureState == CaptureState.SAVED) CaptureSignals.saved(service, successful.captureId)
            else CaptureSignals.failed(service, successful.lastErrorCode?.let { dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(it) }
                ?: "Capture interrupted; audio retained on watch", successful.captureId)
            if (successful.finalizedByteLength != null && repository.audio(successful.captureId).isFile) {
                WearCaptureTransport(service).publishPending(successful)
            }
        } catch (_: Exception) {
            timing?.finish("metadata-save-failed")
            CaptureSignals.failed(service, "Recording retained · could not save capture status", active.captureId)
        } finally {
            service.retireForeground()
            record = null
            stopSelf()
            releaseCaptureLifetime()
        }
    }

    private fun failStart(created: CaptureRecord, stage: String, code: String, message: String) {
        runCatching { repository.save(created.copy(captureState = CaptureState.FAILED, lastErrorStage = stage, lastErrorCode = code)) }
        fail(message)
    }

    private fun fail(message: String) {
        stopping.set(true)
        preparing = false
        runCatching { liveTransport?.abort() }
        liveTransport = null
        releaseMicrophone()
        runCatching { writer?.close() }
        writer = null
        record = null
        captureThread = null
        timing?.finish("capture-failed")
        CaptureSignals.failed(service, message)
        service.retireForeground()
        stopSelf()
        releaseCaptureLifetime()
    }

    private fun updateNotification(message: String) {
        service.updateNotification(message)
    }

    fun failPreparation(message: String) { if (!retired) fail(message) }

    private fun stopSelf() = service.stopSelf()

    private fun releaseMicrophone(): Boolean = synchronized(stopping) {
        val owned = recorder
        val lease = microphoneLease
        runCatching { owned?.stop() }
        if (runCatching { owned?.release() }.isSuccess) {
            recorder = null; microphoneLease = null; lease?.close()
            true
        } else {
            lease?.onRetirementRetry {
                synchronized(stopping) {
                    if (recorder !== owned || microphoneLease !== lease) false else releaseMicrophone()
                }
            }
            CaptureSignals.failed(service, "Microphone cleanup failed · assistant listening remains blocked")
            false
        }
    }

    fun onDestroy() {
        if (!stopping.get()) stopCapture("service_destroyed")
        if (captureThread == null || captureThread?.isAlive == false) releaseMicrophone()
    }

    companion object {
        const val MAX_CAPTURE_MS = CaptureService.MAX_CAPTURE_MS
        private const val CHECKPOINT_MS = 1_000L
        private const val WARNING_BEFORE_LIMIT_MS = 60_000L
        private const val STORAGE_OVERHEAD_BYTES = 4L * 1024 * 1024
        private const val CHANNEL = android.media.AudioFormat.CHANNEL_IN_MONO
        private const val ENCODING = android.media.AudioFormat.ENCODING_PCM_16BIT
    }
}
