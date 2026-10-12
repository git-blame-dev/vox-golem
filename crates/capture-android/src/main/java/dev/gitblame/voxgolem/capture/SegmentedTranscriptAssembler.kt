package dev.gitblame.voxgolem.capture

/** Turns segmented commits plus the mutable current hypothesis into one full-text snapshot. */
class SegmentedTranscriptAssembler {
    private val committed = StringBuilder()

    fun commit(segment: String): String {
        val normalized = segment.trim()
        if (normalized.isNotEmpty()) {
            if (committed.isNotEmpty()) committed.append(' ')
            committed.append(normalized)
        }
        return committed.toString()
    }

    fun hypothesis(text: String): String = join(committed.toString(), text)
    fun finish(text: String): String = join(committed.toString(), text)

    private fun join(prefix: String, suffix: String): String {
        if (prefix.isBlank()) return suffix
        if (suffix.isBlank() || suffix == prefix || suffix.startsWith("$prefix ")) return if (suffix.isBlank()) prefix else suffix
        return "$prefix ${suffix.trim()}"
    }
}
