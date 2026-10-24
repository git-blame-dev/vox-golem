package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.runtime.*
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.Executor

@RunWith(PhoneAssistantRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class EnginePhoneBackendTest {
    private class Queue : Executor {
        private val tasks = ConcurrentLinkedQueue<Runnable>()
        override fun execute(command: Runnable) { tasks.add(command) }
        fun drain() { while (true) (tasks.poll() ?: return).run() }
    }

    private class Fixture(private val status: String, private val contentType: String = "output_text") : AutoCloseable {
        private val context = RuntimeEnvironment.getApplication()
        private val actor = Queue()
        private val provider = Queue()
        private val store = AssistantStore(context).apply {
            val conversation = createConversation("Synthetic wire fixture").getJSONObject("operation").getJSONObject("id")
            selectConversation(conversation, null)
        }
        private val worker = AssistantRequestWorker(provider, object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                callback.onReady()
                callback.onDelta("Synthetic provisional text")
                return JSONObject().put("status", status).put("contentType", contentType)
                    .put("text", "Synthetic final content").toString()
            }
            override fun cancel(requestId: String) {}
        })
        private val models = object : AssistantModelStorage {
            // A fake optional provider avoids private credentials; no SDK/provider call is made.
            override fun read() = "agy-flash-high"
            override fun write(value: String) {}
        }
        private val engine = AssistantEngine::class.java.declaredConstructors.single {
            it.parameterTypes.size == 8 && it.parameterTypes.first() == AssistantStore::class.java
        }.newInstance(store, actor, worker, { "/synthetic/private/provider.json" }, models, null,
            NativeAssistantPolicyStorage(context), null) as AssistantEngine
        val backend = EnginePhoneBackend(context, engine)
        var terminal: Result<Any?>? = null
        val updates = mutableListOf<JSONObject>()
        init { engine.resume(); actor.drain() }

        fun admit() {
            backend.submit("synthetic-client", "Synthetic question", { packet ->
                updates += packet
                assertFalse(packet.getJSONObject("update").has("save_disposition"))
                assertTrue(backend.acknowledge("synthetic-client", packet.getString("providerRequestId"), packet.getString("revision"), true))
            }, { terminal = it })
            actor.drain()
            assertTrue(engine.snapshot().busy)
            assertNull(terminal)
        }
        fun receiveProvider() { provider.drain(); assertNull("Provider outcome is not the store commit", terminal) }
        fun settle() { actor.drain() }
        fun rejectCompletionCommit() {
            // Exercise production completion failure without adding a public test API to the store.
            AssistantStore::class.java.getDeclaredField("beforeCommit").apply { isAccessible = true }
                .set(store, { error("Synthetic completion commit failure") } as () -> Unit)
        }
        fun rejectPostCommitSelection() {
            AssistantStore::class.java.getDeclaredField("beforeSelection").apply { isAccessible = true }
                .set(store, { error("Synthetic post-commit selection failure") } as () -> Unit)
        }
        fun snapshot(): JSONObject {
            var result: Result<Any?>? = null
            backend.command("get_app_snapshot", JSONObject()) { result = it }
            actor.drain()
            return result!!.getOrThrow() as JSONObject
        }
        override fun close() { engine.stop(); provider.drain(); actor.drain(); worker.close(); store.close() }
    }

    @Test fun completedAndRefusedAreSavedOnlyAfterTheRealStoreCommit() {
        for ((contentType, kind) in listOf("output_text" to "completed", "refusal" to "refused")) {
            Fixture("complete", contentType).use { f ->
                f.admit()
                f.receiveProvider()
                assertTrue(f.updates.any { it.getJSONObject("update").getString("kind") == "delta" })
                f.settle()
                val outcome = f.terminal!!.getOrThrow() as JSONObject
                assertEquals(kind, outcome.getString("kind"))
                assertEquals("saved", outcome.getString("save_disposition"))
                assertFalse(outcome.has("unsaved_result"))
                val snapshot = f.snapshot()
                assertTrue(snapshot.has("unsaved_result"))
                assertSame(JSONObject.NULL, snapshot.get("unsaved_result"))
                assertEquals(1, snapshot.getJSONArray("transcript").length())
                assertEquals(outcome.getJSONObject("turn").toString(), snapshot.getJSONArray("transcript").getJSONObject(0).toString())
            }
        }
    }

    @Test fun failedCompletionCommitCannotPublishASavedOrCanonicalTurn() {
        Fixture("complete").use { f ->
            f.admit()
            f.rejectCompletionCommit()
            f.receiveProvider()
            f.settle()
            assertTrue(f.terminal!!.exceptionOrNull() is AssistantPhoneFailure)
            assertEquals(0, f.snapshot().getJSONArray("transcript").length())
        }
    }

    @Test fun cancelledTerminalRemainsContentFree() {
        Fixture("cancelled").use { f ->
            f.admit()
            f.receiveProvider()
            f.settle()
            val outcome = f.terminal!!.getOrThrow() as JSONObject
            assertEquals(setOf("kind"), outcome.keys().asSequence().toSet())
            assertEquals("cancelled", outcome.getString("kind"))
            assertEquals(0, f.snapshot().getJSONArray("transcript").length())
        }
    }

    @Test fun postCommitLocalStateFailureRemainsSavedWithoutAnInlineSnapshot() {
        Fixture("complete").use { f ->
            f.admit()
            f.rejectPostCommitSelection()
            f.receiveProvider()
            f.settle()
            val outcome = f.terminal!!.getOrThrow() as JSONObject
            assertEquals("completed", outcome.getString("kind"))
            assertEquals("saved", outcome.getString("save_disposition"))
            assertTrue(outcome.has("turn"))
            assertTrue(outcome.has("warning"))
            assertFalse(outcome.has("app_snapshot"))
            assertFalse(outcome.has("unsaved_result"))
        }
    }
}
