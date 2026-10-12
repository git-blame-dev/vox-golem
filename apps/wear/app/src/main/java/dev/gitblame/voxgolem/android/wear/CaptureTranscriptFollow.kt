package dev.gitblame.voxgolem.android.wear

/** Only new transcript content arms a follow request; measurements settle that request. */
internal class CaptureTranscriptFollow {
    private var requested: String? = null
    private var pending = false

    fun observe(transcript: String) {
        if (requested != transcript) {
            requested = transcript
            pending = true
        }
    }

    fun distance(transcript: String, measuredTranscript: String?, measuredHeight: Int, itemHeight: Int,
                 viewportHeight: Int, bottomPadding: Int, scrolling: Boolean): Float? {
        if (!pending || requested != transcript || measuredTranscript != transcript || scrolling ||
            measuredHeight <= 0 || itemHeight != measuredHeight || viewportHeight <= 0) return null
        return (itemHeight.toFloat() + bottomPadding).coerceAtLeast(0f)
    }

    fun completed(transcript: String) {
        if (requested == transcript) pending = false
    }
}
