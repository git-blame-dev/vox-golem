package dev.gitblame.voxgolem.assistant.runtime

import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.DataInputStream
import java.io.DataOutputStream

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class PhoneRelayWireTest {
    private fun rejected(block: () -> Unit) {
        try { block() } catch (_: Exception) { return }
        fail("Expected rejection")
    }

    @Test fun roundTripsLargeAndEscapedJson() {
        for (text in listOf("a".repeat(100_000), "🧪".repeat(100_000), "\"\\\n".repeat(50_000))) {
            val encoded = PhoneRelayWire.encode(JSONObject().put("text", text))
            val stream = ByteArrayOutputStream()
            PhoneRelayWire.write(DataOutputStream(stream), encoded)
            assertEquals(text, PhoneRelayWire.read(DataInputStream(ByteArrayInputStream(stream.toByteArray()))).getString("text"))
        }
        rejected { PhoneRelayWire.encode(JSONObject().put("text", "🧪".repeat(600_000))) }
    }

    @Test fun rejectsLengthsBeforeReadingPayloadAndMalformedUtf8() {
        for (length in listOf(-1, 0, PhoneRelayWire.MAX_FRAME_BYTES + 1)) {
            val bytes = ByteArrayOutputStream().also { DataOutputStream(it).writeInt(length) }.toByteArray()
            rejected { PhoneRelayWire.read(DataInputStream(ByteArrayInputStream(bytes))) }
        }
        val bytes = ByteArrayOutputStream().also {
            DataOutputStream(it).apply { writeInt(2); write(byteArrayOf(0xc3.toByte(), 0x28)) }
        }.toByteArray()
        rejected { PhoneRelayWire.read(DataInputStream(ByteArrayInputStream(bytes))) }
        rejected { PhoneRelayWire.write(DataOutputStream(ByteArrayOutputStream()), byteArrayOf()) }
    }

    @Test fun pathsAreCanonicalFreshUuidPaths() {
        val valid = PhoneRelayWire.PREFIX + "12345678-1234-4234-8234-123456789abc"
        assertTrue(PhoneRelayWire.validPath(valid))
        for (path in listOf(valid + "/extra", valid.uppercase(), valid.replace("4234", "1234"), PhoneRelayWire.PREFIX + "request")) {
            assertFalse(PhoneRelayWire.validPath(path))
        }
    }

    private fun event(seq: Any, type: String) = JSONObject().put("requestId", "synthetic-wire").put("seq", seq).put("type", type)

    @Test fun sequenceAndTypesAreStrict() {
        val events = PhoneRelayEvents("synthetic-wire")
        for (seq in listOf<Any>(0, -1, 2, "1", 1.0)) rejected { events.accept(event(seq, "dispatched")) }
        rejected { events.accept(event(1, "ready")) }
        rejected { events.accept(event(1, "dispatched").put("requestId", "other")) }
        events.accept(event(1, "dispatched"))
        rejected { events.accept(event(1, "dispatched")) }
        rejected { events.accept(event(2, "status").put("status", "arbitrary")) }
        events.accept(event(2, "status").put("status", "Searching · 2 results"))
        rejected { events.accept(event(3, "first_text").put("milliseconds", "4")) }
        rejected { events.accept(event(3, "first_text").put("milliseconds", 86_400_001)) }
        events.accept(event(3, "first_text").put("milliseconds", 0))
    }

    @Test fun callbackBudgetAndTerminalOrdering() {
        val events = PhoneRelayEvents("synthetic-wire")
        events.accept(event(1, "delta").put("text", "🧪".repeat(AssistantRequestWorker.MAX_CALLBACK_BYTES / 4)))
        rejected { events.accept(event(2, "delta").put("text", "x")) }
        events.accept(event(2, "outcome").put("outcomeJson", "{\"status\":\"complete\"}"))
        rejected { events.accept(event(3, "dispatched")) }
        rejected { events.accept(event(3, "outcome").put("outcomeJson", "{}")) }
        events.accept(event(3, "retired"))
        rejected { events.accept(event(4, "retired")) }
        rejected { PhoneRelayEvents("synthetic-wire").accept(event(1, "retired")) }
    }

    @Test fun requestsAreBoundedRestrictedAndRewritten() {
        fun request(model: String = "gemini-nano") = JSONObject().put("requestId", "synthetic-wire").put("model", model)
        for (model in AssistantModels.aliases.filter { it == "gemini-nano" || it.startsWith("agy-") }) {
            assertEquals("synthetic-local", JSONObject(PhoneRelayWire.prepareRequest("synthetic-wire", request(model).toString(), "synthetic-local")).getString("requestId"))
        }
        for (model in listOf("custom-sol-high", "local-fast", "agy-unknown")) {
            rejected { PhoneRelayWire.prepareRequest("synthetic-wire", request(model).toString(), "synthetic-local") }
        }
        rejected { PhoneRelayWire.prepareRequest("other", request().toString(), "synthetic-local") }
        rejected { PhoneRelayWire.prepareRequest("synthetic-wire", request().put("prompt", "🧪".repeat(70_000)).toString(), "synthetic-local") }
        rejected { PhoneRelayWire.prepareRequest("", request().toString(), "synthetic-local") }
    }

    @Test fun outcomesPreserveSuccessAndRedactErrors() {
        val success = JSONObject().put("status", "complete").put("text", "Synthetic answer").put("speech", "Synthetic speech").put("sources", listOf("synthetic-source")).toString()
        assertEquals(success, PhoneRelayWire.safeOutcome(success))
        val safe = JSONObject(PhoneRelayWire.safeOutcome("{\"status\":\"error\",\"code\":\"private-code\",\"message\":\"Synthetic sensitive detail\",\"extra\":42}"))
        assertEquals("relay_error", safe.getString("code"))
        assertFalse(safe.toString().contains("sensitive"))
        assertFalse(safe.has("extra"))
        assertEquals("timeout", JSONObject(PhoneRelayWire.safeOutcome("{\"status\":\"error\",\"code\":\"timeout\"}")).getString("code"))
        rejected { PhoneRelayWire.safeOutcome("{\"status\":\"unknown\"}") }
        rejected { PhoneRelayWire.safeOutcome(JSONObject().put("status", "complete").put("text", "a".repeat(PhoneRelayWire.MAX_FRAME_BYTES)).toString()) }
    }
}
