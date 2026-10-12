package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.DomainGateway
import dev.gitblame.voxgolem.assistant.runtime.*
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.Executor

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class EnginePhoneSnapshotTest {
    private class Queue : Executor {
        private val tasks = ConcurrentLinkedQueue<Runnable>()
        override fun execute(command: Runnable) { tasks.add(command) }
        fun drain() { while (true) (tasks.poll() ?: return).run() }
    }

    private class Fixture(val choose: Boolean = false, val rejectContext: Boolean = false) : AutoCloseable {
        private val context = RuntimeEnvironment.getApplication()
        private val io = Queue()
        val calls = mutableListOf<String>()
        val conversation = id("1")
        private val head = id("3")
        private val gateway = DomainGateway { raw ->
            // Simulate only the domain boundary. The real actor/store own refresh, draft,
            // selection and fresh-snapshot construction; no provider is involved.
            val input = JSONObject(raw)
            val kind = input.getString("kind")
            calls += kind
            val revision = input.getJSONObject("snapshot").getString("revision")
            val value = when (kind) {
                "projection" -> {
                    val request = input.getJSONObject("request")
                    check(request.getJSONObject("conversation").toString() == conversation.toString())
                    val decision = if (choose) JSONObject().put("kind", "choose").put("heads", JSONArray().put(head).put(id("5")))
                        else JSONObject().put("kind", "keep").put("selected_head", head)
                    var read: Any = JSONObject.NULL
                    if (!request.isNull("read") && !choose) {
                        check(request.getJSONObject("selected_head").toString() == head.toString())
                        val query = request.getJSONObject("read")
                        check(query.getString("kind") == "context")
                        check(query.getInt("max_turns") == 20 && query.getInt("max_text_bytes") == 256 * 1024)
                        read = if (rejectContext) JSONObject().put("ok", false).put("error", JSONObject().put("kind", "invalid").put("field", "synthetic_context"))
                            else JSONObject().put("ok", true).put("value", JSONObject().put("revision", revision).put("turns", JSONArray()))
                    }
                    JSONObject().put("view", JSONObject().put("revision", revision).put("records", JSONArray()).put("pending", JSONArray())
                        .put("conversations", JSONArray().put(JSONObject().put("id", conversation).put("title", "Synthetic conversation").put("deleted", false))))
                        .put("continuation", decision).put("read", read)
                }
                else -> error("Unexpected domain call: $kind")
            }
            JSONObject().put("ok", true).put("value", value).toString()
        }
        private val store = AssistantStore(context, gateway).apply {
            selectConversation(conversation, if (choose) null else head)
            saveDraft(conversation, "Synthetic retained draft")
        }
        private val models = object : AssistantModelStorage {
            override fun read() = "custom-sol-high"
            override fun write(value: String) {}
        }
        private val worker = AssistantRequestWorker(io, object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String =
                error("Snapshot must not submit a provider request")
            override fun cancel(requestId: String) {}
        })
        // Use the existing JVM overload, preserving the shared module's internal constructor.
        private val engine = AssistantEngine::class.java.declaredConstructors.single {
            it.parameterTypes.size == 8 && it.parameterTypes.first() == AssistantStore::class.java
        }.newInstance(store, io, worker, { "/synthetic/private/provider.json" }, models, null,
            NativeAssistantPolicyStorage(context), gateway) as AssistantEngine
        private val backend = EnginePhoneBackend(context, engine)
        init { io.drain(); calls.clear() }
        fun snapshot(): Result<Any?> {
            var outcome: Result<Any?>? = null
            backend.command("get_app_snapshot", JSONObject()) { outcome = it }
            io.drain()
            return outcome ?: error("Snapshot did not settle")
        }
        override fun close() { engine.stop(); io.drain(); worker.close(); store.close() }
        companion object {
            fun id(sequence: String) = JSONObject().put("replica", "00000000-0000-4000-8000-000000000701").put("sequence", sequence)
        }
    }

    @Test fun snapshotUsesOneFreshProjectionForSelectionAndCanonicalContextAndRetainsDraft() {
        Fixture().use { f ->
            val value = f.snapshot().getOrThrow() as JSONObject
            assertEquals(listOf("projection"), f.calls)
            assertEquals(PhoneSnapshotEncoder.wireId(f.conversation), value.getString("session_id"))
            assertEquals("Synthetic retained draft", value.getJSONObject("assistant").getString("draft"))
        }
    }

    @Test fun unresolvedBranchChoicesDoNotFetchOrFlattenContext() {
        Fixture(choose = true).use { f ->
            val value = f.snapshot().getOrThrow() as JSONObject
            assertEquals(listOf("projection"), f.calls)
            assertEquals(2, value.getJSONObject("assistant").getJSONArray("branch_choices").length())
            assertTrue(value.getJSONObject("assistant").isNull("selected_head"))
            assertEquals(0, value.getJSONArray("transcript").length())
        }
    }

    @Test fun failedCanonicalContextDoesNotPublishASuccessfulSnapshot() {
        Fixture(rejectContext = true).use { f ->
            val failure = f.snapshot().exceptionOrNull() as AssistantPhoneFailure
            assertEquals("internal", failure.code)
            assertEquals(listOf("projection"), f.calls)
        }
    }
}
