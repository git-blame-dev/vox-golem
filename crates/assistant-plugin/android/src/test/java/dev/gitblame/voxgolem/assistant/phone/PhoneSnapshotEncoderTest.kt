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
            .put("request", id(request)).put("prompt", "Synthetic question").put("answer", JSONObject().put("kind", "text").put("text", answer)).put("sources", JSONArray()).put("terminal_stage", "instant")))

    @Test fun canonicalObservedSourcesSurviveColdSnapshotProjectionWithoutTextFooterParsing() {
        val record = record("3", "2", "Synthetic answer without a source footer")
        val sources = JSONArray().put(JSONObject().put("url", "https://example.test/document").put("title", "Synthetic observed document"))
        record.getJSONObject("operation").getJSONObject("body").put("sources", sources)
        val turn = PhoneSnapshotEncoder.turn(record, emptyMap())
        assertEquals(sources.toString(), turn.getJSONArray("sources").toString())
        assertEquals("Synthetic answer without a source footer", turn.getJSONObject("answer").getJSONObject("content").getString("text"))
    }

    @Test fun optionalModelsRequirePublishedReadinessAndEnforceWebPolicyInsteadOfPrefixAvailability() {
        val view = JSONObject().put("revision", "0").put("conversations", JSONArray())
        val state = AssistantState(viewJson = view.toString())
        fun options(readiness: Map<String, dev.gitblame.voxgolem.assistant.runtime.OptionalProviderReadiness>, web: Boolean = true): List<JSONObject> {
            val array = PhoneSnapshotEncoder.encode(state.copy(webEnabled = web), null, PhonePreferences.defaults(), emptyMap(),
                optionalProviders = readiness).getJSONArray("model_options")
            return (0 until array.length()).map(array::getJSONObject)
        }
        assertTrue(options(emptyMap()).filter { it.getString("id") in dev.gitblame.voxgolem.assistant.runtime.AssistantModels.optionalLabels }
            .all { !it.getBoolean("available") && !it.isNull("unavailable_reason") })
        val ready = mapOf("agy-flash-high" to dev.gitblame.voxgolem.assistant.runtime.OptionalProviderReadiness(true, "Synthetic installed route"))
        assertTrue(options(ready).single { it.getString("id") == "agy-flash-high" }.getBoolean("available"))
        assertFalse(options(ready, false).single { it.getString("id") == "agy-flash-high" }.getBoolean("available"))
        assertFalse(options(ready).any { it.getString("id") == "local-fast" })
    }

    @Test fun removedDesktopModelIsNotReintroducedByStaleReadinessOrSavedPreferences() {
        val state = AssistantState(model = "local-fast", draft = "Synthetic retained draft", error = "unsupported_model")
        val preferences = PhonePreferences.defaults().put("instant", state.model)
        val readiness = mapOf("local-fast" to dev.gitblame.voxgolem.assistant.runtime.OptionalProviderReadiness(true, "Synthetic stale route"))
        val encoded = PhoneSnapshotEncoder.encode(state, null, preferences, emptyMap(), optionalProviders = readiness)
        val options = encoded.getJSONArray("model_options")
        assertFalse((0 until options.length()).any { options.getJSONObject(it).getString("id") == "local-fast" })
        val capabilities = encoded.getJSONArray("capabilities")
        val local = (0 until capabilities.length()).map(capabilities::getJSONObject).single { it.getString("id") == "local_fast" }
        assertEquals("unavailable", local.getString("state"))
        assertTrue(local.isNull("actual_provider"))
        assertEquals("local-fast", encoded.getJSONObject("preferences").getString("instant"))
        assertEquals("Synthetic retained draft", encoded.getJSONObject("assistant").getString("draft"))
        assertEquals("unsupported_model", encoded.getJSONObject("assistant").getString("error"))
        assertEquals("sleeping", encoded.getString("runtime_phase"))
        assertTrue(encoded.isNull("active_request"))
    }

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

        // CompactView shape from Rust (already bounded to 128 items, no records array, explicit counts and offsets)
        val compactConversations = JSONArray()
        for (sequence in 1..127) compactConversations.put(JSONObject().put("id", id(sequence.toString())).put("title", "Synthetic conversation $sequence").put("deleted", false))
        compactConversations.put(JSONObject().put("id", id("129")).put("title", "Synthetic conversation 129").put("deleted", false))
        val compactView = JSONObject()
            .put("revision", "2")
            .put("conversations", compactConversations)
            .put("conversation_count", 129)
            .put("conversation_offset", 127)
            .put("branch_count", 129)
            .put("branch_offset", 128)
            .put("branch_previews", JSONArray())
            .put("pending", JSONArray())
            .put("quarantine_count", 0)
        val compactState = AssistantState(
            viewJson = compactView.toString(),
            conversation = id("129").toString(),
            branchChoices = (1..128).map { id(it.toString()).toString() }
        )
        val compactProjection = PhoneSnapshotEncoder.encode(compactState, null, PhonePreferences.defaults(), emptyMap()).getJSONObject("assistant")
        assertEquals(128, compactProjection.getJSONArray("conversations").length())
        assertEquals(129, compactProjection.getInt("conversation_count"))
        assertEquals(127, compactProjection.getInt("conversation_offset"))
        assertEquals(129, compactProjection.getInt("branch_count"))
        assertEquals(128, compactProjection.getInt("branch_offset"))
        assertEquals(PhoneSnapshotEncoder.wireId(id("129")), compactProjection.getJSONArray("conversations").getJSONObject(127).getString("id"))
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
