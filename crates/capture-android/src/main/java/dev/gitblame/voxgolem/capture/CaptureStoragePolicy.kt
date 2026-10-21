package dev.gitblame.voxgolem.capture

/** Admission estimate, not a disk reservation. PCM survives until finalized WAV metadata commits. */
object CaptureStoragePolicy {
    const val METADATA_ALLOWANCE_BYTES = 4L * 1024 * 1024

    fun peakBytes(format: CaptureFormat, maximumMillis: Long): Long {
        require(CaptureProtocol.validateFormat(format))
        require(maximumMillis >= 0)
        val frames = Math.multiplyExact(maximumMillis, format.sampleRate.toLong()) / 1_000
        val pcm = Math.multiplyExact(frames, (format.channels * format.bitsPerSample / 8).toLong())
        return Math.addExact(Math.multiplyExact(pcm, 2), 44 + METADATA_ALLOWANCE_BYTES)
    }
}
