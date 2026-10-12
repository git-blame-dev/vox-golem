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
