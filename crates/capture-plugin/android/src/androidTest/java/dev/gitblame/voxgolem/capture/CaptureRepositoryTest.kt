package dev.gitblame.voxgolem.capture

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith
import java.io.FileOutputStream
import java.util.UUID

@RunWith(AndroidJUnit4::class)
class CaptureRepositoryTest {
    private val repository by lazy {
        CaptureRepository(InstrumentationRegistry.getInstrumentation().targetContext)
    }

    @Test
    fun updateIsIdentityPreservingAndAtomicForMissingRows() {
        val id = UUID.randomUUID().toString()
        val saved = repository.save(CaptureRecord(id, createdAtUtc = "2026-10-03T12:00:00Z"))
        assertEquals(saved, repository.save(saved))
        assertThrows(IllegalStateException::class.java) { repository.save(saved.copy(captureState = CaptureState.FAILED)) }
        assertEquals(saved, repository.get(id))
        val changed = repository.update(id) { it.copy(captureState = CaptureState.RECORDING, observedFrames = 9) }
        assertEquals(CaptureState.RECORDING, changed?.captureState)
        assertEquals(9L, repository.get(id)?.observedFrames)
        assertNull(repository.update(UUID.randomUUID().toString()) { it.copy(captureState = CaptureState.FAILED) })
    }

    @Test
    fun processRecoveryTrimsOnlyIncompleteFrameAndMarksRecordInterrupted() {
        val id = UUID.randomUUID().toString()
        val spool = repository.createPcmSpool(id)
        FileOutputStream(spool).use { it.write(byteArrayOf(1, 2, 3, 4, 5)) }
        repository.save(
            CaptureRecord(
                id,
                createdAtUtc = "2026-10-03T12:01:00Z",
                captureState = CaptureState.RECORDING,
                observedFrames = 3,
                localAudioPath = spool.absolutePath,
            ),
        )

        val recovered = repository.recoverInterrupted().single { it.captureId == id }
        assertEquals(CaptureState.INTERRUPTED, recovered.captureState)
        assertEquals(2L, recovered.durableFrames)
        assertEquals(4L, spool.length())
        assertEquals(recovered, repository.get(id))
    }

    @Test
    fun recoveryDoesNotTruncateAnUntrustedPath() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val id = UUID.randomUUID().toString()
        val external = java.io.File(context.cacheDir, "synthetic-untrusted-${UUID.randomUUID()}.pcm")
        external.writeBytes(byteArrayOf(1, 2, 3))
        repository.save(
            CaptureRecord(
                id,
                createdAtUtc = "2026-10-03T12:02:00Z",
                captureState = CaptureState.RECORDING,
                localAudioPath = external.absolutePath,
            ),
        )

        val recovered = repository.recoverInterrupted().single { it.captureId == id }
        assertEquals("invalid-spool-path", recovered.lastErrorCode)
        assertEquals(3L, external.length())
        assertNull(recovered.localAudioPath)
        assertNotNull(repository.get(id))
        external.delete()
    }

    @Test
    fun emptyFinalIsPersistedAsNoSpeechAndNeverBecomesAnOrgTask() {
        val id = UUID.randomUUID().toString()
        repository.save(
            CaptureRecord(
                id,
                createdAtUtc = "2026-10-03T12:03:00Z",
                finishedLocalDate = "2026-10-03",
                captureState = CaptureState.SAVED,
                recognitionState = RecognitionState.FINAL,
                finalText = " \n\t",
                archiveState = ArchiveState.VERIFIED,
                archivedAudioUri = "content://synthetic.provider/audio/$id",
            ),
        )

        val coordinator = OrgExportCoordinator(InstrumentationRegistry.getInstrumentation().targetContext)
        val firstAttempt = coordinator.process(id)
        val secondAttempt = coordinator.process(id)

        assertEquals(OrgState.WAITING_FOR_TRANSCRIPT, firstAttempt.state)
        assertEquals(OrgState.WAITING_FOR_TRANSCRIPT, secondAttempt.state)
        assertEquals(RecognitionState.NO_SPEECH, repository.get(id)?.recognitionState)
        assertNull(repository.get(id)?.exportTranscriptVersion)
    }

    @Test
    fun exportedCaptureReturnsItsPersistedReceiptWithoutReopeningOrPrependingInbox() {
        val id = UUID.randomUUID().toString()
        val exported = repository.save(
            CaptureRecord(
                id,
                createdAtUtc = "2026-10-03T12:04:00Z",
                finishedLocalDate = "2026-10-03",
                captureState = CaptureState.SAVED,
                recognitionState = RecognitionState.FINAL,
                finalText = "synthetic already exported note",
                archiveState = ArchiveState.VERIFIED,
                archivedAudioUri = "content://synthetic.provider/audio/$id",
                orgState = OrgState.EXPORTED,
                exportTranscriptVersion = 1,
                exportDestinationId = "synthetic-destination",
            ),
        )

        val result = OrgExportCoordinator(InstrumentationRegistry.getInstrumentation().targetContext).process(id)

        assertEquals(OrgState.EXPORTED, result.state)
        assertEquals("synthetic-destination", result.destinationId)
        assertEquals(exported, repository.get(id))
    }
}
