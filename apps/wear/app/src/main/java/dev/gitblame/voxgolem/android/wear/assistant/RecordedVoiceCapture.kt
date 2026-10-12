package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Context
import dev.gitblame.voxgolem.assistant.recognition.RecordedAudioCapture
import dev.gitblame.voxgolem.assistant.recognition.RecordedAudioFactory
import dev.gitblame.voxgolem.assistant.recognition.RecordedAudioResult
import dev.gitblame.voxgolem.capture.CapturePurpose
import dev.gitblame.voxgolem.capture.RecordedAudioSession
import dev.gitblame.voxgolem.capture.RecordedAudioStore
import dev.gitblame.voxgolem.android.wear.capture.CaptureLifetime

/** Thin local adapter: Assistant input has no Note transport/export or conversation-storage access. */
internal fun recordedVoiceCapture(context: Context): RecordedAudioFactory {
    val store = RecordedAudioStore(context, "watch")
    return RecordedAudioFactory { request ->
        val lifetime = checkNotNull(CaptureLifetime.tryAcquire()) { "Previous capture is still retiring" }
        val session = RecordedAudioSession(store,
            CapturePurpose.valueOf(request.purpose.name), request.language, request.networkFallback,
            onIdentity = lifetime::bindCapture)
        object : RecordedAudioCapture {
            override fun start(pcm: (ByteArray) -> Boolean, ready: () -> Unit, stopped: (Result<RecordedAudioResult>) -> Unit) =
                session.start(pcm, ready) { result -> stopped(result.map {
                    RecordedAudioResult(it.captureId, it.manifest.sha256, it.manifest.frameCount)
                }) }
            override fun finish() = session.finish()
            override fun recognition(text: String?, error: String?) = session.recognition(text, error)
            override fun close() { session.close(); lifetime.close() }
        }
    }
}
