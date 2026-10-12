package dev.gitblame.voxgolem.assistant.phone

import org.junit.Assert.*
import org.junit.Test

class NewVoiceShortcutPolicyTest {
    private val token = "11111111-1111-4111-8111-111111111111"
    @Test fun coldWarmAndRepeatedTapsAreConsumedOnceAndCoalesced() {
        val gate = ShortcutActionGate()
        assertTrue(gate.offer(token)); assertFalse(gate.offer("22222222-2222-4222-8222-222222222222"))
        assertEquals(token, gate.consume()); assertNull(gate.consume())
        assertTrue(gate.active(token)); gate.release(token); assertFalse(gate.offer(token))
        assertTrue(gate.offer("22222222-2222-4222-8222-222222222222"))
    }
    @Test fun restoredMainIntentIsNotALaunchAndBackgroundRetiresPendingConsent() {
        assertNull(ShortcutActionGate().consume())
        val gate = ShortcutActionGate(); gate.offer(token); gate.background(); assertNull(gate.consume())
        assertFalse(gate.offer("synthetic-untrusted-prompt"))
    }
    @Test fun notificationRequiresTheExactPendingTokenAndDoesNotReplayAfterConsumption() {
        val gate = ShortcutActionGate()
        gate.offer(token)
        assertFalse(gate.notifyPending("22222222-2222-4222-8222-222222222222"))
        assertTrue(gate.notifyPending(token)); assertFalse(gate.notifyPending(token))
        assertEquals(token, gate.consume()); assertFalse(gate.notifyPending(token))
        gate.background(); assertFalse(gate.notifyPending())
    }
    @Test fun stopBeforeAnySpeechCompletesForReviewWithoutWaitingForNewAdmission() {
        for (phase in listOf("idle", "checking")) {
            val policy = StopSendNewPolicy()
            policy.state(phase, "", null)
            assertEquals(StopSendNewPolicy.Action.Review("", "No speech captured; nothing was sent"), policy.stopAndSend())
            assertEquals(StopSendNewPolicy.Action.None, policy.state("final", "Synthetic late final", null))
        }
    }
    @Test fun silenceFinalRestartsButNeverSendsUntilExplicitStop() {
        val policy = StopSendNewPolicy()
        assertEquals(StopSendNewPolicy.Action.Restart("Synthetic first segment"), policy.state("final", "Synthetic first segment", null))
        policy.state("listening", "Synthetic first segment", null)
        policy.state("listening", "Synthetic first segment partial", null)
        assertEquals(StopSendNewPolicy.Action.None, policy.stopAndSend())
        assertEquals(StopSendNewPolicy.Action.Submit("Synthetic first segment final words"), policy.state("final", "Synthetic first segment final words", null))
        assertEquals(StopSendNewPolicy.Action.None, policy.state("final", "Duplicate synthetic final", null))
    }
    @Test fun stopAfterProviderFinalSendsTheAuthoritativeAccumulationExactlyOnce() {
        val policy = StopSendNewPolicy(); policy.state("final", "Synthetic complete words", null)
        assertEquals(StopSendNewPolicy.Action.Submit("Synthetic complete words"), policy.stopAndSend())
        assertEquals(StopSendNewPolicy.Action.None, policy.stopAndSend())
    }
    @Test fun timeoutErrorNoSpeechAndCancellationNeverSubmitPartials() {
        val timeout = StopSendNewPolicy(); timeout.state("listening", "Synthetic partial", null); timeout.stopAndSend()
        assertTrue(timeout.timeout() is StopSendNewPolicy.Action.Review)
        val error = StopSendNewPolicy(); assertTrue(error.state("error", "", "no_speech") is StopSendNewPolicy.Action.Review)
        val cancelled = StopSendNewPolicy(); cancelled.cancel(); assertEquals(StopSendNewPolicy.Action.None, cancelled.state("final", "Synthetic late words", null))
    }
    @Test fun errorAfterAnAuthoritativeSegmentStillRequiresManualReview() {
        val policy = StopSendNewPolicy()
        policy.state("final", "Synthetic completed segment", null)
        policy.state("checking", "Synthetic completed segment", null)
        assertEquals(StopSendNewPolicy.Action.Review("Synthetic completed segment", "no_speech"), policy.state("error", "Synthetic completed segment", "no_speech"))
        assertEquals(StopSendNewPolicy.Action.None, policy.stopAndSend())
    }
    @Test fun boundedSegmentsAndUtf8QuotaFailForReviewRatherThanAutoSending() {
        val policy = StopSendNewPolicy(2)
        policy.state("final", "Synthetic segment one", null)
        policy.state("listening", "Synthetic segment one", null)
        assertTrue(policy.state("final", "Synthetic segment one segment two", null) is StopSendNewPolicy.Action.Review)
        assertEquals(StopSendNewPolicy.Action.None, policy.stopAndSend())
        val oversized = StopSendNewPolicy()
        assertTrue(oversized.state("listening", "é".repeat(16385), null) is StopSendNewPolicy.Action.Review)
    }
}
