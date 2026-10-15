package dev.gitblame.voxgolem.assistant.runtime

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.isActive
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import kotlinx.coroutines.withTimeout
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class LegacyGeminiNanoEngineTest {
    private class FakeClient : LegacyNanoClient {
        var prepares = 0
        var closes = 0
        var prepare: suspend () -> Unit = {}
        var drain: suspend () -> Unit = {}
        var stream: suspend kotlinx.coroutines.flow.FlowCollector<String>.() -> Unit = {
            emit("Hello")
            emit(" ")
            emit("synthetic world")
        }
        override suspend fun prepareInferenceEngine() {
            prepares++
            prepare()
        }
        override fun generateContentStream(prompt: String): Flow<String> = flow { stream() }
        override fun close() { closes++ }
        override suspend fun awaitCancellationDispatch() { drain() }
    }

    private fun engine(client: FakeClient) =
        DefaultGeminiNanoEngine({ client }, Dispatchers.Unconfined)

    @Test fun preservesDeltasAndReusesOnePreparedClient() = runBlocking {
        val client = FakeClient()
        var creations = 0
        val engine = DefaultGeminiNanoEngine({ creations++; client }, Dispatchers.Unconfined)
        try {
            repeat(2) {
                assertEquals(listOf("Hello", " ", "synthetic world"), engine.generateContentStream("synthetic prompt").toList())
                engine.prepareInferenceEngine()
            }
            assertEquals(1, creations)
            assertEquals(1, client.prepares)
        } finally { engine.close() }
    }

    @Test fun warmupAndWholeStreamHoldAdmissionUntilCompletion() = runBlocking {
        val client = FakeClient()
        val warmEntered = CompletableDeferred<Unit>()
        val warmRelease = CompletableDeferred<Unit>()
        val streamEntered = CompletableDeferred<Unit>()
        val streamRelease = CompletableDeferred<Unit>()
        client.prepare = { warmEntered.complete(Unit); warmRelease.await() }
        var streams = 0
        client.stream = {
            streams++
            streamEntered.complete(Unit)
            streamRelease.await()
            emit("done")
        }
        val engine = engine(client)
        try {
            val warm = launch { engine.prepareInferenceEngine() }
            warmEntered.await()
            val first = launch { engine.generateContentStream("first synthetic").toList() }
            yield()
            assertEquals(0, streams)
            warmRelease.complete(Unit)
            streamEntered.await()
            val second = launch { engine.generateContentStream("second synthetic").toList() }
            var laterWarmFinished = false
            val laterWarm = launch { engine.prepareInferenceEngine(); laterWarmFinished = true }
            yield()
            assertEquals(1, streams)
            assertFalse(laterWarmFinished)
            streamRelease.complete(Unit)
            warm.join(); first.join(); second.join(); laterWarm.join()
            assertEquals(2, streams)
            assertEquals(1, client.prepares)
        } finally { engine.close() }
    }

    @Test fun failedPreparationIsRetriedWithoutRecreatingClient() = runBlocking {
        val client = FakeClient()
        client.prepare = { throw IllegalStateException("synthetic failure") }
        val engine = engine(client)
        try {
            try {
                engine.prepareInferenceEngine()
                fail("Preparation must fail")
            } catch (_: IllegalStateException) { }
            client.prepare = {}
            engine.prepareInferenceEngine()
            engine.prepareInferenceEngine()
            assertEquals(2, client.prepares)
        } finally { engine.close() }
    }

    @Test fun cancelledPreparationDoesNotCacheWarmth() = runBlocking {
        val client = FakeClient()
        val entered = CompletableDeferred<Unit>()
        client.prepare = {
            entered.complete(Unit)
            try { awaitCancellation() } catch (error: CancellationException) {
                throw IllegalStateException("synthetic SDK wrapper", error)
            }
        }
        val engine = engine(client)
        try {
            var observed: Throwable? = null
            val job = launch {
                try { engine.prepareInferenceEngine() }
                catch (error: Throwable) { observed = error }
            }
            entered.await()
            job.cancelAndJoin()
            assertTrue(observed is CancellationException)
            client.prepare = {}
            engine.prepareInferenceEngine()
            assertEquals(2, client.prepares)
        } finally { engine.close() }
    }

    @Test fun closeCancelsAdmittedStreamAndRetiresClientWithoutCancellingCallerParent() = runBlocking {
        val client = FakeClient()
        val entered = CompletableDeferred<Unit>()
        client.stream = {
            entered.complete(Unit)
            try { awaitCancellation() } catch (error: CancellationException) {
                throw IllegalStateException("synthetic SDK wrapper", error)
            }
        }
        val engine = engine(client)
        var observed: Throwable? = null
        val stream = launch {
            try { engine.generateContentStream("synthetic").toList() }
            catch (error: Throwable) { observed = error }
        }
        entered.await()
        engine.close(); engine.close()
        stream.join()
        assertTrue(observed is CancellationException)
        assertTrue(isActive)
        assertEquals(1, client.closes)
    }

    @Test fun closeCancelsAdmittedPreparationAndRetiresClient() = runBlocking {
        val client = FakeClient()
        val entered = CompletableDeferred<Unit>()
        client.prepare = {
            entered.complete(Unit)
            try { awaitCancellation() } catch (error: CancellationException) {
                throw IllegalStateException("synthetic SDK wrapper", error)
            }
        }
        val engine = engine(client)
        var observed: Throwable? = null
        val preparation = launch {
            try { engine.prepareInferenceEngine() }
            catch (error: Throwable) { observed = error }
        }
        entered.await()
        engine.close(); engine.close()
        preparation.join()
        assertTrue(observed is CancellationException)
        assertTrue(isActive)
        assertEquals(1, client.closes)
    }

    @Test fun closeIsIdempotentAndNeverCreatesOrRecreatesAClient() = runBlocking {
        val client = FakeClient()
        var creations = 0
        val engine = DefaultGeminiNanoEngine({ creations++; client }, Dispatchers.Unconfined)
        engine.prepareInferenceEngine()
        engine.close(); engine.close()
        assertEquals(1, client.closes)
        try {
            engine.generateContentStream("synthetic").toList()
            fail("Closed engine must reject streams")
        } catch (_: IllegalStateException) { }
        assertEquals(1, creations)
        val unopened = DefaultGeminiNanoEngine({ creations++; client }, Dispatchers.Unconfined)
        unopened.close()
        try {
            unopened.prepareInferenceEngine()
            fail("Closed engine must reject preparation")
        } catch (_: IllegalStateException) { }
        assertEquals(1, creations)
    }

    @Test fun queuedCancellationDispatchBlocksSubsequentStreamAndPreparation() = runBlocking {
        withTimeout(5_000) {
            val client = FakeClient()
            val entered = CompletableDeferred<Unit>()
            val queued = CompletableDeferred<Unit>()
            val drainEntered = CompletableDeferred<Unit>()
            val dispatchRelease = CompletableDeferred<Unit>()
            client.stream = {
                entered.complete(Unit)
                try { awaitCancellation() } catch (error: CancellationException) {
                    queued.complete(Unit)
                    throw IllegalStateException("synthetic queued cancellation", error)
                }
            }
            client.drain = {
                assertTrue(queued.isCompleted)
                drainEntered.complete(Unit)
                dispatchRelease.await()
            }
            val engine = engine(client)
            val first = launch { engine.generateContentStream("synthetic first").toList() }
            entered.await()
            first.cancel()
            drainEntered.await()
            var secondStarted = false
            var preparationCompleted = false
            client.stream = { secondStarted = true; emit("next") }
            val second = launch { engine.generateContentStream("synthetic second").toList() }
            val preparation = launch { engine.prepareInferenceEngine(); preparationCompleted = true }
            yield()
            assertFalse(secondStarted)
            assertFalse(preparationCompleted)
            assertFalse(first.isCompleted)
            dispatchRelease.complete(Unit)
            first.join(); second.join(); preparation.join()
            assertTrue(secondStarted)
            assertTrue(preparationCompleted)
            engine.close()
            assertEquals(1, client.closes)
        }
    }

    @Test fun closeCannotCloseClientBeforeQueuedCancellationDispatch() = runBlocking {
        withTimeout(5_000) {
            val client = FakeClient()
            val entered = CompletableDeferred<Unit>()
            val drainEntered = CompletableDeferred<Unit>()
            val dispatchRelease = CompletableDeferred<Unit>()
            client.prepare = { entered.complete(Unit); awaitCancellation() }
            client.drain = { drainEntered.complete(Unit); dispatchRelease.await() }
            val engine = engine(client)
            val operation = launch { engine.prepareInferenceEngine() }
            entered.await()
            engine.close()
            drainEntered.await()
            assertEquals(0, client.closes)
            assertFalse(operation.isCompleted)
            dispatchRelease.complete(Unit)
            operation.join()
            assertEquals(1, client.closes)
            assertTrue(isActive)
        }
    }

    @Test fun downstreamFailureDrainsQueuedCancellationBeforeNextAdmission() = runBlocking {
        withTimeout(5_000) {
            val client = FakeClient()
            val queued = CompletableDeferred<Unit>()
            val drainEntered = CompletableDeferred<Unit>()
            val dispatchRelease = CompletableDeferred<Unit>()
            var producerWasActive = false
            client.stream = {
                try { emit("synthetic first chunk") }
                finally {
                    producerWasActive = currentCoroutineContext().isActive
                    queued.complete(Unit)
                }
            }
            client.drain = {
                assertTrue(queued.isCompleted)
                drainEntered.complete(Unit)
                dispatchRelease.await()
            }
            val engine = engine(client)
            var observed: Throwable? = null
            val first = launch(Dispatchers.Unconfined) {
                try {
                    engine.generateContentStream("synthetic first").collect {
                        throw IllegalArgumentException("synthetic downstream failure")
                    }
                } catch (failure: IllegalArgumentException) { observed = failure }
            }
            try {
                assertTrue(producerWasActive)
                assertTrue("Exceptional exit must dispatch cancellation even while its job is active", drainEntered.isCompleted)
                assertFalse(first.isCompleted)
                var secondStarted = false
                var prepared = false
                client.stream = { secondStarted = true; emit("synthetic next") }
                val second = launch { engine.generateContentStream("synthetic second").toList() }
                val preparation = launch { engine.prepareInferenceEngine(); prepared = true }
                yield()
                assertFalse(secondStarted)
                assertFalse(prepared)
                dispatchRelease.complete(Unit)
                first.join(); second.join(); preparation.join()
                assertTrue(observed is IllegalArgumentException)
                assertTrue(secondStarted)
                assertTrue(prepared)
            } finally {
                dispatchRelease.complete(Unit)
                first.join()
                engine.close()
            }
        }
    }

    @Test fun failedDispatchPoisonsReuseAndDoesNotSilentlyCloseClient() = runBlocking {
        withTimeout(5_000) {
            val client = FakeClient()
            val entered = CompletableDeferred<Unit>()
            client.prepare = { entered.complete(Unit); awaitCancellation() }
            client.drain = { throw IllegalStateException("synthetic dispatch failure") }
            val engine = engine(client)
            val operation = launch { engine.prepareInferenceEngine() }
            entered.await()
            operation.cancelAndJoin()
            try {
                engine.prepareInferenceEngine()
                fail("Failed dispatch must prohibit reuse")
            } catch (_: IllegalStateException) { }
            assertEquals(1, client.prepares)
            engine.close()
            assertEquals(0, client.closes)
        }
    }

    @Test fun downstreamFailureWithFailedDispatchProhibitsReuse() = runBlocking {
        withTimeout(5_000) {
            val client = FakeClient()
            client.stream = { emit("synthetic first chunk") }
            client.drain = { throw IllegalStateException("synthetic dispatch failure") }
            val engine = engine(client)
            try {
                kotlinx.coroutines.withContext(Dispatchers.Unconfined) {
                    engine.generateContentStream("synthetic first").collect {
                        throw IllegalArgumentException("synthetic downstream failure")
                    }
                }
                fail("Downstream failure must propagate")
            } catch (_: IllegalArgumentException) { }
            try {
                engine.prepareInferenceEngine()
                fail("Failed dispatch must prohibit reuse")
            } catch (_: IllegalStateException) { }
            assertEquals(1, client.prepares)
            engine.close()
            assertEquals(0, client.closes)
        }
    }
}
