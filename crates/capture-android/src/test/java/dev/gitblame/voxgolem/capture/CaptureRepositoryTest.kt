package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.After
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.File

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class CaptureRepositoryTest {
    private val context = RuntimeEnvironment.getApplication()
    private val repositories = mutableListOf<CaptureRepository>()
    private fun openRepository() = CaptureRepository(context).also { repositories.add(it) }
    private val repository = openRepository()
    private val id = "123e4567-e89b-42d3-a456-426614174000"

    @After fun closeTestDatabases() {
        val helper = CaptureRepository::class.java.getDeclaredField("helper").apply { isAccessible = true }
        repositories.forEach { (helper.get(it) as android.database.sqlite.SQLiteOpenHelper).close() }
    }

    @Test fun fullRecordRoundTripsAcrossRepositoryInstancesWithoutChangingPersistedFields() {
        val record = CaptureRecord(
            captureId = id, createdAtUtc = "2026-01-02T03:04:05Z", startedAtUtc = "2026-01-02T03:04:06Z",
            finishedAtUtc = "2026-01-02T03:04:07Z", finishedLocalDate = "2026-01-02", timezoneId = "UTC",
            captureState = CaptureState.SAVED, stopReason = "synthetic-stop", observedFrames = 12, durableFrames = 12,
            format = CaptureFormat(48_000, 2, 16), localAudioPath = "/synthetic/audio.wav",
            archivedAudioUri = "content://synthetic.provider/audio", finalizedByteLength = 92, sha256 = "a".repeat(64),
            recognitionState = RecognitionState.FINAL, recognitionSessionId = "synthetic-session", lastRevision = 3,
            previewText = "synthetic preview", finalText = "synthetic final", engineId = "synthetic-engine", language = "en",
            transcriptVersion = 2, exportTranscriptVersion = 2, transferState = TransferState.PHONE_ARCHIVED,
            archiveState = ArchiveState.VERIFIED, orgState = OrgState.EXPORTED, exportDestinationId = "synthetic-destination",
            lastErrorStage = "synthetic-stage", lastErrorCode = "synthetic-code", retryCount = 1,
            nextRetryAtUtc = "2026-01-02T03:05:00Z", completionNotified = true, attentionNotifiedCode = "synthetic-attention",
            purpose = CapturePurpose.NOTE, requestedLanguage = "en-AU", speechNetworkFallback = true,
            liveTranscriptionState = LiveTranscriptionState.TRANSCRIBING, liveStatusRevision = 4,
            canTranscribeLive = true, liveErrorCode = "synthetic-status",
        )
        repository.save(record)
        assertEquals(record, openRepository().get(id))
        assertEquals(record, repository.list().single())
        assertNull(repository.active())
        assertEquals(1, context.openOrCreateDatabase("captures.db", 0, null).use { it.version })
    }

    @Test fun legacyBodiesRemainLocalOnlyNotesAndCapturePolicyCannotBeChangedByAnUpdate() {
        repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z"))
        context.openOrCreateDatabase("captures.db", 0, null).use { db ->
            val body = db.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(id)).use {
                it.moveToFirst(); org.json.JSONObject(it.getString(0))
            }
            listOf("purpose", "requestedLanguage", "speechNetworkFallback", "liveTranscriptionState",
                "liveStatusRevision", "canTranscribeLive", "liveErrorCode").forEach(body::remove)
            db.execSQL("UPDATE captures SET body = ? WHERE capture_id = ?", arrayOf(body.toString(), id))
        }
        val legacy = openRepository().get(id)!!
        assertEquals(CapturePurpose.NOTE, legacy.purpose)
        assertNull(legacy.requestedLanguage)
        assertFalse(legacy.speechNetworkFallback)
        assertEquals(LiveTranscriptionState.NOT_REQUESTED, legacy.liveTranscriptionState)
        assertThrows(IllegalArgumentException::class.java) { repository.update(id) { it.copy(purpose = CapturePurpose.ASK) } }
        assertThrows(IllegalArgumentException::class.java) { repository.update(id) { it.copy(speechNetworkFallback = true) } }
        assertEquals(legacy, repository.get(id))
    }

    @Test fun updatePreservesIdentityAndRollsBackRejectedTransforms() {
        val record = repository.save(CaptureRecord(id.uppercase(), createdAtUtc = "2026-01-02T03:04:05Z"))
        assertEquals(id, record.captureId)
        assertEquals(record, repository.save(record))
        assertThrows(IllegalStateException::class.java) { repository.save(record.copy(captureState = CaptureState.FAILED)) }
        assertThrows(IllegalArgumentException::class.java) {
            repository.update(id) { it.copy(captureId = "123e4567-e89b-42d3-a456-426614174001") }
        }
        assertEquals(record, repository.get(id))
        val changed = repository.update(id) { it.copy(captureState = CaptureState.RECORDING, observedFrames = 9) }
        assertEquals(changed, repository.active())
        assertEquals(9L, openRepository().get(id)?.observedFrames)
        assertNull(repository.update("123e4567-e89b-42d3-a456-426614174002") { error("must not run") })
    }

    @Test fun recoveryTrimsIncompleteFramesAndPersistsOnlyActiveRecords() {
        val spool = repository.createPcmSpool(id).apply { writeBytes(byteArrayOf(1, 2, 3, 4, 5)) }
        repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.RECORDING,
            observedFrames = 3, localAudioPath = spool.absolutePath))
        val saved = repository.save(CaptureRecord("123e4567-e89b-42d3-a456-426614174001",
            createdAtUtc = "2026-01-02T03:04:06Z", captureState = CaptureState.SAVED))
        val recovered = repository.recoverInterrupted().single()
        assertEquals(CaptureState.INTERRUPTED, recovered.captureState)
        assertEquals(2L, recovered.durableFrames)
        assertEquals(3L, recovered.observedFrames)
        assertEquals(4L, spool.length())
        assertEquals("process-recovery", recovered.stopReason)
        assertEquals(recovered, openRepository().get(id))
        assertEquals(saved, repository.get(saved.captureId))
        assertNull(repository.active())
        assertTrue(repository.recoverInterrupted().isEmpty())
    }

    @Test fun recoveryRejectsUntrustedSpoolPathWithoutModifyingItsAudio() {
        val unrelated = File(context.cacheDir, "synthetic-untrusted.pcm").apply { writeBytes(byteArrayOf(1, 2, 3)) }
        repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.STOPPING,
            stopReason = "synthetic-interruption", localAudioPath = unrelated.absolutePath))
        val recovered = repository.recoverInterrupted().single()
        assertEquals("invalid-spool-path", recovered.lastErrorCode)
        assertEquals("synthetic-interruption", recovered.stopReason)
        assertNull(recovered.localAudioPath)
        assertEquals(0L, recovered.durableFrames)
        assertArrayEquals(byteArrayOf(1, 2, 3), unrelated.readBytes())
    }
}
