package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.recognition.DictationState

/** Foreground-only, once-per-utterance microphone feedback for both mobile shells. */
class MicrophoneCueController(private val output: ListeningCue) : AutoCloseable {
    private var visible = false
    private var closed = false
    private var suppressedUtterance = 0L
    private var latestUtterance = 0L
    private var retiredThrough = 0L
    private var listeningUtterance: Long? = null

    fun foreground(visible: Boolean) {
        if (closed) return
        this.visible = visible
        if (!visible) cancel()
    }

    fun onDictation(value: DictationState) {
        if (closed) return
        if (!visible) {
            latestUtterance = maxOf(latestUtterance, value.utterance)
            retiredThrough = maxOf(retiredThrough, value.utterance)
            return
        }
        if (value.utterance < latestUtterance) return
        if (value.utterance > latestUtterance) {
            listeningUtterance = null
            output.stop()
            latestUtterance = value.utterance
        }
        when (value.phase) {
            DictationPhase.LISTENING -> if (value.utterance > retiredThrough) {
                listeningUtterance = value.utterance
                if (value.readyForSpeech && value.utterance != suppressedUtterance) {
                    suppressedUtterance = value.utterance
                    output.play()
                }
            }
            DictationPhase.FINISHING -> finish(value.utterance)
            DictationPhase.FINAL -> if (value.utterance > retiredThrough && listeningUtterance == value.utterance) {
                // SharedDictationOwner publishes FINAL only after successful SDK retirement.
                // A finish gesture/FINISHING projection is not proof that the microphone stopped.
                retiredThrough = value.utterance
                suppressedUtterance = value.utterance
                listeningUtterance = null
                output.finish()
            }
            DictationPhase.ERROR -> cancel()
            DictationPhase.CHECKING -> { listeningUtterance = null; output.stop() }
            DictationPhase.IDLE -> if (listeningUtterance != null || value.utterance == suppressedUtterance) cancel()
        }
    }

    /** Synchronous silence/suppression only. Successful FINAL owns the end sound. */
    fun finish(utterance: Long) {
        if (!visible || closed) return
        if (utterance < latestUtterance || utterance <= retiredThrough) return
        latestUtterance = maxOf(latestUtterance, utterance)
        suppressedUtterance = utterance
        output.stop()
    }

    /** Called synchronously at the shared output's SPEAKING boundary, before SDK speak(). */
    fun onSpeech(value: SpeechState) { if (value.phase == SpeechPhase.SPEAKING) cancel() }

    fun cancel() {
        if (closed) return
        retiredThrough = maxOf(retiredThrough, latestUtterance)
        listeningUtterance = null
        output.stop()
    }

    override fun close() {
        if (closed) return
        closed = true
        visible = false
        listeningUtterance = null
        output.close()
    }
}
