package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test

class PhoneSnapshotEncoderTest {
    private fun id(sequence: String) = JSONObject().put("replica", "11111111-1111-4111-8111-111111111111").put("sequence", sequence)
    private fun record(sequence: String, request: String, answer: String) = JSONObject().put("operation", JSONObject()
        .put("id", id(sequence)).put("body", JSONObject().put("kind", "turn_completed").put("conversation", id("1"))
            .put("request", id(request)).put("prompt", "Synthetic question").put("answer", JSONObject().put("kind", "text").put("text", answer)).put("terminal_stage", "instant")))

    @Test fun boundedPickerIncludesSelectedConversationAndDoesNotSkipReplacedPageEntry() {
        val conversations = JSONArray()
        for (sequence in 1..129) conversations.put(JSONObject().put("id", id(sequence.toString())).put("title", "Synthetic conversation $sequence").put("deleted", false))
        val view = JSONObject().put("revision", "1").put("records", JSONArray()).put("conversations", conversations)
        val encoded = PhoneSnapshotEncoder.encode(AssistantState(viewJson = view.toString(), conversation = id("129").toString()), null, PhonePreferences.defaults(), emptyMap())
        val projection = encoded.getJSONObject("assistant")
        assertEquals(128, projection.getJSONArray("conversations").length())
        assertEquals(129, projection.getInt("conversation_count"))
        assertEquals(127, projection.getInt("conversation_offset"))
        assertEquals(PhoneSnapshotEncoder.wireId(id("129")), projection.getJSONArray("conversations").getJSONObject(127).getString("id"))
    }

    @Test fun projectsOnlyRustSelectedPathNotAllGraphRecords() {
        val selected = record("3", "2", "Selected synthetic answer")
        val sibling = record("5", "4", "Sibling synthetic answer")
        val view = JSONObject().put("revision", "7").put("records", JSONArray().put(selected).put(sibling))
            .put("conversations", JSONArray().put(JSONObject().put("id", id("1")).put("title", "Synthetic conversation").put("deleted", false)))
        val state = AssistantState(viewJson = view.toString(), conversation = id("1").toString(), selectedHead = id("3").toString(), draft = "Synthetic draft")
        val projection = PhoneSnapshotEncoder.encode(state, JSONObject().put("turns", JSONArray().put(selected)), PhonePreferences.defaults(),
            mapOf(PhoneSnapshotEncoder.wireId(id("2")) to "synthetic-client-request"))
        assertEquals(1, projection.getJSONArray("transcript").length())
        assertFalse(projection.toString().contains("Sibling synthetic answer"))
        assertEquals("synthetic-client-request", projection.getJSONArray("transcript").getJSONObject(0).getString("request_id"))
        assertEquals("Synthetic draft", projection.getJSONObject("assistant").getString("draft"))
    }

    @Test fun refusalUsesReasonAndPreservesDistributedIdentitiesWithoutNumberCoercion() {
        val record = record("18446744073709551615", "2", "unused")
        record.getJSONObject("operation").getJSONObject("body").put("answer", JSONObject().put("kind", "refusal").put("reason", "Synthetic refusal"))
        val turn = PhoneSnapshotEncoder.turn(record, emptyMap())
        assertEquals("11111111-1111-4111-8111-111111111111:18446744073709551615", turn.getJSONObject("answer").getString("id"))
        assertEquals("Synthetic refusal", turn.getJSONObject("answer").getJSONObject("content").getString("text"))
    }
}
