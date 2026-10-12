package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.runtime.AssistantDelta
import dev.gitblame.voxgolem.assistant.runtime.AssistantDeliveryBoundary
import org.junit.Assert.*
import org.junit.Test
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicInteger

class PhoneRequestDeliveryTest {
    @Test fun searchStatusUsesExistingStatusChannelAndCorrelatedAck() {
        lateinit var transport: PhoneRequestDelivery
        val packets = mutableListOf<JSONObject>()
        transport = PhoneRequestDelivery("synthetic-client", { packet ->
            packets += packet
            assertTrue(transport.acknowledge(packet.getString("providerRequestId"), packet.getString("revision"), true))
        }, { fail("delivery must succeed") })
        assertTrue(transport.gate.deliverStatus(dev.gitblame.voxgolem.assistant.runtime.AssistantToolStatus("synthetic-client", "synthetic-provider", 1, "Reading")))
        assertTrue(transport.gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 1, "{}")))
        assertEquals("status", packets.first().getJSONObject("update").getString("kind"))
        assertEquals("Reading", packets.first().getJSONObject("update").getString("message"))
        assertFalse(packets.first().getJSONObject("update").has("text"))
    }

    @Test fun forwardsRevisionDeltaAndRequiresFinalCandidateAck() {
        lateinit var transport: PhoneRequestDelivery
        val packets = mutableListOf<JSONObject>()
        transport = PhoneRequestDelivery("synthetic-client", { packet ->
            packets += packet
            assertTrue(transport.acknowledge(packet.getString("providerRequestId"), packet.getString("revision"), true))
        }, { fail("delivery must succeed") })
        assertTrue(transport.gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 1, "Synthetic delta")))
        assertTrue(transport.gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 1, "{\"status\":\"complete\"}")))
        assertEquals(listOf("1", "2"), packets.map { it.getString("revision") })
        assertEquals("delta", packets.first().getJSONObject("update").getString("kind"))
        assertEquals("status", packets.last().getJSONObject("update").getString("kind"))
    }

    @Test fun sinkFailureAndCancellationCannotBeAcknowledgedIntoCommit() {
        for (cancel in listOf(false, true)) {
            lateinit var transport: PhoneRequestDelivery
            val rejected = AtomicInteger(0)
            transport = PhoneRequestDelivery("synthetic-client", { packet ->
                if (cancel) transport.cancel()
                else transport.acknowledge(packet.getString("providerRequestId"), packet.getString("revision"), false)
            }, { rejected.incrementAndGet() })
            assertFalse(transport.gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 1, "Synthetic delta")))
            assertFalse(transport.acknowledge("synthetic-provider", "1", true))
            assertFalse(transport.gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 1, "{}")))
        }
    }

    @Test fun missingAcknowledgementAndWrongProviderFailBoundedly() {
        val transport = PhoneRequestDelivery("synthetic-client", {}, {}, timeoutMillis = 10)
        assertTrue(transport.gate.deliver(AssistantDelta("synthetic-client", "synthetic-provider", 1, "Synthetic delta")))
        assertFalse(transport.acknowledge("other-synthetic-provider", "1", true))
        assertFalse(transport.gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 1, "{}")))
        val missing = PhoneRequestDelivery("synthetic-client", {}, {}, timeoutMillis = 10)
        assertFalse(missing.gate.awaitReady(AssistantDeliveryBoundary("synthetic-client", "synthetic-provider", 0, "{}")))
    }
}
