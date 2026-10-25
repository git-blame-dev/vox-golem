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

@RunWith(PhoneAssistantRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class EnginePhoneSnapshotTest {
    private class Queue : Executor {
        private val tasks = ConcurrentLinkedQueue<Runnable>()
        var closed = false
        var rejected = 0
        var accepted = 0
        override fun execute(command: Runnable) {
            if (closed) { rejected++; throw java.util.concurrent.RejectedExecutionException("Synthetic retired actor") }
            accepted++; tasks.add(command)
        }
        fun drain() { while (true) (tasks.poll() ?: return).run() }
        fun runNext() { checkNotNull(tasks.poll()).run() }
    }

    private class Fixture(val choose: Boolean = false, val rejectContext: Boolean = false,
        initiallyEmpty: Boolean = false) : AutoCloseable {
        private val context = RuntimeEnvironment.getApplication()
        private val io = Queue()
        val calls = mutableListOf<String>()
        lateinit var conversation: JSONObject
        private lateinit var head: JSONObject
        private val store = AssistantStore(context).apply {
            if (initiallyEmpty) {
                selectConversation(null, null)
                saveDraft(null, "Synthetic unassigned draft")
            } else {
            conversation = createConversation("Synthetic conversation").getJSONObject("operation").getJSONObject("id")
            val ctx = context(conversation, null)
            head = completeTurn(
                allocateId(),
                ctx,
                "Synthetic question 1",
                JSONObject().put("kind", "text").put("text", "Synthetic answer 1"),
                "instant",
                provenance()
            ).getJSONObject("operation").getJSONObject("id")
            if (choose) {
                completeTurn(
                    allocateId(),
                    ctx,
                    "Synthetic question 2",
                    JSONObject().put("kind", "text").put("text", "Synthetic answer 2"),
                    "instant",
                    provenance()
                )
            }
            selectConversation(conversation, if (choose) null else head)
            saveDraft(conversation, "Synthetic retained draft")
            }
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
        private val evaluateOwnedRead = AssistantStore::class.java.declaredMethods.single {
            it.name.startsWith("evaluateOwnedRead") && it.parameterTypes.size == 1
        }.apply { isAccessible = true }
        private val engine = AssistantEngine::class.java.declaredConstructors.single {
            it.parameterTypes.size == 8 && it.parameterTypes.first() == AssistantStore::class.java
        }.newInstance(store, io, worker, { "/synthetic/private/provider.json" }, models, null,
            NativeAssistantPolicyStorage(context), DomainGateway { raw ->
                val request = JSONObject(raw)
                calls += request.getString("kind")
                if (rejectContext) {
                    val inner = request.getJSONObject("request")
                    if (!inner.isNull("read") && inner.getJSONObject("read").getString("kind") == "context") {
                        val base = JSONObject(evaluateOwnedRead.invoke(store, JSONObject(raw).apply {
                            getJSONObject("request").put("read", JSONObject.NULL)
                        }.toString()) as String)
                        base.getJSONObject("value").put("read", JSONObject().put("ok", false)
                            .put("error", JSONObject().put("kind", "invalid").put("field", "selected_head")))
                        return@DomainGateway base.toString()
                    }
                }
                evaluateOwnedRead.invoke(store, raw) as String
            }) as AssistantEngine
        val backend = EnginePhoneBackend(context, engine)
        init { io.drain(); calls.clear() }
        fun snapshot(): Result<Any?> {
            var outcome: Result<Any?>? = null
            backend.command("get_app_snapshot", JSONObject()) { outcome = it }
            io.drain()
            return outcome ?: error("Snapshot did not settle")
        }
        fun command(name: String, arguments: JSONObject): Result<Any?> {
            var outcome: Result<Any?>? = null
            backend.command(name, arguments) { outcome = it }
            io.drain()
            return outcome ?: error("Command $name did not settle")
        }
        fun createAdditionalConversation(title: String): JSONObject =
            store.createConversation(title).getJSONObject("operation").getJSONObject("id")
        fun terminalAfterMutation(localSelectionOnly: Boolean): JSONObject {
            // Simulate the already-committed result boundary; the real engine/store fence
            // and phone encoder run here without requiring a provider or host JNI.
            val captured = engine.snapshot()
            val revision = JSONObject(captured.viewJson).getString("revision")
            val raw = JSONObject().put("revision", revision)
                .put("conversation", conversation).put("parent", head).put("turns", JSONArray()).toString()
            val result = AssistantRequestResult("synthetic-client", status = "complete", revision = revision,
                recordJson = "{\"syntheticDurableRecord\":true}", terminalContextJson = raw)
            val durableTurn = JSONObject().put("id", "synthetic-durable-turn")
            val voiceContext = JSONObject().put("conversation", "synthetic-committed-conversation").put("selectedHead", "synthetic-committed-head")
            val outcome = JSONObject().put("kind", "completed").put("save_disposition", "saved")
                .put("turn", durableTurn).put("voiceContext", voiceContext)
            backend.attachTerminalSnapshot(outcome, result, captured)
            check(outcome.has("app_snapshot"))
            outcome.remove("app_snapshot")
            if (localSelectionOnly) store.selectConversation(conversation, null)
            else store.createConversation("Synthetic concurrent")
            backend.attachTerminalSnapshot(outcome, result, captured)
            assertEquals(durableTurn.toString(), outcome.getJSONObject("turn").toString())
            assertEquals("saved", outcome.getString("save_disposition"))
            assertEquals(voiceContext.toString(), outcome.getJSONObject("voiceContext").toString())
            return outcome
        }
        fun closeDispatchForTest() { io.closed = true }
        fun rejectedDispatches() = io.rejected
        fun acceptedDispatches() = io.accepted
        @Suppress("UNCHECKED_CAST") fun installDeliveryForTest() {
            val field = EnginePhoneBackend::class.java.getDeclaredField("delivery").apply { isAccessible = true }
            val sink = field.get(backend) as java.util.concurrent.atomic.AtomicReference<PhoneRequestDelivery?>
            sink.set(PhoneRequestDelivery("synthetic-owned-request", {}, {}))
        }
        fun drain() = io.drain()
        fun runNext() = io.runNext()
        fun conversationCount() = store.view().getJSONArray("conversations").length()
        fun selectedDraft() = engine.snapshot().draft
        override fun close() { io.closed = false; engine.stop(); io.drain(); worker.close(); store.close() }
        companion object {
            private fun provenance() = JSONObject().put("requested_model", "synthetic-model")
                .put("returned_model", JSONObject.NULL).put("policy_id", "synthetic-policy").put("policy_version", "1")
                .put("instruction_profile_id", "synthetic-instructions").put("instruction_profile_version", "1")
        }
    }

    @Test fun retiredTransportRejectsLateCommandsAndLifecycleWithoutTouchingClosedActor() {
        Fixture().use { f ->
            f.backend.retireIngress()
            f.closeDispatchForTest()
            f.backend.foreground(false)
            f.backend.foreground(true)
            f.backend.prewarm()
            f.backend.saveSpeechDraft("Synthetic late draft")
            f.backend.saveOwnedSpeechDraft("Synthetic late draft", null, null)
            f.backend.publishSpeechOptions(dev.gitblame.voxgolem.assistant.speech.SpeechOptions())
            f.backend.cancelDelivery("synthetic-late-request")
            f.backend.observeSpeechContext { fail("Retired observer") }.close()
            f.backend.observeSpeechStream { _, _ -> fail("Retired stream") }.close()
            assertFalse(f.backend.acknowledge("synthetic-late-request", "synthetic-provider", "1", false))
            var commands = 0
            f.backend.command("get_app_snapshot", JSONObject()) { result ->
                commands++; assertEquals("unavailable", (result.exceptionOrNull() as AssistantPhoneFailure).code)
            }
            var terminals = 0
            f.backend.submit("synthetic-late-request", "Synthetic input", { fail("Retired delta") }) { result ->
                terminals++; assertEquals("unavailable", (result.exceptionOrNull() as AssistantPhoneFailure).code)
            }
            assertEquals(1, commands); assertEquals(1, terminals)
            assertEquals(0, f.rejectedDispatches())
        }
    }

    @Test fun queuedContextPublicationCannotReenterRetiredTransport() {
        Fixture().use { f ->
            var contexts = 0
            val observer = f.backend.observeSpeechContext { contexts++ }
            f.backend.retireIngress()
            f.drain()
            assertEquals(0, contexts)
            observer.close()
        }
    }

    @Test fun retirementSignalsOwnedCancellationBeforeActorCloseAndNeverReschedulesIt() {
        Fixture().use { f ->
            f.installDeliveryForTest()
            val before = f.acceptedDispatches()
            f.backend.retireIngress()
            assertEquals(before + 1, f.acceptedDispatches())
            f.closeDispatchForTest()
            f.backend.retireIngress()
            assertEquals(0, f.rejectedDispatches())
            assertEquals(before + 1, f.acceptedDispatches())
        }
    }

    @Test fun queuedEmptySnapshotCannotScheduleConversationAfterRetirement() {
        NewVoiceShortcutLauncher.cancelEntries()
        Fixture(initiallyEmpty = true).use { f ->
            val beforeCount = f.conversationCount()
            val beforeDispatch = f.acceptedDispatches()
            var replies = 0
            var outcome: Result<Any?>? = null
            f.backend.command("get_app_snapshot", JSONObject()) { replies++; outcome = it }
            f.backend.retireIngress()
            f.drain()
            assertEquals(beforeDispatch + 1, f.acceptedDispatches())
            assertEquals(beforeCount, f.conversationCount())
            assertEquals(1, replies)
            assertEquals("unavailable", (outcome!!.exceptionOrNull() as AssistantPhoneFailure).code)
        }
    }

    @Test fun alreadyQueuedConversationCannotScheduleDraftContinuationAfterRetirement() {
        NewVoiceShortcutLauncher.cancelEntries()
        Fixture(initiallyEmpty = true).use { f ->
            var replies = 0
            var outcome: Result<Any?>? = null
            f.backend.command("get_app_snapshot", JSONObject()) { replies++; outcome = it }
            f.runNext() // selectedContext has queued the pre-retirement conversation operation.
            val beforeRetireDispatch = f.acceptedDispatches()
            f.backend.retireIngress()
            f.drain()
            assertEquals(beforeRetireDispatch, f.acceptedDispatches())
            assertEquals(1, replies)
            assertEquals("unavailable", (outcome!!.exceptionOrNull() as AssistantPhoneFailure).code)
            assertEquals("", f.selectedDraft())
        }
    }

    @Test fun frozenLatePluginBindingLoadsAndRejectsVoiceWithoutConstructingSpeechOrUsingActor() {
        Fixture().use { f ->
            f.backend.retireIngress()
            f.closeDispatchForTest()
            val controller = org.robolectric.Robolectric.buildActivity(androidx.activity.ComponentActivity::class.java).create().start()
            val activity = controller.get()
            var speechReads = 0
            val binding = object : PhoneActivityBinding {
                override val backend = f.backend
                override val speech: PhoneSpeechSession get() { speechReads++; error("No speech owner was ever constructed") }
                override fun webViewCreated(webView: android.webkit.WebView) = Unit
                override fun admitVoice(policy: String, draft: String, loop: Boolean, shortcut: String?) = error("Retired voice")
                override fun admitPrompt(requestId: String, prompt: String) = error("Retired prompt")
                override fun terminal(requestId: String, result: Result<Any?>) = Unit
            }
            val plugin = AssistantPlugin(activity, binding)
            val view = android.webkit.WebView(activity)
            plugin.load(view)
            controller.resume()
            val commands = listOf<(app.tauri.plugin.Invoke) -> Unit>(plugin::assistant_voice_status,
                plugin::get_app_snapshot, plugin::set_preferences, plugin::cancel_prompt,
                plugin::start_assistant_voice, plugin::finish_assistant_dictation, plugin::stop_assistant_voice,
                plugin::prepare_new_voice_shortcut, plugin::stop_send_new_voice_shortcut,
                plugin::submit_prompt, plugin::new_conversation, plugin::select_conversation, plugin::select_conversation_head)
            val mapper = Class.forName("com.fasterxml.jackson.databind.ObjectMapper").getConstructor().newInstance()
            for (command in commands) {
                val replies = mutableListOf<Pair<Long, String>>()
                val sender: (Long, String) -> Unit = { callback, value -> replies += callback to value }
                val invoke = app.tauri.plugin.Invoke::class.java.constructors.single()
                    .newInstance(1L, "synthetic", 10L, 11L, sender, "{}", mapper) as app.tauri.plugin.Invoke
                command(invoke)
                assertEquals(11L, replies.single().first)
                assertEquals("unavailable", JSONObject(replies.single().second).getString("code"))
            }
            controller.pause().stop().destroy()
            view.destroy()
            assertEquals(0, speechReads)
            assertEquals(0, f.rejectedDispatches())
        }
    }

    @Test fun snapshotUsesOneFreshProjectionForSelectionAndCanonicalContextAndRetainsDraft() {
        Fixture().use { f ->
            val value = f.snapshot().getOrThrow() as JSONObject
            assertTrue(value.has("unsaved_result"))
            assertSame(JSONObject.NULL, value.get("unsaved_result"))
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

    @Test fun externallyCommittedCanonicalMutationOmitsInlineSnapshotButPreservesDurableOutcome() {
        Fixture().use { assertFalse(it.terminalAfterMutation(localSelectionOnly = false).has("app_snapshot")) }
    }

    @Test fun localSelectionOnlyMutationOmitsInlineSnapshotButPreservesDurableOutcome() {
        Fixture().use { assertFalse(it.terminalAfterMutation(localSelectionOnly = true).has("app_snapshot")) }
    }

    @Test fun conversationAndHeadPagesReturnBoundedItemsAndMapRevisionConflictToBusy() {
        Fixture(choose = true).use { f ->
            f.createAdditionalConversation("Synthetic second")
            val snap = f.snapshot().getOrThrow() as JSONObject
            val revision = snap.getJSONObject("assistant").getString("revision")
            f.calls.clear()

            val convPage = f.command("list_assistant_conversations", JSONObject().put("expectedRevision", revision).put("offset", 0)).getOrThrow() as JSONObject
            assertEquals(revision, convPage.getString("revision"))
            assertEquals(2, convPage.getJSONArray("conversations").length())
            assertEquals(2, convPage.getInt("next_offset"))

            val headPage = f.command("list_assistant_heads", JSONObject().put("expectedRevision", revision).put("conversationId", PhoneSnapshotEncoder.wireId(f.conversation)).put("offset", 0)).getOrThrow() as JSONObject
            assertEquals(revision, headPage.getString("revision"))
            assertEquals(2, headPage.getJSONArray("heads").length())
            assertEquals(2, headPage.getInt("next_offset"))
            assertTrue("Pagination reads must not invoke projection refresh", f.calls.isEmpty())

            val staleRevision = "0"
            val staleConv = f.command("list_assistant_conversations", JSONObject().put("expectedRevision", staleRevision).put("offset", 0)).exceptionOrNull() as AssistantPhoneFailure
            assertEquals("busy", staleConv.code)

            val staleHeads = f.command("list_assistant_heads", JSONObject().put("expectedRevision", staleRevision).put("conversationId", PhoneSnapshotEncoder.wireId(f.conversation)).put("offset", 0)).exceptionOrNull() as AssistantPhoneFailure
            assertEquals("busy", staleHeads.code)
            assertTrue("Pagination conflicts must not invoke projection refresh", f.calls.isEmpty())

            val recovered = f.snapshot().getOrThrow() as JSONObject
            assertTrue("Global assistant error must remain null after pagination revision conflict", recovered.getJSONObject("assistant").isNull("error"))
        }
    }
}

class PhoneAssistantRunner(testClass: Class<*>) : RobolectricTestRunner(testClass) {
    override fun createClassLoaderConfig(method: org.junit.runners.model.FrameworkMethod): org.robolectric.internal.bytecode.InstrumentationConfiguration =
        org.robolectric.internal.bytecode.InstrumentationConfiguration.Builder(super.createClassLoaderConfig(method))
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.AssistantDomainNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.AssistantStoreNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.speech.AssistantSpeechNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.runtime.AssistantProviderNative")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.runtime.ProviderNativeCalls")
            .doNotAcquireClass("dev.gitblame.voxgolem.assistant.runtime.ProviderDeltaCallback").build()
}
