package dev.gitblame.voxgolem.assistant.runtime

interface AssistantClient {
    fun snapshot(): AssistantState
    fun subscribe(observer: (AssistantState) -> Unit): AutoCloseable
    fun resume()
    fun refresh()
    fun background()
    fun stop()
    fun setDraft(text: String)
    fun selectModel(model: String)
    fun selectSearchMode(mode: String) {}
    fun setWebEnabled(enabled: Boolean) { throw UnsupportedOperationException("Web policy unavailable") }
    fun setSpeechNetworkFallback(enabled: Boolean) { throw UnsupportedOperationException("Speech policy unavailable") }
    fun setSpeechSelection(selection: dev.gitblame.voxgolem.assistant.speech.SpeechSelection) { throw UnsupportedOperationException("Speech selections unavailable") }
    /** Completion is the actor's policy commit/reject, never an observer/snapshot acknowledgement. */
    fun setSpeechSelection(selection: dev.gitblame.voxgolem.assistant.speech.SpeechSelection, onSettled: (AssistantState) -> Unit) {
        throw UnsupportedOperationException("Speech selection settlement unavailable")
    }
    fun publishSpeechOptions(options: dev.gitblame.voxgolem.assistant.speech.SpeechOptions) {}
    fun newConversation()
    fun selectConversation(id: String)
    fun selectHead(id: String)
    fun send()
    fun history(before: String? = null, onSettled: (String?, AssistantState) -> Unit) { onSettled(null, snapshot()) }
}
