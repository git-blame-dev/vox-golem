package dev.gitblame.voxgolem.android.wear.capture

import android.content.Context
import android.content.Intent
import dev.gitblame.voxgolem.capture.CaptureFormat
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureRepository as SharedCaptureRepository
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.PcmRecordingWriter
import dev.gitblame.voxgolem.capture.RecognitionState
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Robolectric
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.File
import java.io.FileOutputStream
import java.security.MessageDigest
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.time.Instant
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureRecoveryTest {
    @Test fun automaticAndExplicitRepairSkipAnInterruptedRecordStillOwnedByARetiringWriter() {
        val context = RuntimeEnvironment.getApplication()
        val watch = CaptureRepository(context)
        val record = watch.create(16_000, UUID.randomUUID().toString())
        val owner = CaptureLifetime.tryAcquire()!!
        owner.bindCapture(record.captureId)
        val writerHold = owner.retain()
        val pcm = byteArrayOf(1, 0, 2, 0)
        val writer = PcmWavWriter(watch.pcm(record.captureId), watch.audio(record.captureId), 16_000)
        try {
            writer.append(pcm)
            writer.checkpoint()
            watch.save(record.copy(captureState = CaptureState.INTERRUPTED))
            val interrupted = watch.get(record.captureId)!!
            owner.close()
            watch.repairInterruptedAudio()
            assertFalse(watch.retryInterruptedFinalization(record.captureId))
            assertEquals(interrupted, watch.get(record.captureId))
            assertArrayEquals(pcm, watch.pcm(record.captureId).readBytes())
            assertFalse(watch.audio(record.captureId).exists())
        } finally {
            writer.close()
            owner.close()
            writerHold.close()
        }
        assertTrue(watch.retryInterruptedFinalization(record.captureId))
        assertArrayEquals(pcm, watch.audio(record.captureId).readBytes().drop(44).toByteArray())
        assertFalse(watch.pcm(record.captureId).exists())
    }

    @Test fun admissionRecoveryKeepsOwnedAlignedSpoolAndDefersWavRepairUntilRequested() {
        val context = RuntimeEnvironment.getApplication()
        val watch = CaptureRepository(context)
        val abandoned = watch.create(16_000, UUID.randomUUID().toString())
        val pcm = byteArrayOf(1, 0, 2, 0)
        watch.pcm(abandoned.captureId).writeBytes(pcm + byteArrayOf(9))
        val historical = watch.create(16_000, UUID.randomUUID().toString())
        watch.save(historical.copy(captureState = CaptureState.INTERRUPTED))
        watch.pcm(historical.captureId).writeBytes(pcm)

        watch.recoverForAdmission()

        assertEquals(null, watch.active())
        val recovered = watch.get(abandoned.captureId)!!
        assertEquals(CaptureState.INTERRUPTED, recovered.captureState)
        assertEquals(2L, recovered.durableFrames)
        assertEquals("process-recovery", recovered.stopReason)
        assertArrayEquals(pcm, watch.pcm(abandoned.captureId).readBytes())
        for (id in listOf(abandoned.captureId, historical.captureId)) {
            assertFalse(watch.audio(id).exists())
            assertEquals(null, watch.get(id)!!.finalizedByteLength)
        }

        watch.repairInterruptedAudio()

        for (id in listOf(abandoned.captureId, historical.captureId)) {
            assertEquals(CaptureState.INTERRUPTED, watch.get(id)!!.captureState)
            assertEquals(48L, watch.get(id)!!.finalizedByteLength)
            assertArrayEquals(pcm, watch.audio(id).readBytes().drop(44).toByteArray())
            assertFalse(watch.pcm(id).exists())
            assertFalse(watch.claimCompletionFeedback(id))
        }
    }

    @Test fun nonthreadedStopAndServiceDestructionPreserveDraftAndFinalizedAudio() {
        val context = RuntimeEnvironment.getApplication() as Context
        val watch = CaptureRepository(context)
        val pcm = byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8)
        for (reason in listOf("manual_stop", "service_destroyed")) {
            val service = Robolectric.buildService(CaptureService::class.java).create().get()
            assertTrue(service.session.prepareRecovery())
            val sessionId = UUID.randomUUID().toString()
            val record = watch.create(16_000, sessionId).copy(captureState = CaptureState.RECORDING)
            watch.save(record)
            service.session.bindCaptureLifetime(record.captureId)
            val writer = PcmWavWriter(watch.pcm(record.captureId), watch.audio(record.captureId), 16_000)
            try {
                writer.append(pcm)
                writer.checkpoint()
                RecordingSession::class.java.getDeclaredField("record").apply { isAccessible = true }.set(service.session, record)
                RecordingSession::class.java.getDeclaredField("writer").apply { isAccessible = true }.set(service.session, writer)
                watch.updateTranscript(record.captureId, sessionId, 1, "synthetic visible draft", false)
                CaptureRepository(context).updateTranscript(record.captureId, sessionId, 2, "", false)

                if (reason == "service_destroyed") service.onDestroy()
                else service.onStartCommand(Intent(CaptureService.ACTION_STOP)
                    .putExtra(CaptureService.EXTRA_STOP_REASON, reason), 0, 1)

                val restored = CaptureRepository(context).get(record.captureId)!!
                assertEquals(if (reason == "manual_stop") CaptureState.SAVED else CaptureState.INTERRUPTED, restored.captureState)
                assertEquals(reason, restored.stopReason)
                assertEquals("synthetic visible draft", restored.previewText)
                assertEquals(null, restored.finalText)
                assertEquals(4L, restored.durableFrames)
                val wav = File(restored.localAudioPath!!).readBytes()
                assertEquals("RIFF", String(wav, 0, 4, Charsets.US_ASCII))
                assertArrayEquals(pcm, wav.copyOfRange(44, wav.size))
                assertFalse(watch.pcm(record.captureId).exists())
                assertFalse(restored.completionNotified)
                assertFalse(watch.claimCompletionFeedback(record.captureId))
            } finally {
                service.onDestroy()
                writer.close()
            }
        }
    }

    @Test fun processRecoveryKeepsDraftAndCheckpointedAudioWhenRecordingOrStoppingWasInterrupted() {
        val context = RuntimeEnvironment.getApplication() as Context
        val shared = SharedCaptureRepository(context)
        val watch = CaptureRepository(context)
        for (state in listOf(CaptureState.RECORDING, CaptureState.STOPPING)) {
            val sessionId = UUID.randomUUID().toString()
            val created = watch.create(16_000, sessionId)
            val pcm = byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8)
            val checkpoint = PcmRecordingWriter(watch.pcm(created.captureId), created.format).use { writer ->
                writer.append(pcm)
                writer.checkpoint()
            }
            watch.save(created.copy(
                captureState = state, observedFrames = checkpoint.frameCount, durableFrames = checkpoint.frameCount,
                stopReason = if (state == CaptureState.STOPPING) "service_destroyed" else null,
            ))
            watch.updateTranscript(created.captureId, sessionId, 1, "synthetic visible draft", false)
            // A later torn write must not damage the checkpoint's complete frames.
            FileOutputStream(watch.pcm(created.captureId), true).use { it.write(byteArrayOf(9)) }
            CaptureRepository(context).updateTranscript(created.captureId, sessionId, 2, "", false)

            val restarted = CaptureRepository(context)
            restarted.recoverInterrupted()
            val recovered = restarted.get(created.captureId)!!
            assertEquals(CaptureState.INTERRUPTED, recovered.captureState)
            assertEquals("synthetic visible draft", recovered.previewText)
            assertEquals(null, recovered.finalText)
            assertEquals(RecognitionState.RUNNING, recovered.recognitionState)
            assertEquals(4L, recovered.durableFrames)
            assertEquals(52L, recovered.finalizedByteLength)
            assertEquals(if (state == CaptureState.STOPPING) "service_destroyed" else "process-recovery", recovered.stopReason)
            val wav = File(recovered.localAudioPath!!)
            val waveBytes = wav.readBytes()
            assertEquals("RIFF", String(waveBytes, 0, 4, Charsets.US_ASCII))
            assertEquals("WAVEfmt ", String(waveBytes, 8, 8, Charsets.US_ASCII))
            assertEquals("data", String(waveBytes, 36, 4, Charsets.US_ASCII))
            val header = ByteBuffer.wrap(waveBytes).order(ByteOrder.LITTLE_ENDIAN)
            assertEquals(waveBytes.size - 8, header.getInt(4))
            assertEquals(16, header.getInt(16))
            assertEquals(1, header.getShort(20).toInt())
            assertEquals(1, header.getShort(22).toInt())
            assertEquals(16_000, header.getInt(24))
            assertEquals(32_000, header.getInt(28))
            assertEquals(2, header.getShort(32).toInt())
            assertEquals(16, header.getShort(34).toInt())
            assertEquals(pcm.size, header.getInt(40))
            assertArrayEquals(pcm, waveBytes.copyOfRange(44, waveBytes.size))
            assertEquals(MessageDigest.getInstance("SHA-256").digest(waveBytes)
                .joinToString("") { "%02x".format(it) }, recovered.sha256)
            assertFalse(watch.pcm(created.captureId).exists())
            assertFalse(restarted.claimCompletionFeedback(created.captureId))
            assertTrue(restarted.pendingTransfers().any { it.captureId == created.captureId })
            restarted.recoverInterrupted()
            assertEquals(recovered, shared.get(created.captureId))
        }
    }

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
        assertEquals("wav-finalization-retry-limit", shared.get(id)!!.lastErrorCode)
        CaptureRepository(context).recoverInterrupted()
        assertEquals("wav-finalization-retry-limit", shared.get(id)!!.lastErrorCode)

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

    @Test fun explicitRepairRejectsEveryNonInterruptedStateWithoutChangingMetadataOrAudio() {
        val context = RuntimeEnvironment.getApplication()
        val shared = SharedCaptureRepository(context)
        val watch = CaptureRepository(context)
        for (state in CaptureState.entries.filter { it != CaptureState.INTERRUPTED }) {
            val id = UUID.randomUUID().toString()
            val pcm = shared.createPcmSpool(id)
            val bytes = byteArrayOf(1, 0, 2, 0)
            pcm.writeBytes(bytes)
            val record = shared.save(CaptureRecord(id, createdAtUtc = "2026-10-05T12:30:00Z",
                captureState = state, localAudioPath = pcm.absolutePath))
            assertFalse(CaptureRepository(context).retryInterruptedFinalization(id))
            assertEquals(record, shared.get(id))
            assertArrayEquals(bytes, pcm.readBytes())
            assertFalse(watch.audio(id).exists())
        }
        assertFalse(watch.retryInterruptedFinalization(UUID.randomUUID().toString()))
    }

    @Test fun recoveryUsesOnlyOwnedSpoolAndNeverTruncatesAnUnexpectedRecordedPath() {
        val context = RuntimeEnvironment.getApplication()
        val shared = SharedCaptureRepository(context)
        val watch = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        val outside = File(context.cacheDir, "synthetic-unowned-audio.pcm.part")
        val unownedBytes = byteArrayOf(1, 2, 3)
        outside.writeBytes(unownedBytes)
        val ownedBytes = byteArrayOf(1, 0, 2, 0)
        shared.createPcmSpool(id).writeBytes(ownedBytes)
        shared.save(CaptureRecord(id, createdAtUtc = "2026-10-05T12:30:00Z",
            captureState = CaptureState.RECORDING, localAudioPath = outside.absolutePath))
        CaptureRepository(context).recoverInterrupted()
        val recovered = watch.get(id)!!
        assertEquals(CaptureState.INTERRUPTED, recovered.captureState)
        assertEquals(watch.audio(id).absolutePath, recovered.localAudioPath)
        assertEquals(2L, recovered.durableFrames)
        assertArrayEquals(ownedBytes, watch.audio(id).readBytes().drop(44).toByteArray())
        assertArrayEquals(unownedBytes, outside.readBytes())
        assertFalse(watch.claimCompletionFeedback(id))
    }
}
