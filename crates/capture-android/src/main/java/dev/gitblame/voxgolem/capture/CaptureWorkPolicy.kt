package dev.gitblame.voxgolem.capture

/** Capture-only portable retry policy; adapters own effects and resource admission. */
object CaptureWorkPolicy {
    const val MAX_AUTOMATIC_RECOGNITION_ATTEMPTS = 5
    fun recognitionDelaySeconds(attempts: Int): Long = (30L shl attempts.coerceIn(0, 4)).coerceAtMost(300L)
}
