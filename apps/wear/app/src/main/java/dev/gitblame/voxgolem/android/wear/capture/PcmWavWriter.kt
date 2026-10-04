package dev.gitblame.voxgolem.android.wear.capture

import dev.gitblame.voxgolem.capture.CaptureFormat
import dev.gitblame.voxgolem.capture.CaptureManifest
import dev.gitblame.voxgolem.capture.PcmRecordingWriter
import java.io.Closeable
import java.io.File

/** Thin watch-specific facade over the shared, checkpointed PCM/WAV writer. */
class PcmWavWriter(pcmFile: File, wavFile: File, sampleRate: Int) : Closeable {
    private val delegate = PcmRecordingWriter(pcmFile, CaptureFormat(sampleRate, 1, 16))
    private val captureId = pcmFile.parentFile?.name ?: error("PCM spool must be in a capture directory")
    private val wavFile = wavFile
    private var finished: CaptureManifest? = null
    var frameCount: Long = pcmFile.length() / 2
        private set

    @Synchronized fun append(bytes: ByteArray, count: Int = bytes.size) {
        delegate.append(bytes, count)
        frameCount += count / 2
    }

    @Synchronized fun checkpoint() {
        frameCount = delegate.checkpoint().frameCount
    }

    @Synchronized fun finish(): CaptureManifest = finished ?: delegate.finish(wavFile, captureId).also { finished = it }

    override fun close() { delegate.close() }

}
