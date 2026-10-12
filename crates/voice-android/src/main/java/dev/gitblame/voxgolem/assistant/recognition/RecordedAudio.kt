package dev.gitblame.voxgolem.assistant.recognition

enum class VoiceCapturePurpose { ASK, TALK, DRAFT }

/** App-local reference, not a canonical chat operation, filesystem payload or export instruction. */
data class RecordedAudioResult(val captureId: String, val sha256: String, val frameCount: Long)

data class RecordedAudioRequest(val purpose: VoiceCapturePurpose, val language: String, val networkFallback: Boolean)

/** Implemented by shells using capture-android; voice-android has no capture-module dependency. */
fun interface RecordedAudioFactory { fun open(request: RecordedAudioRequest): RecordedAudioCapture }
interface RecordedAudioCapture : AutoCloseable {
    /** PCM callback runs on the audio worker; it must be bounded and must not wait on main. */
    fun start(pcm: (ByteArray) -> Boolean, ready: () -> Unit, stopped: (Result<RecordedAudioResult>) -> Unit)
    fun finish()
    fun recognition(text: String?, error: String? = null)
}
