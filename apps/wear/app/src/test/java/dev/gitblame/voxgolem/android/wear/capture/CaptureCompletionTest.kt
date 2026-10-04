package dev.gitblame.voxgolem.android.wear.capture

import dev.gitblame.voxgolem.capture.CaptureState
import org.junit.Assert.assertEquals
import org.junit.Test

class CaptureCompletionTest {
    @Test
    fun onlyDeliberateStopsProduceAnAutomaticallySubmittableCapture() {
        assertEquals(CaptureState.SAVED, completedCaptureState("manual_stop"))
        assertEquals(CaptureState.SAVED, completedCaptureState("provisional_motion_lowering"))
        for (reason in listOf("audio_read_error", "capture_io_error", "limit_reached", "service_destroyed")) {
            assertEquals(CaptureState.INTERRUPTED, completedCaptureState(reason))
        }
    }
}
