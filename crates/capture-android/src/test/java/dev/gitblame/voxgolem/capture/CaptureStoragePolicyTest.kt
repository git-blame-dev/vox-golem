package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test

class CaptureStoragePolicyTest {
    @Test fun peakIncludesBothPcmAndWavAtEverySupportedRate() {
        for (rate in listOf(8_000, 16_000, 48_000)) for (channels in 1..2) {
            val format = CaptureFormat(rate, channels)
            val peak = CaptureStoragePolicy.peakBytes(format, 1_000)
            assertEquals(rate.toLong() * channels * 2 * 2 + 44 + CaptureStoragePolicy.METADATA_ALLOWANCE_BYTES, peak)
        }
    }

    @Test fun invalidAndOverflowingDurationsAreRejected() {
        assertThrows(IllegalArgumentException::class.java) { CaptureStoragePolicy.peakBytes(CaptureFormat(), -1) }
        assertThrows(ArithmeticException::class.java) { CaptureStoragePolicy.peakBytes(CaptureFormat(), Long.MAX_VALUE) }
    }
}
