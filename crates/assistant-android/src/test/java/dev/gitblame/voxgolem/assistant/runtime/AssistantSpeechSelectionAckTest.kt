package dev.gitblame.voxgolem.assistant.runtime

import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.speech.SpeechOptions
import dev.gitblame.voxgolem.assistant.speech.SpeechSelection
import org.junit.After
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.Executor

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AssistantSpeechSelectionAckTest {
    private class Queue : Executor {
        private val tasks = ConcurrentLinkedQueue<Runnable>()
        override fun execute(command: Runnable) { tasks.add(command) }
        fun next() { requireNotNull(tasks.poll()).run() }
        fun drain() { while (true) (tasks.poll() ?: return).run() }
    }
    private class Policies : AssistantPolicyStorage {
        var value = AssistantPolicy()
        var reject = false
        var attempts = 0
        val events = mutableListOf<String>()
        override fun read() = value
        override fun write(value: AssistantPolicy) {
            attempts++; events += "write"
            if (reject) { events += "rejected"; error("Synthetic policy storage failure") }
            this.value = value; events += "committed"
        }
    }
    private val store = AssistantStore(RuntimeEnvironment.getApplication())
    private val io = Queue()
    private val providerQueue = Queue()
    private val policies = Policies()
    private val engine = AssistantEngine(store, io, AssistantRequestWorker(providerQueue), { "/synthetic/private/auth.json" },
        object : AssistantModelStorage {
            override fun read() = "custom-sol-high"
            override fun write(value: String) {}
        }, policies = policies)
    init {
        engine.resume()
        engine.publishSpeechOptions(SpeechOptions(ttsProviders = listOf("synthetic.tts"),
            recognitionInstalledLanguages = listOf("en-AU"), recognitionSupportedLanguages = listOf("en-AU")))
        io.drain()
    }
    @After fun close() { engine.stop(); providerQueue.drain(); io.drain(); store.close() }

    @Test fun completionWaitsForTheQueuedActorCommitAndDefaultClearingUsesTheSameSetterPath() {
        val selected = SpeechSelection("en-AU", "synthetic.tts")
        val results = mutableListOf<AssistantState>()
        engine.setSpeechSelection(selected) { result -> policies.events += "callback"; results += result }
        assertEquals(0, policies.attempts); assertTrue(results.isEmpty())
        assertEquals(SpeechSelection(), engine.snapshot().speechAdmission().selection)
        io.next()
        assertEquals(listOf("write", "committed", "callback"), policies.events)
        assertEquals(selected, policies.value.selection())
        assertEquals(selected, results.single().speechAdmission().selection); assertNull(results.single().error)
        engine.publishSpeechOptions(SpeechOptions()); engine.refresh(); io.drain()
        assertEquals(1, results.size)
        engine.setSpeechSelection(SpeechSelection()) { results += it }
        assertEquals(1, results.size); io.drain()
        assertEquals(SpeechSelection(), policies.value.selection())
        assertEquals(SpeechSelection(), results.last().speechAdmission().selection)
        assertEquals(2, policies.attempts); assertEquals(2, results.size)
    }

    @Test fun matchingSnapshotAndMetadataDoNotAcknowledgeAQueuedStorageFailure() {
        val requested = SpeechSelection()
        val results = mutableListOf<AssistantState>()
        policies.reject = true
        engine.publishSpeechOptions(SpeechOptions())
        engine.setSpeechSelection(requested) { result -> policies.events += "callback"; results += result }
        assertEquals(requested, engine.snapshot().speechAdmission().selection)
        assertTrue(results.isEmpty())
        io.next() // A coalesced metadata projection is not policy settlement.
        assertTrue(results.isEmpty()); assertEquals(0, policies.attempts)
        io.next()
        assertEquals(listOf("write", "rejected", "callback"), policies.events)
        assertEquals("storage_error", results.single().error)
        assertEquals(requested, results.single().speechAdmission().selection)
        engine.publishSpeechOptions(SpeechOptions()); io.drain()
        assertEquals(1, results.size); assertEquals(AssistantPolicy(), policies.value)
    }

    @Test fun actorValidationRejectsWithoutStorageAndThrowingConsumerCannotRetireTheActor() {
        val results = mutableListOf<AssistantState>()
        engine.setSpeechSelection(SpeechSelection(ttsProvider = "synthetic.missing")) { results += it }
        assertTrue(results.isEmpty()); io.drain()
        assertEquals("unsupported_tts_provider", results.single().error); assertEquals(0, policies.attempts)
        var deliveries = 0
        engine.setSpeechSelection(SpeechSelection(ttsProvider = "synthetic.tts")) {
            deliveries++; error("Synthetic backend result consumer failure")
        }
        io.drain()
        assertEquals(1, deliveries); assertEquals("synthetic.tts", policies.value.speechTtsProvider)
        engine.setSpeechSelection(SpeechSelection()) { results += it }; io.drain()
        assertEquals(2, results.size); assertNull(results.last().error)
        assertEquals(SpeechSelection(), policies.value.selection())
    }
}
