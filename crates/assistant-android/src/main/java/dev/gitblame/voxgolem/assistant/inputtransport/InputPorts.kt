package dev.gitblame.voxgolem.assistant.inputtransport

fun interface CallerForegroundSignal { fun isForeground(): Boolean }

/** Must verify the selected signed peer and both installation identities, not merely a node ID. */
fun interface SourcePeerBinding { fun accepts(node: String, source: String, target: String): Boolean }

data class InputOutcome(val session: String, val text: String, val authoritative: Boolean, val error: String? = null)
