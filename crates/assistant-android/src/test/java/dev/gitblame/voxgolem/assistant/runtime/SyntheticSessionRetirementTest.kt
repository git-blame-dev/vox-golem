package dev.gitblame.voxgolem.assistant.runtime

import dev.gitblame.voxgolem.assistant.AssistantStore
import dev.gitblame.voxgolem.assistant.AssistantStoreRunner
import dev.gitblame.voxgolem.assistant.AssistantTiming
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.io.File
import java.util.UUID

@RunWith(AssistantStoreRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class SyntheticSessionRetirementTest {
    @Test fun registeredPhoneDirectoryIsExplicitAndCannotAdoptExistingStore() {
        val context = RuntimeEnvironment.getApplication()
        val directory = File(context.cacheDir, "synthetic-phone-ask-${UUID.randomUUID()}").apply { mkdir() }
        val sentinel = directory.resolve("assistant.db").apply { writeText("synthetic") }
        try {
            assertThrows(IllegalStateException::class.java) {
                AssistantRuntime.registeredSyntheticSession(context, directory) { AssistantTiming.begin(context, it) }
            }
            assertEquals("synthetic", sentinel.readText())
        } finally { directory.deleteRecursively() }
    }

    @Test fun registeredPhoneSessionUsesOwnedStoreButOrdinaryCredentialContext() {
        val app = RuntimeEnvironment.getApplication()
        val directory = File(app.cacheDir, "synthetic-phone-ask-${UUID.randomUUID()}").apply { mkdir() }
        val requested = mutableListOf<String>()
        val context = object : android.content.ContextWrapper(app) {
            override fun getApplicationContext(): android.content.Context = this
            override fun getNoBackupFilesDir(): File { requested += "ordinary-no-backup"; return app.noBackupFilesDir }
        }
        val session = AssistantRuntime.registeredSyntheticSession(context, directory) { AssistantTiming.begin(context, it) }
        val executor = Executors.newSingleThreadExecutor()
        try {
            val hydrated = CountDownLatch(1)
            val subscription = session.engine.subscribe { if (it.contextReady) hydrated.countDown() }
            try { assertTrue(hydrated.await(3, TimeUnit.SECONDS)) } finally { subscription.close() }
            assertTrue(directory.resolve("assistant.db").isFile)
            // ProviderAuthSource constructs credential owners from the ordinary application context.
            assertTrue(requested.contains("ordinary-no-backup"))
        } finally {
            try { executor.submit { session.close() }.get(3, TimeUnit.SECONDS) }
            finally { executor.shutdownNow() }
        }
        assertTrue(directory.resolve("assistant.db").isFile)
        directory.deleteRecursively()
    }

    @Test fun failedCredentialConstructionRetainsOwnedDirectoryWithoutPublishingAHandle() {
        val app = RuntimeEnvironment.getApplication()
        val directory = File(app.cacheDir, "synthetic-phone-ask-${UUID.randomUUID()}").apply { mkdir() }
        val context = object : android.content.ContextWrapper(app) {
            override fun getApplicationContext(): android.content.Context = this
            override fun getNoBackupFilesDir(): File = error("synthetic-construction-failure")
        }
        try {
            val failure = assertThrows(IllegalStateException::class.java) {
                AssistantRuntime.registeredSyntheticSession(context, directory) { AssistantTiming.begin(context, it) }
            }
            assertEquals("synthetic-construction-failure", failure.message)
            // Store opens lazily; failure precedes engine hydration, so only directory ownership exists.
            assertTrue(directory.isDirectory)
            assertFalse(directory.resolve("assistant.db").exists())
            assertEquals(0, failure.suppressed.size)
        } finally { directory.deleteRecursively() }
    }

    private fun exercise(timeoutFirst: Boolean) {
        val context = RuntimeEnvironment.getApplication()
        val store = AssistantStore(context)
        val io = Executors.newSingleThreadExecutor()
        val providerExecutor = Executors.newSingleThreadExecutor()
        val closing = Executors.newSingleThreadExecutor()
        val registered = CountDownLatch(1)
        lateinit var retire: () -> Unit
        val provider = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String,
                callback: ProviderDeltaCallback): String =
                """{"status":"complete","text":"Synthetic answer","contentType":"output_text"}"""
            override fun cancel(requestId: String) = Unit
            override fun whenRetired(requestId: String, retired: () -> Unit) {
                retire = retired
                registered.countDown()
            }
        }
        val worker = AssistantRequestWorker(providerExecutor, provider)
        val models = object : AssistantModelStorage {
            override fun read() = "custom-sol-high"
            override fun write(value: String) = Unit
        }
        val engine = AssistantEngine(store, io, worker, { "/synthetic/auth.json" }, models,
            beginTiming = { AssistantTiming.begin(context, it, sink = AssistantTiming.Sink {}) })
        try {
            engine.resume()
            engine.setDraft("Synthetic question")
            engine.send()
            assertTrue(registered.await(2, TimeUnit.SECONDS))
            engine.background()
            worker.close()
            providerExecutor.shutdown()
            assertTrue(providerExecutor.awaitTermination(2, TimeUnit.SECONDS))
            if (timeoutFirst) {
                try {
                    AssistantRuntime.awaitSyntheticRetirement(engine, io, 30, TimeUnit.MILLISECONDS)
                    fail("Retirement must not be inferred from provider executor termination")
                } catch (expected: IllegalStateException) {
                    assertEquals("synthetic_retirement_unconfirmed", expected.message)
                }
                assertFalse(io.isShutdown)
                io.submit { store.snapshot() }.get(2, TimeUnit.SECONDS)
            }
            val waiting = CountDownLatch(1)
            val barrier = closing.submit {
                waiting.countDown()
                AssistantRuntime.awaitSyntheticRetirement(engine, io, 2, TimeUnit.SECONDS)
            }
            assertTrue(waiting.await(2, TimeUnit.SECONDS))
            try {
                barrier.get(30, TimeUnit.MILLISECONDS)
                fail("Remote retirement is still withheld")
            } catch (_: java.util.concurrent.TimeoutException) { }
            assertTrue(engine.snapshot().busy)
            retire()
            barrier.get(2, TimeUnit.SECONDS)
            assertFalse(engine.snapshot().busy)
            assertTrue(engine.snapshot().contextReady)
            io.submit { store.close() }.get(2, TimeUnit.SECONDS)
        } finally {
            worker.close()
            providerExecutor.shutdownNow()
            closing.shutdownNow()
            io.shutdown()
            assertTrue(io.awaitTermination(2, TimeUnit.SECONDS))
        }
    }

    @Test fun providerTaskExitDoesNotSettleActorUntilRemoteRetirement() = exercise(false)
    @Test fun timeoutPreservesActorAndStoreForLateRetirementAndRetry() = exercise(true)

    private fun closeSessionWithRetry(failLaterStage: Boolean) {
        val context = RuntimeEnvironment.getApplication()
        val directory = File(context.cacheDir, "synthetic-ask-${UUID.randomUUID()}")
        val session = AssistantRuntime.syntheticSession(context, directory) { AssistantTiming.begin(context, it) }
        val closing = Executors.newSingleThreadExecutor()
        try {
            closing.submit {
                var laterStageAttempts = 0
                val finishTrial = {
                    session.close()
                    if (failLaterStage && laterStageAttempts++ == 0) error("synthetic_later_cleanup_failed")
                }
                if (failLaterStage) {
                    val laterFailure = runCatching { finishTrial() }.exceptionOrNull()
                    assertEquals("synthetic_later_cleanup_failed", laterFailure?.message)
                } else finishTrial()
                // finishTrial retries the same handle when a subsequent cleanup stage fails.
                finishTrial()
            }.get(3, TimeUnit.SECONDS)
            assertFalse(directory.exists())
        } finally { closing.shutdownNow() }
    }

    @Test fun completeSyntheticSessionCloseIsIdempotent() = closeSessionWithRetry(false)
    @Test fun completeSyntheticSessionCanBeRetriedAfterALaterCleanupStageFails() = closeSessionWithRetry(true)
}
