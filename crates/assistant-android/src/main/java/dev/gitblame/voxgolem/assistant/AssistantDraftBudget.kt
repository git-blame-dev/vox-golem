package dev.gitblame.voxgolem.assistant

/** Assistant draft/prompt byte budget. Empty drafts are valid; send admission checks blankness separately. */
object AssistantDraftBudget {
    const val MAX_BYTES = 32 * 1024
    fun accepts(text: String): Boolean = text.toByteArray(Charsets.UTF_8).size <= MAX_BYTES
}
