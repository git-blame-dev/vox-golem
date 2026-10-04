package dev.gitblame.voxgolem.android.wear.capture

import android.content.Context
import dev.gitblame.voxgolem.capture.CaptureFormat
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureRepository as SharedCaptureRepository
import dev.gitblame.voxgolem.capture.CaptureState
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.File
import java.time.Instant
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureRecoveryTest {
    @Test fun interruptedSpoolCanRetryFailedWavFinalizationWithoutBecomingSaved() {
        val context = RuntimeEnvironment.getApplication() as Context
        val shared = SharedCaptureRepository(context)
        val watch = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        val spool = shared.createPcmSpool(id)
        val pcm = byteArrayOf(0, 1, 2, 3, 4, 5, 6, 7)
        spool.writeBytes(pcm)
        val now = Instant.parse("2026-10-04T12:30:00Z").toString()
        shared.save(CaptureRecord(
            captureId = id,
            createdAtUtc = now,
            startedAtUtc = now,
            captureState = CaptureState.INTERRUPTED,
            stopReason = "capture-finalize-error",
            observedFrames = 4,
            durableFrames = 4,
            format = CaptureFormat(),
            localAudioPath = spool.absolutePath,
        ))

        val wav = watch.audio(id)
        assertTrue(wav.parentFile!!.exists())
        val corruptFixture = byteArrayOf(9, 8, 7)
        wav.writeBytes(corruptFixture)
        watch.recoverInterrupted()

        val failed = shared.get(id)!!
        assertEquals(CaptureState.INTERRUPTED, failed.captureState)
        assertEquals(spool.absolutePath, failed.localAudioPath)
        assertEquals(null, failed.sha256)
        assertTrue(spool.isFile)
        assertArrayEquals(corruptFixture, wav.readBytes())

        watch.recoverInterrupted()
        watch.recoverInterrupted()
        watch.recoverInterrupted()
        assertEquals(CaptureState.INTERRUPTED, shared.get(id)!!.captureState)
        assertTrue(spool.isFile)
        assertArrayEquals(corruptFixture, wav.readBytes())

        assertTrue(watch.retryInterruptedFinalization(id))
        val quarantine = wav.parentFile!!.listFiles()!!.single { it.name.startsWith("audio-invalid-") }
        assertArrayEquals(corruptFixture, quarantine.readBytes())

        val recovered = shared.get(id)!!
        assertEquals(id, recovered.captureId)
        assertEquals(CaptureState.INTERRUPTED, recovered.captureState)
        assertEquals("capture-finalize-error", recovered.stopReason)
        assertEquals(4L, recovered.durableFrames)
        assertEquals(52L, recovered.finalizedByteLength)
        assertNotNull(recovered.sha256)
        assertEquals(64, recovered.sha256!!.length)
        assertEquals(wav.absolutePath, recovered.localAudioPath)
        assertEquals("RIFF", String(wav.readBytes(), 0, 4))
        assertFalse(spool.exists())

        watch.recoverInterrupted()
        assertEquals(CaptureState.INTERRUPTED, shared.get(id)!!.captureState)
        assertEquals(id, shared.get(id)!!.captureId)
    }
}
