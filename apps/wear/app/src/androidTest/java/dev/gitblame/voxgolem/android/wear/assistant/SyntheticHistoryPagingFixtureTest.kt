package dev.gitblame.voxgolem.android.wear.assistant

import android.os.Build
import android.os.Bundle
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.runtime.AssistantEngine
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.security.MessageDigest
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

@RunWith(AndroidJUnit4::class)
class SyntheticHistoryPagingFixtureTest {
    @Test fun seedNewConversationAndVerifyRuntimePagingWithoutReplacingExistingData() {
        assertTrue("This persistent synthetic fixture is emulator-only",
            Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish")
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val target = instrumentation.targetContext.applicationContext
        assertEquals("dev.gitblame.voxgolem.android", target.packageName)
        val runtime = AssistantRuntime.get(target)
        val initial = awaitValue<AssistantState> { runtime.refresh(it) }
        assertFalse("Do not seed while a runtime request is active", initial.busy)
        assertTrue("The existing runtime must be healthy before seeding", initial.error == null)

        val title = "Synthetic Wear paging 40 + sibling ${UUID.randomUUID()}"
        AssistantStore(target).use { store ->
            val before = store.snapshot()
            val oldRecords = fingerprints(before.getJSONArray("records"))
            val oldRetired = fingerprints(before.getJSONArray("retired"))
            val oldDrafts = existingDrafts(store, before)
            val conversation = operationId(store.createConversation(title))
            val mainIds = mutableListOf<JSONObject>()
            var head: JSONObject? = null
            var siblingContext: JSONObject? = null
            repeat(40) { index ->
                val context = store.context(conversation, head)
                val turn = store.completeTurn(store.allocateId(), context,
                    "Synthetic main prompt ${index + 1}",
                    JSONObject().put("kind", "text").put("text", "Synthetic main answer ${index + 1}"),
                    "instant", syntheticProvenance())
                head = operationId(turn)
                mainIds += head!!
                if (index == 19) siblingContext = store.context(conversation, head)
            }
            val mainHead = mainIds.last()
            val sibling = operationId(store.completeTurn(store.allocateId(), siblingContext!!,
                "Synthetic sibling prompt", JSONObject().put("kind", "text").put("text", "Synthetic sibling answer"),
                "instant", syntheticProvenance()))
            store.selectConversation(conversation, mainHead)

            // Use normal selection APIs to load this conversation's draft, not the previous projection's draft.
            val conversationSelected = awaitValue<AssistantState> { runtime.selectConversation(conversation.toString(), it) }
            assertTrue("Selecting the synthetic conversation must succeed without starting a request",
                conversationSelected.error == null && !conversationSelected.busy)
            assertSelection(awaitValue<AssistantState> { runtime.selectHead(mainHead.toString(), it) }, conversation, mainHead)
            // Retain the new selection and fixture for the subsequent rendered paging inspection.
            AssistantRuntime.refreshIfStarted()
            val selected = awaitValue<AssistantState> { runtime.refresh(it) }
            assertSelection(selected, conversation, mainHead)
            val fixture = store.view().getJSONArray("conversations").objects().single {
                sameId(it.getJSONObject("id"), conversation)
            }
            assertEquals(title, fixture.getString("title"))
            val heads = fixture.getJSONArray("heads").objects()
            assertEquals(2, heads.size)
            assertTrue("Both synthetic terminal branches must remain valid",
                heads.any { sameId(it, mainHead) } && heads.any { sameId(it, sibling) })

            val latest = history(runtime, null, conversation, mainHead)
            assertPage(latest, conversation, mainHead, mainIds.drop(8), 9..40, sibling)
            assertFalse("The latest 32-turn page must expose an older cursor", latest.isNull("older_before"))
            val older = history(runtime, latest.getString("older_before"), conversation, mainHead)
            assertPage(older, conversation, mainHead, mainIds.take(8), 1..8, sibling)
            assertTrue("The older eight turns must exhaust the selected branch", older.isNull("older_before"))
            assertEquals(latest.getString("revision"), older.getString("revision"))
            val allTurns = older.getJSONArray("turns").objects() + latest.getJSONArray("turns").objects()
            assertEquals(40, allTurns.size)
            assertTrue("History must preserve causal order without duplicate or sibling turns",
                allTurns.map(::operationId).zip(mainIds).all { (actual, expected) -> sameId(actual, expected) })

            val contextResult = awaitValue<HistoryResult> { done ->
                runtime.selectedContext { raw, state -> done(HistoryResult(raw, state)) }
            }
            assertSelection(contextResult.state, conversation, mainHead)
            assertTrue("The actual runtime must return selected provider context", contextResult.raw != null)
            val context = JSONObject(contextResult.raw!!)
            assertEquals(20, context.getJSONArray("turns").length())
            assertEquals(20, context.getJSONArray("ancestor_ids").length())
            assertTrue("Provider context must retain only the last twenty main-branch turns",
                context.getJSONArray("turns").objects().map(::operationId).zip(mainIds.takeLast(20))
                    .all { (actual, expected) -> sameId(actual, expected) })
            assertTrue("Provider context must keep the selected main parent", sameId(context.getJSONObject("parent"), mainHead))

            val after = store.snapshot()
            assertEquals(42, after.getJSONArray("records").length() - before.getJSONArray("records").length())
            assertTrue("Every previous immutable history record must remain unchanged",
                fingerprints(after.getJSONArray("records")).containsAll(oldRecords))
            assertTrue("The previous retired ledger must remain unchanged",
                fingerprints(after.getJSONArray("retired")) == oldRetired)
            assertTrue("Every previous draft, including the unselected new draft, must remain unchanged",
                oldDrafts.all { (id, digest) -> fingerprint(store.draft(id)) == digest })
            assertTrue("The selected fixture head must remain durable",
                sameId(store.localState().getJSONObject("selected_head"), mainHead))
            AssistantRuntime.refreshIfStarted()
            assertSelection(awaitValue<AssistantState> { runtime.refresh(it) }, conversation, mainHead)
        }
        instrumentation.sendStatus(0, Bundle().apply {
            putString("stream", "\n$title | PASS main=40 sibling=1 history=32+8 context=20 previous-history-and-drafts=preserved\n")
        })
    }

    private data class HistoryResult(val raw: String?, val state: AssistantState)

    private fun history(runtime: AssistantEngine, before: String?, conversation: JSONObject, head: JSONObject): JSONObject {
        val result = awaitValue<HistoryResult> { done ->
            runtime.history(before) { raw, state -> done(HistoryResult(raw, state)) }
        }
        assertSelection(result.state, conversation, head)
        assertTrue("The actual history gateway must return a page", result.raw != null)
        return JSONObject(result.raw!!)
    }

    private fun assertSelection(state: AssistantState, conversation: JSONObject, head: JSONObject) {
        assertTrue("Runtime reconciliation must settle without an error", state.error == null)
        assertFalse("Fixture reads must never start a provider request", state.busy)
        assertTrue("Runtime must project the synthetic conversation",
            state.conversation?.let { sameId(JSONObject(it), conversation) } == true)
        assertTrue("Runtime must retain the selected main branch",
            state.selectedHead?.let { sameId(JSONObject(it), head) } == true)
        assertTrue("The explicitly selected branch must not require a branch choice", state.branchChoices.isEmpty())
    }

    private fun assertPage(page: JSONObject, conversation: JSONObject, head: JSONObject,
        expectedIds: List<JSONObject>, numbers: IntRange, sibling: JSONObject) {
        assertTrue("History must belong to the synthetic conversation", sameId(page.getJSONObject("conversation"), conversation))
        assertTrue("History must belong to the selected main branch", sameId(page.getJSONObject("head"), head))
        val records = page.getJSONArray("turns").objects()
        assertEquals(expectedIds.size, records.size)
        records.forEachIndexed { index, record ->
            val id = operationId(record)
            assertTrue("History must return the main branch in native causal order", sameId(id, expectedIds[index]))
            assertFalse("A sibling completion must never leak into selected-branch history", sameId(id, sibling))
            val body = record.getJSONObject("operation").getJSONObject("body")
            assertEquals("turn_completed", body.getString("kind"))
            assertEquals("Synthetic main prompt ${numbers.first + index}", body.getString("prompt"))
            assertEquals("Synthetic main answer ${numbers.first + index}", body.getJSONObject("answer").getString("text"))
        }
    }

    private fun existingDrafts(store: AssistantStore, snapshot: JSONObject): List<Pair<JSONObject?, String>> {
        val conversations = mutableListOf<JSONObject>()
        snapshot.getJSONArray("records").objects().forEach { record ->
            val operation = record.getJSONObject("operation")
            val body = operation.getJSONObject("body")
            val id = if (body.getString("kind") == "conversation_created") operation.getJSONObject("id")
                else body.getJSONObject("conversation")
            if (conversations.none { sameId(it, id) }) conversations += id
        }
        snapshot.getJSONArray("retired").objects().forEach { record ->
            val id = record.getJSONObject("conversation")
            if (conversations.none { sameId(it, id) }) conversations += id
        }
        return listOf(null to fingerprint(store.draft(null))) + conversations.map { it to fingerprint(store.draft(it)) }
    }

    private fun syntheticProvenance() = JSONObject().put("requested_model", "synthetic-model")
        .put("returned_model", JSONObject.NULL).put("policy_id", "synthetic-policy").put("policy_version", "1")
        .put("instruction_profile_id", "synthetic-instructions").put("instruction_profile_version", "1")

    private fun operationId(record: JSONObject): JSONObject = record.getJSONObject("operation").getJSONObject("id")
    private fun sameId(first: JSONObject, second: JSONObject) =
        first.getString("replica") == second.getString("replica") && first.getString("sequence") == second.getString("sequence")
    private fun JSONArray.objects(): List<JSONObject> = (0 until length()).map(::getJSONObject)
    private fun fingerprints(records: JSONArray): Set<String> = records.objects().map(::fingerprint).toSet()
    private fun fingerprint(value: JSONObject): String = MessageDigest.getInstance("SHA-256")
        .digest(canonical(value).toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it) }

    private fun canonical(value: Any): String = when (value) {
        is JSONObject -> value.keys().asSequence().toList().sorted().joinToString(",", "{", "}") {
            "${JSONObject.quote(it)}:${canonical(value.get(it))}"
        }
        is JSONArray -> (0 until value.length()).joinToString(",", "[", "]") { canonical(value.get(it)) }
        is String -> JSONObject.quote(value)
        else -> value.toString()
    }

    private fun <T : Any> awaitValue(start: ((T) -> Unit) -> Unit): T {
        val result = AtomicReference<T>()
        val settled = CountDownLatch(1)
        start { value -> result.set(value); settled.countDown() }
        assertTrue("The actual runtime callback must settle within thirty seconds", settled.await(30, TimeUnit.SECONDS))
        return result.get()
    }
}
