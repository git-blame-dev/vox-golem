package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AssistantHistoryTest {
    private val conversation = id("synthetic-chat", "1")
    private val head = id("synthetic-turn", "30")
    private val state = AssistantState(conversation = conversation, selectedHead = head,
        viewJson = "{\"revision\":\"8\",\"conversations\":[],\"records\":[]}")
    private class FakeHistory {
        val calls = mutableListOf<Pair<String?, (String?, AssistantState) -> Unit>>()
        fun request(before: String?, settled: (String?, AssistantState) -> Unit) { calls += before to settled }
    }

    @Test fun completedHistoryProjectsExistingRequestIdentityWithoutChangingTurnIdentity() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        val raw = JSONObject(page(state, listOf("30"), null))
        val body = raw.getJSONArray("turns").getJSONObject(0).getJSONObject("operation").getJSONObject("body")
        body.put("request", JSONObject().put("sequence", "7").put("replica", "synthetic-request"))
        val original = raw.toString()
        fake.calls[0].second(original, state)
        val turn = history.snapshot.turns.single()
        assertNull(history.snapshot.error)
        assertEquals(head, turn.id)
        assertEquals("synthetic-request:7", turn.requestKey)
        assertEquals("Synthetic question 30", turn.prompt)
        assertEquals("Synthetic answer 30", turn.answer)
    }

    @Test fun distinctNativeTurnIdsWithTheSameRequestRemainSeparateAndInNativeOrder() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        val raw = JSONObject(page(state, listOf("29", "30"), null))
        val turns = raw.getJSONArray("turns")
        for (index in 0 until turns.length()) turns.getJSONObject(index).getJSONObject("operation").getJSONObject("body")
            .put("request", JSONObject(id("synthetic-import-request", "7")))
        fake.calls[0].second(raw.toString(), state)
        assertNull(history.snapshot.error)
        assertEquals(listOf("29", "30"), history.snapshot.turns.map { JSONObject(it.id).getString("sequence") })
        assertEquals(listOf("synthetic-import-request:7", "synthetic-import-request:7"), history.snapshot.turns.map { it.requestKey })
        assertEquals(4, AssistantTurnItemKeys(history.snapshot.turns).rowKeys.size)
    }

    @Test fun missingNullOrMalformedOptionalRequestDoesNotHideLegacyHistoryOrInventAKey() {
        val variants = listOf<Any?>(null, JSONObject.NULL, "synthetic-invalid", JSONObject(),
            JSONObject().put("replica", "synthetic-request"),
            JSONObject().put("replica", 12).put("sequence", "7"),
            JSONObject().put("replica", "synthetic-request").put("sequence", ""))
        variants.forEach { request ->
            val fake = FakeHistory()
            val history = AssistantHistory(fake::request) {}
            history.select(state)
            val raw = JSONObject(page(state, listOf("30"), null))
            if (request != null) raw.getJSONArray("turns").getJSONObject(0).getJSONObject("operation")
                .getJSONObject("body").put("request", request)
            fake.calls[0].second(raw.toString(), state)
            assertNull(history.snapshot.error)
            assertNull(history.snapshot.turns.single().requestKey)
        }
    }

    @Test fun requestProjectionDoesNotChangeOverlapMergeEligibilityOrNativeOrder() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        val raw = JSONObject(page(state, listOf("30"), "synthetic-older"))
        raw.getJSONArray("turns").getJSONObject(0).getJSONObject("operation").getJSONObject("body")
            .put("request", JSONObject(id("synthetic-request", "7")))
        fake.calls[0].second(raw.toString(), state)
        history.older()
        fake.calls[1].second(page(state, listOf("29", "30"), null), state)
        assertNull(history.snapshot.error)
        assertEquals(listOf("29", "30"), history.snapshot.turns.map { JSONObject(it.id).getString("sequence") })
        assertNull(history.snapshot.turns.first().requestKey)
        assertEquals("synthetic-request:7", history.snapshot.turns.last().requestKey)
    }

    @Test fun nativeOrderAndOpaqueCursorArePreservedWithoutSortingOrSiblingProjection() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        fake.calls[0].second(page(state, listOf("9", "10", "30"), "opaque-older"), state)
        assertEquals(listOf("9", "10", "30"), history.snapshot.turns.map { JSONObject(it.id).getString("sequence") })
        history.older(); history.older()
        assertEquals(2, fake.calls.size)
        assertEquals("opaque-older", fake.calls[1].first)
        fake.calls[1].second(page(state, listOf("1", "2"), null), state)
        assertEquals(listOf("1", "2", "9", "10", "30"), history.snapshot.turns.map { JSONObject(it.id).getString("sequence") })
        assertNull(history.snapshot.olderBefore)
    }

    @Test fun staleConversationOrHeadCallbackCannotReplaceTheNewSelectionAndReadsNeverOverlap() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        val next = state.copy(conversation = id("synthetic-other-chat", "1"), selectedHead = id("synthetic-other-turn", "2"))
        history.select(next)
        assertEquals(1, fake.calls.size)
        fake.calls[0].second(page(state, listOf("30"), null), state)
        assertTrue(history.snapshot.turns.isEmpty())
        assertEquals(2, fake.calls.size)
        fake.calls[1].second(page(next, listOf("2"), null), next)
        assertEquals(1, history.snapshot.turns.size)
        assertEquals(next.conversation, history.snapshot.conversation)
    }

    @Test fun pauseRetiresCallbacksAndResumeWaitsForOutstandingReadBeforeReloading() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        history.suspendReads()
        history.select(state)
        assertEquals(1, fake.calls.size)
        fake.calls[0].second(page(state, listOf("30"), null), state)
        assertEquals(2, fake.calls.size)
        assertTrue(history.snapshot.turns.isEmpty())
    }

    @Test fun wrongPageHeadDuplicateRecordsAndCursorCyclesAreErrorsNotMixedHistory() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        fake.calls[0].second(page(state.copy(selectedHead = id("synthetic-turn", "99")), listOf("99"), null), state)
        assertNotNull(history.snapshot.error)
        assertTrue(history.snapshot.turns.isEmpty())
        history.retry()
        fake.calls[1].second(page(state, listOf("30", "30"), null), state)
        assertNotNull(history.snapshot.error)
        history.retry()
        fake.calls[2].second(page(state, listOf("30"), "opaque-cycle"), state)
        history.older()
        fake.calls[3].second(page(state, listOf("29"), "opaque-cycle"), state)
        assertNotNull(history.snapshot.error)
        assertEquals(1, history.snapshot.turns.size)
    }

    @Test fun fastForwardRetainsLoadedOlderMessagesButForkDoesNotReuseThem() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        fake.calls[0].second(page(state, listOf("1", "2", "30"), "opaque-first"), state)
        val advanced = state.copy(selectedHead = id("synthetic-turn", "31"))
        history.select(advanced)
        fake.calls[1].second(page(advanced, listOf("30", "31"), "opaque-new"), advanced)
        assertEquals(listOf("1", "2", "30", "31"), history.snapshot.turns.map { JSONObject(it.id).getString("sequence") })
        // Rebase pagination to the new head; never send the old branch's cursor.
        assertEquals("opaque-new", history.snapshot.olderBefore)
        val fork = advanced.copy(selectedHead = id("synthetic-turn", "40"))
        history.select(fork)
        fake.calls[2].second(page(fork, listOf("2", "40"), null), fork)
        assertEquals(listOf("2", "40"), history.snapshot.turns.map { JSONObject(it.id).getString("sequence") })
    }

    @Test fun unselectedAndUnresolvedBranchesNeverRequestOrDisplayFlattenedRecords() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state.copy(conversation = null))
        history.select(state.copy(branchChoices = listOf(head)))
        assertTrue(fake.calls.isEmpty())
        assertTrue(history.snapshot.turns.isEmpty())
    }

    @Test fun moreThanTwentyTurnsRemainReadableAcrossOverlappingRefreshPages() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        fake.calls[0].second(page(state, (11..30).map(Int::toString), "synthetic-older"), state)
        history.older()
        fake.calls[1].second(page(state, (1..10).map(Int::toString), null), state)
        assertEquals(30, history.snapshot.turns.size)
        val advanced = state.copy(selectedHead = id("synthetic-turn", "31"))
        history.select(advanced)
        fake.calls[2].second(page(advanced, (12..31).map(Int::toString), "synthetic-refreshed"), advanced)
        history.older()
        fake.calls[3].second(page(advanced, (1..11).map(Int::toString), null), advanced)
        assertEquals((1..31).map(Int::toString), history.snapshot.turns.map { JSONObject(it.id).getString("sequence") })
    }

    @Test fun duplicateAndRevisionStaleSettlementsCannotOverwriteAcceptedHistory() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state)
        fake.calls[0].second(page(state, listOf("30"), null), state)
        fake.calls[0].second(null, state)
        assertNull(history.snapshot.error)
        assertEquals(1, history.snapshot.turns.size)
        history.retry()
        fake.calls[1].second(page(state, listOf("30"), null), state.copy(viewJson = state.viewJson.replace("\"8\"", "\"9\"")))
        assertNotNull(history.snapshot.error)
        assertEquals(1, history.snapshot.turns.size)
    }

    @Test fun retryWhileUnselectedCannotLeaveHistoryWaitingForAPageThatWillNeverBeRequested() {
        val fake = FakeHistory()
        val history = AssistantHistory(fake::request) {}
        history.select(state.copy(conversation = null, selectedHead = null))
        history.retry()
        assertFalse(history.snapshot.loading)
        assertTrue(fake.calls.isEmpty())
        history.select(state)
        assertEquals(1, fake.calls.size)
    }

    @Test fun configurationRestorationKeepsLoadedPagesButRevalidatesTheNativeSelectionAndCursor() {
        val fake = FakeHistory()
        val initial = AssistantHistoryState(conversation, head,
            (1..30).map { AssistantTurn(id("synthetic-turn", it.toString()), "Synthetic question $it", "Synthetic answer $it", false) },
            "synthetic-retired-cursor")
        val history = AssistantHistory(fake::request, initial) {}
        history.select(state)
        assertEquals(30, history.snapshot.turns.size)
        history.older()
        assertEquals(1, fake.calls.size)
        assertNull(fake.calls[0].first)
        fake.calls[0].second(page(state, (11..30).map(Int::toString), "synthetic-fresh-cursor"), state)
        assertEquals(30, history.snapshot.turns.size)
        assertEquals("synthetic-fresh-cursor", history.snapshot.olderBefore)
        val other = AssistantHistory(fake::request, initial) {}
        other.select(state.copy(conversation = id("synthetic-other-chat", "1")))
        assertTrue(other.snapshot.turns.isEmpty())
    }

    private fun page(selected: AssistantState, sequences: List<String>, before: String?): String = JSONObject()
        .put("revision", "8").put("conversation", JSONObject(selected.conversation!!))
        .put("head", selected.selectedHead?.let(::JSONObject) ?: JSONObject.NULL)
        .put("older_before", before ?: JSONObject.NULL)
        .put("turns", JSONArray(sequences.map { sequence -> JSONObject().put("operation", JSONObject()
            .put("id", JSONObject(id("synthetic-turn", sequence)))
            .put("body", JSONObject().put("kind", "turn_completed")
                .put("conversation", JSONObject(selected.conversation!!))
                .put("prompt", "Synthetic question $sequence")
                .put("answer", JSONObject().put("kind", "text").put("text", "Synthetic answer $sequence")))) }))
        .toString()

    private fun id(replica: String, sequence: String) = JSONObject().put("replica", replica).put("sequence", sequence).toString()
}
