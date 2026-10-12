package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.runtime.AcknowledgedAssistantDelivery
import dev.gitblame.voxgolem.assistant.runtime.AssistantDeliveryGate
import org.json.JSONObject

/** Transport-only projection/ACK adapter to the shared engine's delivery gate. */
class PhoneRequestDelivery(val clientRequestId: String, update: (JSONObject) -> Unit,
    private val rejected: () -> Unit, timeoutMillis: Long = 5_000,
) {
    private val acknowledged = AcknowledgedAssistantDelivery(clientRequestId, { packet ->
        val value = if (packet.delta != null) JSONObject().put("kind", "delta").put("text", packet.delta)
            else JSONObject().put("kind", "status").put("message", packet.toolStatus ?: "Saving response…")
        update(JSONObject().put("requestId", packet.clientRequestToken).put("providerRequestId", packet.providerRequestId)
            .put("revision", packet.sequence.toString()).put("update", value))
    }, timeoutMillis = timeoutMillis)
    val gate: AssistantDeliveryGate get() = acknowledged

    fun acknowledge(providerRequestId: String, revision: String, accepted: Boolean): Boolean {
        val sequence = revision.toLongOrNull()
        if (!accepted || sequence == null || sequence <= 0 || sequence.toString() != revision) {
            acknowledged.cancel()
            rejected()
            return false
        }
        val result = acknowledged.acknowledge(clientRequestId, providerRequestId, sequence)
        if (!result) { acknowledged.cancel(); rejected() }
        return result
    }

    fun cancel() = acknowledged.cancel()
}
