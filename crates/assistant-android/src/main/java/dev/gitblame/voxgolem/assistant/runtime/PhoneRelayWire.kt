package dev.gitblame.voxgolem.assistant.runtime

import org.json.JSONObject
import java.io.DataInputStream
import java.io.DataOutputStream
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.nio.charset.StandardCharsets.UTF_8

/** Bounded, length-prefixed JSON shared by the phone and Wear transport adapters. */
object PhoneRelayWire {
    const val PREFIX = "/voxgolem/assistant/v2/phone-relay/"
    const val MAX_FRAME_BYTES = 2 * 1024 * 1024
    const val MAX_REQUEST_BYTES = 256 * 1024
    private val path = Regex("^${PREFIX}[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
    private val safeCodes = setOf("relay_error", "transport", "timeout", "invalid_response", "unavailable", "unsupported_model", "not_ready", "busy", "output_limit",
        "internal_error", "retirement_failed", "unsupported_web_permission", "auth_unavailable", "provider_failure",
        "aicore_not_available", "aicore_needs_update", "aicore_busy", "aicore_unavailable", "too_large")

    fun validPath(path: String): Boolean = this.path.matches(path)

    fun encode(json: JSONObject): ByteArray = json.toString().toByteArray(UTF_8).also {
        require(it.size in 1..MAX_FRAME_BYTES) { "Invalid relay frame size" }
    }

    fun read(input: DataInputStream): JSONObject {
        val length = input.readInt()
        require(length in 1..MAX_FRAME_BYTES) { "Invalid relay frame size" }
        val bytes = ByteArray(length)
        input.readFully(bytes)
        val text = UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString()
        return JSONObject(text)
    }

    fun write(output: DataOutputStream, bytes: ByteArray) {
        require(bytes.size in 1..MAX_FRAME_BYTES) { "Invalid relay frame size" }
        output.writeInt(bytes.size)
        output.write(bytes)
        output.flush()
    }

    fun prepareRequest(wireId: String, requestJson: String, localId: String): String {
        require(wireId.isNotEmpty() && wireId.length <= 128) { "Invalid relay request ID" }
        require(localId.isNotEmpty() && localId.length <= 128) { "Invalid execution ID" }
        require(requestJson.toByteArray(UTF_8).size in 1..MAX_REQUEST_BYTES) { "Relay request too large" }
        val request = JSONObject(requestJson)
        require(request.get("requestId") == wireId) { "Relay request ID mismatch" }
        val model = request.get("model")
        require(model is String && model in AssistantModels.aliases &&
            (model == "gemini-nano" || model.startsWith("agy-"))) { "Unsupported relay model" }
        // Older Wear peers may include this field; it is not a provider control anymore.
        request.remove("searchMode")
        return request.put("requestId", localId).toString().also {
            require(it.toByteArray(UTF_8).size <= MAX_REQUEST_BYTES) { "Relay request too large" }
        }
    }

    fun safeOutcome(raw: String): String {
        require(raw.toByteArray(UTF_8).size in 1..MAX_FRAME_BYTES) { "Relay outcome too large" }
        val outcome = JSONObject(raw)
        // Older phones used a mode-named code for Agy's enforced Web-off capability failure.
        val code = (outcome.opt("code") as? String)?.let {
            if (it == "unsupported_search_mode") "unsupported_web_permission" else it
        }
        return when (outcome.get("status")) {
            "complete" -> raw
            "cancelled" -> JSONObject().put("status", "cancelled").toString()
            "error" -> JSONObject().put("status", "error")
                .put("code", code?.takeIf { it in safeCodes } ?: "relay_error")
                .put("message", "Phone relay request failed").toString()
            else -> throw IllegalArgumentException("Invalid relay outcome status")
        }
    }

    internal fun encodeOutcome(requestId: String, sequence: Long, raw: String): ByteArray {
        val envelope = JSONObject().put("type", "outcome").put("requestId", requestId).put("seq", sequence)
        if (raw.toByteArray(UTF_8).size > MAX_FRAME_BYTES) {
            return encode(envelope.put("outcomeJson", TOO_LARGE_OUTCOME))
        }
        envelope.put("outcomeJson", safeOutcome(raw))
        return try { encode(envelope) } catch (_: IllegalArgumentException) {
            // The nested JSON string can exceed the frame budget even when the raw outcome fits.
            encode(envelope.put("outcomeJson", TOO_LARGE_OUTCOME))
        }
    }

    private const val TOO_LARGE_OUTCOME = """{"status":"error","code":"too_large","message":"Phone relay result exceeds transport limit"}"""
}

/** Single-reader validation after bounded frame decoding. Failed events do not advance the sequence. */
internal class PhoneRelayEvents(private val wireRequestId: String) {
    private var next = 1L
    private var callbackBytes = 0L
    private var statuses = 0
    private var outcome = false
    private var retired = false
    private var dispatched = false
    private var firstText = false

    fun accept(event: JSONObject): JSONObject {
        require(!retired) { "Relay already retired" }
        require(event.get("requestId") == wireRequestId) { "Relay event ID mismatch" }
        require(integer(event, "seq") == next) { "Invalid relay sequence" }
        val type = event.get("type")
        var bytes = 0
        var safe: String? = null
        when (type) {
            "dispatched" -> require(!outcome && !dispatched)
            "status" -> {
                require(!outcome && statuses < AssistantToolStatuses.MAX_STATUS_EVENTS)
                val status = event.get("status")
                require(status is String && AssistantToolStatuses.isValidStatus(status))
            }
            "first_text" -> {
                require(!outcome && !firstText)
                require(integer(event, "milliseconds") in 0L..86_400_000L)
            }
            "delta" -> {
                require(!outcome)
                val text = event.get("text")
                require(text is String && text.isNotEmpty())
                bytes = text.toByteArray(UTF_8).size
            }
            "outcome" -> {
                require(!outcome)
                val raw = event.get("outcomeJson")
                require(raw is String)
                safe = PhoneRelayWire.safeOutcome(raw)
            }
            // The phone synthesizes a cancelled outcome when the worker suppresses it.
            "retired" -> require(outcome)
            else -> throw IllegalArgumentException("Invalid relay event type")
        }
        require(callbackBytes + bytes <= AssistantRequestWorker.MAX_CALLBACK_BYTES)
        callbackBytes += bytes
        if (type == "status") statuses++
        if (type == "dispatched") dispatched = true
        if (type == "first_text") firstText = true
        if (type == "outcome") {
            event.put("outcomeJson", safe)
            outcome = true
        }
        if (type == "retired") retired = true
        next++
        return event
    }

    private fun integer(event: JSONObject, name: String): Long {
        val value = event.get(name)
        require(value is Int || value is Long) { "Invalid relay integer" }
        return (value as Number).toLong()
    }
}
