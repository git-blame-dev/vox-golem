package dev.gitblame.voxgolem.capture

import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Process
import android.os.SystemClock
import java.util.concurrent.atomic.AtomicBoolean

/** The caller owns admission/foreground consent. This session opens exactly one microphone. */
class RecordedAudioSession(
    private val store: RecordedAudioStore,
    private val purpose: CapturePurpose,
    private val language: String,
    private val networkFallback: Boolean,
    private val inputFactory: () -> PcmInput = { AndroidPcmInput(store.appContext) },
    private val clock: () -> Long = SystemClock::elapsedRealtime,
    private val onIdentity: (String) -> Unit = {},
) : AutoCloseable {
    interface PcmInput : AutoCloseable {
        fun start()
        fun read(bytes: ByteArray): Int
        fun stop()
    }
    private val stopping = AtomicBoolean(false)
    @Volatile private var worker: Thread? = null
    @Volatile private var input: PcmInput? = null
    @Volatile private var reason = "finished"
    @Volatile private var retirementFailed = false
    private var recording: RecordedAudioStore.Recording? = null

    fun start(onPcm: (ByteArray) -> Boolean, onReady: () -> Unit,
        onStopped: (Result<RecordedAudioArtifact>) -> Unit) {
        check(worker == null && !stopping.get())
        val opened = store.open(purpose, language, networkFallback)
        recording = opened
        try { onIdentity(opened.captureId) }
        catch (failure: Exception) { runCatching { opened.finish("start-failed") }; throw failure }
        worker = Thread({
            runCatching { Process.setThreadPriority(Process.THREAD_PRIORITY_AUDIO) }
            var failure: Exception? = null
            var ready = false
            var lastCheckpoint = clock()
            try {
                if (!stopping.get()) {
                    val microphone = inputFactory()
                    synchronized(stopping) {
                        input = microphone
                        if (!stopping.get()) microphone.start()
                    }
                    if (!stopping.get()) opened.started()
                    val bytes = ByteArray(640)
                    while (!stopping.get()) {
                        val count = microphone.read(bytes)
                        if (count < 0) error("audio-read-failed")
                        if (count == 0) continue
                        check(count <= bytes.size && count % 2 == 0) { "audio-frame-invalid" }
                        opened.append(bytes, count)
                        if (!onPcm(bytes.copyOf(count))) error("audio-consumer-backpressure")
                        if (!ready && !stopping.get()) { ready = true; onReady() }
                        if (clock() - lastCheckpoint >= 1_000) {
                            opened.checkpoint(); lastCheckpoint = clock()
                        }
                    }
                }
            } catch (error: Exception) { failure = error }
            finally {
                stopping.set(true)
                if (!releaseInput()) failure = IllegalStateException("microphone-retirement-failed")
                val artifact = runCatching { opened.finish(if (failure != null) "recording-failed" else reason) }
                // A playable archive may exist despite a consumer failure; never promote that to ASR success.
                onStopped(failure?.let { Result.failure<RecordedAudioArtifact>(it) } ?: artifact)
            }
        }, "recorded-voice-audio").apply { isDaemon = true }
        worker!!.start()
    }

    fun finish() = stop("finished")
    private fun stop(stopReason: String) {
        synchronized(stopping) {
            if (stopping.compareAndSet(false, true)) reason = stopReason
            runCatching { input?.stop() }
        }
    }
    fun recognition(text: String?, error: String? = null) { recording?.recognition(text, error) }
    override fun close() {
        stop("cancelled")
        worker?.let { if (it !== Thread.currentThread()) it.join(500) }
        check(worker?.isAlive != true) { "recording-retirement-pending" }
        if (retirementFailed) check(releaseInput()) { "microphone-retirement-failed" }
    }
    private fun releaseInput(): Boolean = synchronized(stopping) {
        try { input?.close(); input = null; retirementFailed = false; true }
        catch (_: Exception) { retirementFailed = true; false }
    }

    private class AndroidPcmInput(private val context: Context) : PcmInput {
        private val sdk: AudioRecord
        init {
            val minimum = AudioRecord.getMinBufferSize(16_000, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
            check(minimum > 0) { "audio-format-unavailable" }
            if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED)
                throw SecurityException("microphone_permission_required")
            sdk = try {
                AudioRecord.Builder().setAudioSource(MediaRecorder.AudioSource.VOICE_RECOGNITION)
                    .setAudioFormat(AudioFormat.Builder().setSampleRate(16_000).setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT).build())
                    .setBufferSizeInBytes(maxOf(minimum * 2, 3_200)).build()
            } catch (failure: SecurityException) {
                throw SecurityException("microphone_permission_required", failure)
            }
            if (sdk.state != AudioRecord.STATE_INITIALIZED) { sdk.release(); error("audio-input-unavailable") }
        }
        override fun start() {
            if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED)
                throw SecurityException("microphone_permission_required")
            sdk.startRecording(); check(sdk.recordingState == AudioRecord.RECORDSTATE_RECORDING)
        }
        override fun read(bytes: ByteArray) = sdk.read(bytes, 0, bytes.size, AudioRecord.READ_BLOCKING)
        override fun stop() { if (sdk.recordingState == AudioRecord.RECORDSTATE_RECORDING) sdk.stop() }
        override fun close() { try { stop() } finally { sdk.release() } }
    }
}
