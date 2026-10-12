package dev.gitblame.voxgolem.assistant.runtime

import org.junit.Assert.*
import org.junit.Test

class AssistantDeliveryGateTest {
    @Test fun statusDoesNotReduceExactTextCallbackBudget() {
        lateinit var gate: AcknowledgedAssistantDelivery
        gate = AcknowledgedAssistantDelivery("synthetic-client", { packet ->
            assertTrue(gate.acknowledge(packet.clientRequestToken, packet.providerRequestId, packet.sequence))
        }, timeoutMillis = 100)
        assertTrue(gate.deliverStatus(AssistantToolStatus("synthetic-client", "synthetic-provider", 1, "Searching")))
        for (sequence in 2L..262145L) {
            assertTrue(gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", sequence, "x")))
        }
        assertTrue(gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 262145, "synthetic-complete")))
    }

    @Test fun toolStatusSharesCorrelationAndAcknowledgementWithoutBecomingText() {
        val packets = mutableListOf<AssistantDeliveryPacket>()
        lateinit var gate: AcknowledgedAssistantDelivery
        gate = AcknowledgedAssistantDelivery("synthetic-client", { packet ->
            packets += packet
            gate.acknowledge(packet.clientRequestToken, packet.providerRequestId, packet.sequence)
        }, timeoutMillis = 100)
        assertTrue(gate.deliverStatus(AssistantToolStatus("synthetic-client", "synthetic-provider", 1, "Searching")))
        assertTrue(gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 2, "Synthetic answer")))
        assertTrue(gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 2, "synthetic-complete")))
        assertEquals(listOf(1L, 2L, 3L), packets.map { it.sequence })
        assertEquals("Searching", packets.first().toolStatus)
        assertNull(packets.first().delta)
        assertNull(packets.first().candidate)
    }

    @Test fun statusSinkFailureOrUnboundedContentCannotAuthorizeCommit() {
        for (status in listOf("Searching", "Synthetic query or secret")) {
            val gate = AcknowledgedAssistantDelivery("synthetic-client", { error("Synthetic sink failure") }, timeoutMillis = 10)
            assertFalse(gate.deliverStatus(AssistantToolStatus("synthetic-client", "synthetic-provider", 1, status)))
            assertFalse(gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 1, "synthetic-complete")))
        }
    }

    @Test fun sendingWithoutAcknowledgementCannotAuthorizeCommit() {
        val gate = AcknowledgedAssistantDelivery("synthetic-client", {}, timeoutMillis = 10)
        assertTrue(gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 1, "Synthetic")))
        assertFalse(gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 1, "synthetic-complete")))
    }
    @Test fun acknowledgementMustMatchBothIdentifiersAndBeContiguousThroughCandidate() {
        val packets = mutableListOf<AssistantDeliveryPacket>()
        lateinit var gate: AcknowledgedAssistantDelivery
        gate = AcknowledgedAssistantDelivery("synthetic-client", { packet ->
            packets += packet
            if (packet.candidate != null) {
                assertFalse(gate.acknowledge("synthetic-other", "synthetic-provider", packet.sequence))
                assertFalse(gate.acknowledge("synthetic-client", "synthetic-other", packet.sequence))
                assertFalse(gate.acknowledge("synthetic-client", "synthetic-provider", packet.sequence + 1))
                assertTrue(gate.acknowledge("synthetic-client", "synthetic-provider", packet.sequence))
                assertTrue(gate.acknowledge("synthetic-client", "synthetic-provider", 1))
            }
        }, timeoutMillis = 100)
        assertTrue(gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 1, "Synthetic")))
        assertTrue(gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 1, "synthetic-complete")))
        assertEquals(listOf(1L, 2L), packets.map { it.sequence })
        assertEquals(2L, gate.contiguousAcknowledgement())
    }
    @Test fun gapInDeltaAcknowledgementsCannotBeHiddenByCandidateAcknowledgement() {
        lateinit var gate: AcknowledgedAssistantDelivery
        gate = AcknowledgedAssistantDelivery("synthetic-client", { packet ->
            if (packet.candidate != null) gate.acknowledge(packet.clientRequestToken, packet.providerRequestId, packet.sequence)
        }, timeoutMillis = 10)
        gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 1, "Synthetic"))
        assertFalse(gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 1, "synthetic-complete")))
        assertEquals(0L, gate.contiguousAcknowledgement())
    }
    @Test fun boundedWindowAndCancellationWakeBlockedDeliveryWithoutAnotherPacket() {
        val packets = mutableListOf<AssistantDeliveryPacket>()
        val gate = AcknowledgedAssistantDelivery("synthetic-client", { packets += it }, maxInFlight = 1, timeoutMillis = 1000)
        assertTrue(gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 1, "Synthetic")))
        val thread = Thread { assertFalse(gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 2, "Synthetic late"))) }
        thread.start()
        gate.cancel()
        thread.join(1000)
        assertFalse(thread.isAlive)
        assertEquals(1, packets.size)
    }
    @Test fun duplicateAcknowledgementsAreIdempotentAndWrongDeltaSequenceFailsClosed() {
        val gate = AcknowledgedAssistantDelivery("synthetic-client", {}, timeoutMillis = 10)
        assertTrue(gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 1, "Synthetic")))
        assertTrue(gate.acknowledge("synthetic-client", "synthetic-provider", 1))
        assertTrue(gate.acknowledge("synthetic-client", "synthetic-provider", 1))
        assertFalse(gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 3, "Synthetic gap")))
    }
}
