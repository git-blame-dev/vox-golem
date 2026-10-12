package dev.gitblame.voxgolem.assistant.inputtransport

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Handler
import android.os.Looper
import java.util.concurrent.atomic.AtomicBoolean

/** Assistant-only ephemeral stream; caller must acquire CHAT before open and never use on phone. */
class AndroidWatchMicrophoneFactory(private val context: Context) : WatchMicrophoneFactory {
    override fun open(): WatchMicrophone {
        requireInputMain()
        check(context.packageManager.hasSystemFeature(PackageManager.FEATURE_WATCH)) { "watch-only-audio-source" }
        check(context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED)
        val minimum = AudioRecord.getMinBufferSize(AssistantInputWire.SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        check(minimum > 0)
        val record = AudioRecord.Builder().setAudioSource(MediaRecorder.AudioSource.VOICE_RECOGNITION)
            .setAudioFormat(AudioFormat.Builder().setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(AssistantInputWire.SAMPLE_RATE).setChannelMask(AudioFormat.CHANNEL_IN_MONO).build())
            .setBufferSizeInBytes(maxOf(minimum, AssistantInputWire.PCM_CHUNK_BYTES * 4)).build()
        if (record.state != AudioRecord.STATE_INITIALIZED) { record.release(); error("watch-audio-unavailable") }
        return Session(record)
    }
    private class Session(private val sdk: AudioRecord) : WatchMicrophone {
        private val closing = AtomicBoolean(false)
        private val main = Handler(Looper.getMainLooper())
        private var worker: Thread? = null
        private var released = false
        override fun start(pcm: (ByteArray) -> Boolean, error: () -> Unit) {
            requireInputMain(); check(worker == null && !closing.get())
            sdk.startRecording()
            check(sdk.recordingState == AudioRecord.RECORDSTATE_RECORDING)
            worker = Thread({
                val buffer = ByteArray(AssistantInputWire.PCM_CHUNK_BYTES)
                val deadline = System.nanoTime() + AssistantInputWire.MAX_SESSION_MILLIS * 1000000
                var lastRead = System.nanoTime()
                try {
                    while (!closing.get()) {
                        check(System.nanoTime() < deadline)
                        val count = sdk.read(buffer, 0, buffer.size, AudioRecord.READ_NON_BLOCKING)
                        check(count >= 0 && count % 2 == 0)
                        if (count == 0) {
                            check(System.nanoTime() - lastRead < 1000000000L)
                            Thread.sleep(10)
                        } else {
                            lastRead = System.nanoTime()
                            check(pcm(buffer.copyOf(count))) { "assistant-audio-backpressure" }
                        }
                    }
                } catch (_: Exception) { if (!closing.get()) main.post { if (!closing.get()) error() } }
                finally {
                    // The read owner stops recording even if the UI dispatcher is delayed.
                    runCatching { sdk.stop() }
                        .onFailure { if (!closing.get()) main.post { if (!closing.get()) error() } }
                }
            }, "assistant-watch-audio").apply { isDaemon = true; start() }
        }
        override fun close() {
            requireInputMain()
            if (released) return
            closing.set(true)
            worker?.interrupt(); worker?.join(500)
            check(worker?.isAlive != true) { "watch-audio-retirement-failed" }
            try { if (sdk.recordingState == AudioRecord.RECORDSTATE_RECORDING) sdk.stop() }
            finally { sdk.release(); released = true }
        }
    }
}
