package dev.gitblame.voxgolem.android.wear.stop

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class StopPolicyTest {
    @Test fun onlyExplicitManualActionRequestsStop() {
        assertEquals(StopRequest("capture", "manual_stop"), ManualStopPolicy.onManualStop("capture"))
    }

    @Test fun gravityDiagnosticsNeverProduceAStopRequest() {
        val events = mutableListOf<String>()
        val diagnostics = MotionStopDiagnostics(events::add)
        diagnostics.onGravitySample("capture", 0f, 0f, 9.8f, 500)
        diagnostics.onGravitySample("capture", 9.8f, 0f, 0f, 600)
        assertTrue(events.isNotEmpty())
        assertTrue(events.all { it.startsWith("gravity_angle_degrees=") })
    }

    @Test fun ambientTransitionsAreDiagnosticOnly() {
        val events = mutableListOf<String>()
        val diagnostics = AmbientStopDiagnostics(events::add)
        diagnostics.onInteractiveChanged(false, 1_000)
        diagnostics.onAmbientChanged(true, 1_100)
        assertEquals(2, events.size)
        assertTrue(events.none { it.contains("stop") })
    }

    @Test fun unknownMotionThresholdKeepsPhysicalStopDisabled() {
        val policy = ProvisionalMotionStopPolicy(loweringAngleDegrees = null)
        assertEquals(null, policy.onGravitySample("capture", 0f, 0f, 9.8f, 500))
        assertEquals(null, policy.onGravitySample("capture", 9.8f, 0f, 0f, 2_000))
    }

    @Test fun calibratedCandidateRequiresStableReferenceAndSustainedLowering() {
        val policy = ProvisionalMotionStopPolicy(loweringAngleDegrees = 50f)
        assertEquals(null, policy.onGravitySample("capture", 0f, 0f, 9.8f, 0))
        assertEquals(null, policy.onGravitySample("capture", 0f, 0f, 9.8f, 250))
        assertEquals(null, policy.onGravitySample("capture", 0f, 0f, 9.8f, 500))
        assertEquals(null, policy.onGravitySample("capture", 9.8f, 0f, 0f, 600))
        assertEquals(null, policy.onGravitySample("capture", 0f, 0f, 9.8f, 700))
        assertEquals(null, policy.onGravitySample("capture", 9.8f, 0f, 0f, 800))
        assertEquals(StopRequest("capture", "provisional_motion_lowering"),
            policy.onGravitySample("capture", 9.8f, 0f, 0f, 1_550))
    }
}
