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
        policy.onMicrophoneReady("capture")
        establishRaisedReference(policy)
        for (time in 600L..2_100L step 250) assertEquals(null, lowered(policy, time))
    }

    @Test fun motionCannotArmOrStopBeforeMicrophoneReadiness() {
        val policy = ProvisionalMotionStopPolicy(loweringAngleDegrees = 50f)
        establishRaisedReference(policy)
        for (time in 600L..1_350L step 250) assertEquals(null, lowered(policy, time))
        policy.onMicrophoneReady("capture")
        for (time in 1_600L..2_850L step 250) assertEquals(null, lowered(policy, time))
    }

    @Test fun calibratedCandidateRequiresStableReferenceAndSustainedLowering() {
        val policy = ProvisionalMotionStopPolicy(loweringAngleDegrees = 50f)
        policy.onMicrophoneReady("capture")
        establishRaisedReference(policy)
        assertEquals(null, lowered(policy, 600))
        assertEquals(null, raised(policy, 700))
        for (time in 800L..1_300L step 250) assertEquals(null, lowered(policy, time))
        assertEquals(motionStop(), lowered(policy, 1_550))
    }

    @Test fun queuedPreReadinessSamplesCannotEstablishTheSpeakingReference() {
        val policy = ProvisionalMotionStopPolicy(loweringAngleDegrees = 50f)
        policy.onMicrophoneReady("capture", 1_000)
        establishRaisedReference(policy)
        for (time in 1_000L..2_250L step 250) assertEquals(null, lowered(policy, time))
    }

    @Test fun stopIsLatchedDespiteMoreSamplesAndRepeatedReadiness() {
        val policy = readyPolicy()
        for (time in 600L..1_100L step 250) assertEquals(null, lowered(policy, time))
        assertEquals(motionStop(), lowered(policy, 1_350))
        policy.onMicrophoneReady("capture")
        for (time in 1_600L..3_100L step 250) assertEquals(null, lowered(policy, time))
    }

    @Test fun repeatedReadinessDoesNotRestartReferenceOrHold() {
        val policy = ProvisionalMotionStopPolicy(loweringAngleDegrees = 50f)
        policy.onMicrophoneReady("capture")
        assertEquals(null, raised(policy, 0))
        assertEquals(null, raised(policy, 250))
        policy.onMicrophoneReady("capture")
        assertEquals(null, raised(policy, 500))
        assertEquals(null, lowered(policy, 600))
        assertEquals(null, lowered(policy, 850))
        policy.onMicrophoneReady("capture")
        assertEquals(null, lowered(policy, 1_100))
        assertEquals(motionStop(), lowered(policy, 1_350))
    }

    @Test fun readinessAndSamplesBelongToOneCapture() {
        val policy = readyPolicy()
        for (time in 600L..1_600L step 250) assertEquals(null, lowered(policy, time, "other-capture"))
        for (time in 1_850L..2_350L step 250) assertEquals(null, lowered(policy, time))
        assertEquals(motionStop(), lowered(policy, 2_600))
        policy.onMicrophoneReady("next-capture")
        establishRaisedReference(policy, "next-capture", 3_000)
        for (time in 3_600L..4_100L step 250) {
            assertEquals(null, lowered(policy, time, "capture"))
            assertEquals(null, lowered(policy, time, "next-capture"))
        }
        assertEquals(motionStop("next-capture"), lowered(policy, 4_350, "next-capture"))
    }

    @Test fun newCaptureReadinessClearsAnUnfinishedLoweringHold() {
        val policy = readyPolicy()
        assertEquals(null, lowered(policy, 600))
        assertEquals(null, lowered(policy, 850))
        policy.onMicrophoneReady("next-capture")
        for (time in 1_000L..2_250L step 250) assertEquals(null, lowered(policy, time, "next-capture"))
    }

    @Test fun resetRequiresReadinessAgainAndClearsTheStopLatch() {
        val policy = readyPolicy()
        for (time in 600L..1_100L step 250) assertEquals(null, lowered(policy, time))
        assertEquals(motionStop(), lowered(policy, 1_350))
        policy.reset()
        establishRaisedReference(policy)
        for (time in 600L..1_350L step 250) assertEquals(null, lowered(policy, time))
        policy.onMicrophoneReady("capture")
        establishRaisedReference(policy, startMs = 2_000)
        for (time in 2_600L..3_100L step 250) assertEquals(null, lowered(policy, time))
        assertEquals(motionStop(), lowered(policy, 3_350))
    }

    @Test fun invalidGravityVectorsInterruptLoweringHold() {
        val invalidVectors = listOf(
            floatArrayOf(0f, 0f, 0f),
            floatArrayOf(20f, 0f, 0f),
            floatArrayOf(Float.NaN, 0f, 0f),
            floatArrayOf(Float.POSITIVE_INFINITY, 0f, 0f),
        )
        for (vector in invalidVectors) {
            val policy = readyPolicy()
            assertEquals(null, lowered(policy, 600))
            assertEquals(null, lowered(policy, 850))
            assertEquals(null, policy.onGravitySample("capture", vector[0], vector[1], vector[2], 950))
            for (time in 1_100L..1_600L step 250) assertEquals(null, lowered(policy, time))
            assertEquals(motionStop(), lowered(policy, 1_850))
        }
    }

    @Test fun longSampleGapRestartsLoweringHold() {
        val policy = readyPolicy()
        for (time in 600L..1_100L step 250) assertEquals(null, lowered(policy, time))
        for (time in 1_850L..2_350L step 250) assertEquals(null, lowered(policy, time))
        assertEquals(motionStop(), lowered(policy, 2_600))
    }

    @Test fun duplicateOrBackwardSampleTimeInterruptsLoweringHold() {
        for (discontinuousTime in listOf(850L, 700L)) {
            val policy = readyPolicy()
            assertEquals(null, lowered(policy, 600))
            assertEquals(null, lowered(policy, 850))
            assertEquals(null, lowered(policy, discontinuousTime))
            for (time in 1_100L..1_600L step 250) assertEquals(null, lowered(policy, time))
            assertEquals(motionStop(), lowered(policy, 1_850))
        }
    }

    @Test fun referenceDurationCannotSpanInvalidSamplesOrLongGaps() {
        for (invalidSample in listOf(false, true)) {
            val policy = ProvisionalMotionStopPolicy(loweringAngleDegrees = 50f)
            policy.onMicrophoneReady("capture")
            assertEquals(null, raised(policy, 0))
            assertEquals(null, raised(policy, 250))
            if (invalidSample) assertEquals(null, policy.onGravitySample("capture", 0f, 0f, 0f, 400))
            val resumedAt = if (invalidSample) 500L else 600L
            for (time in resumedAt..(resumedAt + 1_250) step 250) assertEquals(null, lowered(policy, time))
        }
    }

    private fun readyPolicy() = ProvisionalMotionStopPolicy(loweringAngleDegrees = 50f).also {
        it.onMicrophoneReady("capture")
        establishRaisedReference(it)
    }

    private fun establishRaisedReference(policy: ProvisionalMotionStopPolicy, captureId: String = "capture", startMs: Long = 0) {
        for (time in startMs..(startMs + 500) step 250) assertEquals(null, raised(policy, time, captureId))
    }

    private fun raised(policy: ProvisionalMotionStopPolicy, time: Long, captureId: String = "capture") =
        policy.onGravitySample(captureId, 0f, 0f, 9.8f, time)

    private fun lowered(policy: ProvisionalMotionStopPolicy, time: Long, captureId: String = "capture") =
        policy.onGravitySample(captureId, 9.8f, 0f, 0f, time)

    private fun motionStop(captureId: String = "capture") = StopRequest(captureId, "provisional_motion_lowering")
}
