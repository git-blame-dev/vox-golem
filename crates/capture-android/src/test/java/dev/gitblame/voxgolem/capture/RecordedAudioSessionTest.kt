package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test
import org.junit.After
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class RecordedAudioSessionTest {
    private val context = RuntimeEnvironment.getApplication()
    private val repositories = mutableListOf<CaptureRepository>()
    private fun store() = RecordedAudioStore(context, "watch").also {
        val field = RecordedAudioStore::class.java.getDeclaredField("repository").apply { isAccessible = true }
        repositories += field.get(it) as CaptureRepository
    }
    private fun repository() = CaptureRepository(context).also { repositories += it }
    @After fun closeDatabases() {
        val field = CaptureRepository::class.java.getDeclaredField("helper").apply { isAccessible = true }
        repositories.forEach { (field.get(it) as android.database.sqlite.SQLiteOpenHelper).close() }
    }
    private class Input : RecordedAudioSession.PcmInput {
        var starts = 0
        var closes = 0
        override fun start() { starts++ }
        override fun read(bytes: ByteArray): Int { bytes[0] = 1; bytes[1] = 0; return 2 }
        override fun stop() {}
        override fun close() { closes++ }
    }
    @Test fun preparationDoesNotBlockStartAndIdentityPrecedesMicrophoneFramesAndReadiness() {
        val store = store(); val entered = CountDownLatch(1); val allow = CountDownLatch(1)
        val returned = CountDownLatch(1); val ended = CountDownLatch(1)
        val events = java.util.concurrent.CopyOnWriteArrayList<String>()
        val caller = java.util.concurrent.Executors.newSingleThreadExecutor()
        val failure = AtomicReference<Throwable?>()
        val callerThread = AtomicReference<Thread>()
        val preparationThread = AtomicReference<Thread>()
        val session = RecordedAudioSession(store, CapturePurpose.TALK, "en-US", false,
            inputFactory = { events += "input"; Input() },
            onIdentity = { events += "identity" }, prepare = {
                preparationThread.set(Thread.currentThread()); entered.countDown()
                check(allow.await(5, TimeUnit.SECONDS))
                store.open(CapturePurpose.TALK, "en-US", false)
            })
        try {
            caller.submit {
                callerThread.set(Thread.currentThread())
                try { session.start({ events += "pcm"; true }, { events += "ready"; session.finish() }, { ended.countDown() }) }
                catch (error: Throwable) { failure.set(error) }
                finally { returned.countDown() }
            }
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            assertTrue("Start must return while preparation remains blocked", returned.await(1, TimeUnit.SECONDS))
            assertTrue(events.isEmpty()); assertEquals(1L, ended.count)
            assertNotSame(callerThread.get(), preparationThread.get())
        } finally {
            allow.countDown(); caller.shutdown(); assertTrue(caller.awaitTermination(3, TimeUnit.SECONDS))
            assertTrue(ended.await(3, TimeUnit.SECONDS)); session.close()
        }
        assertNull(failure.get())
        assertEquals(listOf("identity", "input", "pcm", "ready"), events)
        assertEquals(1L, repository().list().single().durableFrames)
    }

    @Test fun cancellationDuringPreparationKeepsWorkerOwnedAndFinishesOpenedArtifactWithoutMicrophone() {
        val store = store(); val entered = CountDownLatch(1); val allow = CountDownLatch(1)
        val returned = CountDownLatch(1); val ended = CountDownLatch(1)
        val terminals = java.util.concurrent.atomic.AtomicInteger()
        val result = AtomicReference<Result<RecordedAudioArtifact>>()
        val caller = java.util.concurrent.Executors.newSingleThreadExecutor()
        val session = RecordedAudioSession(store, CapturePurpose.ASK, "en-US", false,
            inputFactory = { error("Cancelled preparation must not allocate a microphone") }, prepare = {
                entered.countDown(); check(allow.await(5, TimeUnit.SECONDS))
                store.open(CapturePurpose.ASK, "en-US", false)
            })
        try {
            caller.submit {
                try { session.start({ error("No PCM") }, { error("No readiness") }, {
                    terminals.incrementAndGet(); result.set(it); ended.countDown()
                }) } finally { returned.countDown() }
            }
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            assertTrue(returned.await(1, TimeUnit.SECONDS))
            val pending = assertThrows(IllegalStateException::class.java) { session.close() }
            assertEquals("recording-retirement-pending", pending.message)
            assertEquals(1L, ended.count)
        } finally {
            allow.countDown(); caller.shutdown(); assertTrue(caller.awaitTermination(3, TimeUnit.SECONDS))
            assertTrue(ended.await(3, TimeUnit.SECONDS)); session.close()
        }
        assertEquals(1, terminals.get())
        assertEquals(0L, result.get().getOrThrow().manifest.frameCount)
        assertEquals("cancelled", repository().list().single().stopReason)
    }

    @Test fun preparationFailureIsOneAsynchronousFailureWithoutArtifactOrReadiness() {
        val failure = java.io.IOException("synthetic preparation failure")
        val ended = CountDownLatch(1); val terminals = java.util.concurrent.atomic.AtomicInteger()
        val result = AtomicReference<Result<RecordedAudioArtifact>>()
        val session = RecordedAudioSession(store(), CapturePurpose.TALK, "en-US", false,
            inputFactory = { error("No microphone after preparation failure") }, prepare = { throw failure })
        try {
            session.start({ error("No PCM") }, { error("No readiness") }, {
                result.set(it); terminals.incrementAndGet(); ended.countDown()
            })
            assertTrue(ended.await(2, TimeUnit.SECONDS))
            assertSame(failure, result.get().exceptionOrNull()); assertEquals(1, terminals.get())
            assertTrue(repository().list().isEmpty())
        } finally { session.close() }
    }

    @Test fun cancellationBeforePreparationSkipsStorageAndMicrophoneButStillReportsTerminalFailure() {
        val entered = CountDownLatch(1); val allow = CountDownLatch(1); val ended = CountDownLatch(1)
        val result = AtomicReference<Result<RecordedAudioArtifact>>()
        val session = RecordedAudioSession(store(), CapturePurpose.TALK, "en-US", false,
            inputFactory = { error("No microphone after cancellation") }, clock = {
                entered.countDown(); check(allow.await(5, TimeUnit.SECONDS)); 0L
            })
        try {
            session.start({ error("No PCM") }, { error("No readiness") }, { result.set(it); ended.countDown() })
            assertTrue(entered.await(2, TimeUnit.SECONDS)); session.finish()
        } finally { allow.countDown() }
        assertTrue(ended.await(2, TimeUnit.SECONDS)); session.close()
        assertTrue(result.get().isFailure); assertTrue(repository().list().isEmpty())
    }

    @Test fun identityFailureFinishesTheOpenedRecordingAndReportsOneFailureWithoutMicrophone() {
        val ended = CountDownLatch(1); val result = AtomicReference<Result<RecordedAudioArtifact>>()
        val failure = IllegalStateException("synthetic identity failure")
        val session = RecordedAudioSession(store(), CapturePurpose.ASK, "en-US", false,
            inputFactory = { error("No microphone before identity binding") }, onIdentity = { throw failure })
        try {
            session.start({ error("No PCM") }, { error("No readiness") }, { result.set(it); ended.countDown() })
            assertTrue(ended.await(2, TimeUnit.SECONDS)); assertSame(failure, result.get().exceptionOrNull())
            val record = repository().list().single()
            assertEquals(0L, record.durableFrames); assertEquals("recording-failed", record.stopReason)
        } finally { session.close() }
    }
    @Test fun oneMicrophoneTeeWritesEveryAcceptedFrameBeforePublishingTheSavedArtifact() {
        val microphone = Input()
        var created = 0
        val session = RecordedAudioSession(store(), CapturePurpose.TALK, "en-US", false,
            { created++; microphone })
        val ended = CountDownLatch(1)
        val result = AtomicReference<Result<RecordedAudioArtifact>>()
        var chunks = 0
        session.start({ if (++chunks == 4) session.finish(); true }, {}, { result.set(it); ended.countDown() })
        assertTrue(ended.await(2, TimeUnit.SECONDS)); session.close()
        val artifact = result.get().getOrThrow()
        assertEquals(1, created); assertEquals(1, microphone.starts); assertEquals(1, microphone.closes)
        assertEquals(4L, artifact.manifest.frameCount)
        assertEquals(CaptureState.SAVED, repository().get(artifact.captureId)!!.captureState)
    }
    @Test fun backpressureRetainsAudioButReportsFailureInsteadOfSuccessfulRecognition() {
        val session = RecordedAudioSession(store(), CapturePurpose.ASK, "en-US", false, { Input() })
        val ended = CountDownLatch(1)
        val result = AtomicReference<Result<RecordedAudioArtifact>>()
        session.start({ false }, {}, { result.set(it); ended.countDown() })
        assertTrue(ended.await(2, TimeUnit.SECONDS)); session.close()
        assertTrue(result.get().isFailure)
        val record = repository().list().single()
        assertEquals(CaptureState.SAVED, record.captureState); assertEquals(1L, record.durableFrames)
        assertEquals("recording-failed", record.stopReason)
    }
    @Test fun permissionRevocationAtMicrophoneStartReportsFailureAndRetiresTheAllocatedInput() {
        var closes = 0
        var reads = 0
        val microphone = object : RecordedAudioSession.PcmInput {
            override fun start() { throw SecurityException("Synthetic permission revocation") }
            override fun read(bytes: ByteArray): Int { reads++; return 2 }
            override fun stop() {}
            override fun close() { closes++ }
        }
        val session = RecordedAudioSession(store(), CapturePurpose.ASK, "en-AU", false, { microphone })
        val ended = CountDownLatch(1)
        val result = AtomicReference<Result<RecordedAudioArtifact>>()
        session.start({ error("Revoked permission must not publish PCM") },
            { error("Revoked permission must not publish readiness") }, { result.set(it); ended.countDown() })
        assertTrue(ended.await(2, TimeUnit.SECONDS)); session.close()
        assertTrue(result.get().exceptionOrNull() is SecurityException)
        assertEquals(0, reads); assertEquals(1, closes)
        val record = repository().list().single()
        assertEquals(0L, record.durableFrames); assertEquals("recording-failed", record.stopReason)
    }
}
