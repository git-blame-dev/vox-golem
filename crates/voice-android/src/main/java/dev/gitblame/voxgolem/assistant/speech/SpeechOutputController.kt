package dev.gitblame.voxgolem.assistant.speech

import dev.gitblame.voxgolem.assistant.AssistantTiming

interface SpeechOutputController {
    val enabled: Boolean get() = true
    fun speakStreamChunk(text: String, language: String?, flush: Boolean, keepOpen: Boolean = false,
        timingParent: AssistantTiming? = null, onDone: (String?) -> Unit = {})
    fun finishStream()
    /** Report actual lease release/cleanup outcome, after any posted SDK work. */
    fun finishStream(onDone: (String?) -> Unit)
    fun speak(finalText: String, language: String, timingParent: AssistantTiming? = null, onTerminal: (String?) -> Unit)
    fun stop()
}

fun LocalSpeechOutput.asController(): SpeechOutputController = object : SpeechOutputController {
    override val enabled: Boolean get() = this@asController.snapshot().enabled
    override fun speakStreamChunk(text: String, language: String?, flush: Boolean, keepOpen: Boolean, timingParent: AssistantTiming?, onDone: (String?) -> Unit) {
        this@asController.speakStreamChunk(text, language, flush, keepOpen, timingParent, onDone)
    }
    override fun finishStream() { this@asController.finishStream() }
    override fun finishStream(onDone: (String?) -> Unit) { this@asController.finishStream(onDone) }
    override fun speak(finalText: String, language: String, timingParent: AssistantTiming?, onTerminal: (String?) -> Unit) {
        this@asController.speak(finalText, language, timingParent, onTerminal)
    }
    override fun stop() { this@asController.stop() }
}
