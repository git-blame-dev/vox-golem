package dev.gitblame.voxgolem.assistant.runtime

data class AssistantDelta(val clientRequestToken: String, val providerRequestId: String, val sequence: Long, val text: String)
data class AssistantToolStatus(val clientRequestToken: String, val providerRequestId: String, val sequence: Long, val status: String)
data class AssistantDeliveryBoundary(val clientRequestToken: String, val providerRequestId: String, val lastDeltaSequence: Long, val outcomeJson: String)
data class AssistantDeliveryPacket(
    val clientRequestToken: String, val providerRequestId: String, val sequence: Long,
    val delta: String? = null, val candidate: String? = null,
    val toolStatus: String? = null,
)

/** Called on the provider thread. Ready means contiguous ACKs plus the final candidate, not send(). */
interface AssistantDeliveryGate {
    fun deliver(delta: AssistantDelta): Boolean
    fun deliverStatus(status: AssistantToolStatus): Boolean = true
    fun awaitReady(boundary: AssistantDeliveryBoundary): Boolean
    fun cancel()
}

data class AssistantRequestResult(
    val clientRequestToken: String,
    val providerRequestId: String? = null,
    val status: String,
    val code: String? = null,
    val revision: String? = null,
    val recordJson: String? = null,
    val speechText: String? = null,
)

fun interface AssistantResultListener {
    fun onResult(result: AssistantRequestResult, snapshot: AssistantState)
}

/** A bounded per-request ACK ledger. The transport must enqueue promptly and ACK only consumed packets. */
class AcknowledgedAssistantDelivery(
    private val clientRequestToken: String,
    private val enqueue: (AssistantDeliveryPacket) -> Unit,
    private val maxInFlight: Int = 64,
    private val timeoutMillis: Long = 5000,
) : AssistantDeliveryGate {
    private val lock = Object()
    private var providerId: String? = null
    private var sent = 0L
    private var contiguous = 0L
    private val received = mutableSetOf<Long>()
    private var failed = false
    private var terminal = false
    private var statuses = 0

    init { require(maxInFlight in 1..128 && timeoutMillis in 1..15000) }

    override fun deliver(delta: AssistantDelta): Boolean {
        if (delta.text.toByteArray(Charsets.UTF_8).size > 256 * 1024) { cancel(); return false }
        return publish(AssistantDeliveryPacket(delta.clientRequestToken, delta.providerRequestId, delta.sequence, delta = delta.text), false)
    }

    override fun deliverStatus(status: AssistantToolStatus): Boolean {
        if (status.status !in AssistantSearchModes.statuses) { cancel(); return false }
        if (synchronized(lock) { ++statuses > AssistantSearchModes.MAX_STATUS_EVENTS }) { cancel(); return false }
        return publish(AssistantDeliveryPacket(status.clientRequestToken, status.providerRequestId, status.sequence, toolStatus = status.status), false)
    }

    override fun awaitReady(boundary: AssistantDeliveryBoundary): Boolean {
        if (boundary.outcomeJson.toByteArray(Charsets.UTF_8).size > 2 * 1024 * 1024 || boundary.lastDeltaSequence !in 0..262144 + AssistantSearchModes.MAX_STATUS_EVENTS) {
            cancel(); return false
        }
        val target = boundary.lastDeltaSequence + 1
        if (!publish(AssistantDeliveryPacket(boundary.clientRequestToken, boundary.providerRequestId, target, candidate = boundary.outcomeJson), true)) return false
        return synchronized(lock) {
            waitUntil { contiguous == target }
        }
    }

    private fun publish(packet: AssistantDeliveryPacket, candidate: Boolean): Boolean {
        synchronized(lock) {
            if (failed || terminal || packet.clientRequestToken != clientRequestToken || packet.sequence != sent + 1 || packet.sequence > 262145 + AssistantSearchModes.MAX_STATUS_EVENTS ||
                (providerId != null && providerId != packet.providerRequestId)) { failed = true; lock.notifyAll(); return false }
            providerId = packet.providerRequestId
            if (!waitUntil { sent - contiguous < maxInFlight }) return false
            sent = packet.sequence
            terminal = candidate
        }
        try { enqueue(packet) } catch (_: RuntimeException) { cancel(); return false }
        return synchronized(lock) { !failed }
    }

    fun acknowledge(clientToken: String, providerRequestId: String, sequence: Long): Boolean = synchronized(lock) {
        if (failed || clientToken != clientRequestToken || providerRequestId != providerId || sequence !in 1..sent) return@synchronized false
        if (sequence > contiguous) received += sequence
        while (received.remove(contiguous + 1)) contiguous++
        lock.notifyAll()
        true
    }

    fun contiguousAcknowledgement(): Long = synchronized(lock) { contiguous }

    override fun cancel() { synchronized(lock) { failed = true; lock.notifyAll() } }

    private fun waitUntil(condition: () -> Boolean): Boolean {
        val deadline = System.nanoTime() + timeoutMillis * 1_000_000
        while (!failed && !condition()) {
            val remaining = deadline - System.nanoTime()
            if (remaining <= 0) { failed = true; break }
            try { lock.wait((remaining / 1_000_000).coerceAtLeast(1)) } catch (_: InterruptedException) {
                Thread.currentThread().interrupt(); failed = true
            }
        }
        return !failed
    }
}
