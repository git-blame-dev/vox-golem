package dev.gitblame.voxgolem.android.wear.transport

import dev.gitblame.voxgolem.capture.CaptureReceiptChannel
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class OversizedReceiptDeliveryTest {
    private val captureId = "123e4567-e89b-42d3-a456-426614174010"

    @Test
    fun oversizedReceiptRequiresExactChannelPathAndMatchingValidCaptureId() {
        val path = CaptureReceiptChannel.path(captureId)
        assertEquals(captureId, CaptureReceiptChannel.captureId(path))
        assertNull(CaptureReceiptChannel.captureId("$path/extra"))
        assertNull(CaptureReceiptChannel.captureId("$path/123e4567-e89b-42d3-a456-426614174011"))
        assertNull(CaptureReceiptChannel.captureId("/voxgolem/capture/v1/receipt/not-a-uuid"))
    }
}
