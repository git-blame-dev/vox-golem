package dev.gitblame.voxgolem.android.wear.assistant

import java.util.ArrayDeque
import java.util.concurrent.Executor
import org.junit.Assert.*
import org.junit.Test

class WearAssistantWorkerTest {
    private class Queue : Executor {
        val tasks = ArrayDeque<Runnable>()
        override fun execute(command: Runnable) { tasks.add(command) }
        fun drain() { while (tasks.isNotEmpty()) tasks.removeFirst().run() }
    }

    private class Native : AssistantNativeCalls {
        var responses = 0
        val cancelled = mutableListOf<String>()
        var body: (DeltaCallback) -> String = { callback ->
            callback.onReady()
            callback.onDelta("Synthetic answer")
            "synthetic-complete"
        }
        override fun respond(requestId: String, authPath: String, requestJson: String, callback: DeltaCallback): String {
            responses++
            return body(callback)
        }
        override fun cancel(requestId: String) { cancelled += requestId }
    }

    private class Listener : AssistantRequestListener {
        val deltas = mutableListOf<String>()
        val outcomes = mutableListOf<String>()
        override fun onDelta(requestId: String, text: String) { deltas += text }
        override fun onOutcome(requestId: String, outcomeJson: String) { outcomes += outcomeJson }
    }

    @Test fun queuedCancellationNeverStartsNativeOrPublishes() {
        val queue = Queue(); val native = Native(); val listener = Listener()
        val worker = WearAssistantWorker(queue, native)
        assertTrue(worker.submit("synthetic-1", "/synthetic/auth.json", "{}", listener))
        worker.cancel()
        queue.drain()
        assertEquals(0, native.responses)
        assertTrue(listener.deltas.isEmpty())
        assertTrue(listener.outcomes.isEmpty())
        assertTrue(native.cancelled.isEmpty())
    }

    @Test fun cancellationBeforeReadyIsReplayedOnlyAfterRegistration() {
        val queue = Queue(); val native = Native(); val listener = Listener()
        val worker = WearAssistantWorker(queue, native)
        native.body = { callback ->
            worker.cancel()
            assertTrue(native.cancelled.isEmpty())
            callback.onReady()
            callback.onDelta("Synthetic stale text")
            "synthetic-complete"
        }
        worker.submit("synthetic-1", "/synthetic/auth.json", "{}", listener)
        queue.drain()
        assertEquals(listOf("synthetic-1"), native.cancelled)
        assertTrue(listener.deltas.isEmpty())
        assertTrue(listener.outcomes.isEmpty())
    }

    @Test fun activeCancellationSignalsNativeAndSuppressesLateCallbacks() {
        val queue = Queue(); val native = Native(); val listener = Listener()
        val worker = WearAssistantWorker(queue, native)
        native.body = { callback ->
            callback.onReady()
            callback.onDelta("Synthetic first")
            worker.cancel()
            callback.onDelta("Synthetic late")
            "synthetic-complete"
        }
        worker.submit("synthetic-1", "/synthetic/auth.json", "{}", listener)
        queue.drain()
        assertEquals(listOf("synthetic-1"), native.cancelled)
        assertEquals(listOf("Synthetic first"), listener.deltas)
        assertTrue(listener.outcomes.isEmpty())
    }

    @Test fun oneOwnerRemainsReservedUntilCancelledWorkerRetires() {
        val queue = Queue(); val native = Native(); val listener = Listener()
        val worker = WearAssistantWorker(queue, native)
        assertTrue(worker.submit("synthetic-1", "/synthetic/auth.json", "{}", listener))
        worker.cancel()
        assertFalse(worker.submit("synthetic-2", "/synthetic/auth.json", "{}", listener))
        queue.drain()
        assertTrue(worker.submit("synthetic-2", "/synthetic/auth.json", "{}", listener))
        queue.drain()
        assertEquals(listOf("Synthetic answer"), listener.deltas)
        assertEquals(listOf("synthetic-complete"), listener.outcomes)
    }

    @Test fun callbacksFromRetiredWorkerCannotEnterReplacement() {
        val queue = Queue(); val native = Native(); val listener = Listener()
        val worker = WearAssistantWorker(queue, native)
        lateinit var oldCallback: DeltaCallback
        native.body = { callback -> oldCallback = callback; callback.onReady(); "synthetic-complete" }
        worker.submit("synthetic-1", "/synthetic/auth.json", "{}", listener)
        queue.drain()
        worker.submit("synthetic-2", "/synthetic/auth.json", "{}", listener)
        oldCallback.onDelta("Synthetic stale")
        assertTrue(listener.deltas.isEmpty())
        queue.drain()
    }

    @Test fun callbackUtf8BudgetAcceptsBoundaryThenRejectsBeforePublishing() {
        val queue = Queue(); val native = Native(); val listener = Listener()
        val worker = WearAssistantWorker(queue, native)
        native.body = { callback ->
            callback.onReady()
            callback.onDelta("🧪".repeat(256 * 1024 / 4))
            assertThrows(IllegalStateException::class.java) { callback.onDelta("x") }
            "synthetic-error"
        }
        worker.submit("synthetic-1", "/synthetic/auth.json", "{}", listener)
        queue.drain()
        assertEquals(1, listener.deltas.size)
        assertEquals(256 * 1024, listener.deltas.single().toByteArray(Charsets.UTF_8).size)
    }

    @Test fun closeRejectsFutureWorkAndRetiresQueuedRequest() {
        val queue = Queue(); val native = Native(); val listener = Listener()
        val worker = WearAssistantWorker(queue, native)
        worker.submit("synthetic-1", "/synthetic/auth.json", "{}", listener)
        worker.close()
        assertFalse(worker.submit("synthetic-2", "/synthetic/auth.json", "{}", listener))
        queue.drain()
        assertEquals(0, native.responses)
    }
}
