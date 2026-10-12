package dev.gitblame.voxgolem.assistant.phone

/** Completion policy layered on SharedDictationOwner, not a recognizer or chat coordinator. */
class StopSendNewPolicy(private val maximumSegments: Int = 32) {
    sealed class Action {
        object None : Action()
        data class Restart(val prefix: String) : Action()
        data class Submit(val text: String) : Action()
        data class Review(val text: String, val reason: String) : Action()
    }
    var latest: String = ""
        private set
    private var authoritative = ""
    private var phase = "idle"
    private var finishing = false
    private var terminal = false
    private var segments = 0

    fun state(phase: String, draft: String, error: String?): Action {
        if (terminal) return Action.None
        if (phase == "final" && this.phase == "final") return Action.None
        this.phase = phase
        if (draft.toByteArray(Charsets.UTF_8).size > 32 * 1024) return review("Dictation exceeded the draft limit; review before sending")
        latest = draft
        if (phase == "error") {
            return review(error ?: "Recognition failed; review before sending")
        }
        if (phase != "final") return Action.None
        authoritative = draft
        segments++
        if (finishing) return submit()
        if (segments >= maximumSegments) return review("Dictation segment limit reached; review before sending")
        return Action.Restart(authoritative)
    }
    fun stopAndSend(): Action {
        if (terminal || finishing) return Action.None
        finishing = true
        return if (phase in setOf("final", "checking", "idle") && latest == authoritative) submit() else Action.None
    }
    fun timeout(): Action = if (terminal) Action.None else review("Final recognition timed out; review the available text and send manually")
    fun cancel() { terminal = true }
    fun finishing() = finishing
    fun terminal() = terminal
    private fun submit(): Action {
        if (authoritative.isBlank()) return review("No speech captured; nothing was sent")
        terminal = true
        return Action.Submit(authoritative)
    }
    private fun review(reason: String): Action { terminal = true; return Action.Review(latest, reason) }
}
