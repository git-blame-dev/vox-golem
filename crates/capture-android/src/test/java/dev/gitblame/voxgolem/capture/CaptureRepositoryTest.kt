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
        assertEquals(2, context.openOrCreateDatabase("captures.db", 0, null).use { it.version })
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

    @Test fun interruptedArchiveEligibilityAndRetryDeadlineSurviveReopenAndRejectedUpdates() {
        val interrupted = CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z",
            captureState = CaptureState.INTERRUPTED, localAudioPath = "/synthetic/audio.wav",
            finalizedByteLength = 48, sha256 = "a".repeat(64), archiveState = ArchiveState.STAGED,
            recognitionState = RecognitionState.UNAVAILABLE)
        repository.save(interrupted)
        assertEquals(listOf(interrupted), repository.dueNoteArchives(0))
        repository.deferNoteArchive(id, 100, false)
        assertTrue(openRepository().dueNoteArchives(5_099).isEmpty())
        assertEquals(listOf(interrupted), openRepository().dueNoteArchives(5_100))
        assertThrows(IllegalArgumentException::class.java) {
            repository.update(id) { it.copy(purpose = CapturePurpose.ASK, archiveState = ArchiveState.VERIFIED) }
        }
        assertEquals(5_100L, repository.earliestNoteArchiveDue())
        repository.deferNoteArchive(id, 200, true)
        assertTrue(openRepository().dueNoteArchives(10_000).isEmpty())
        repository.wakeNoteArchives()
        assertEquals(listOf(interrupted), openRepository().dueNoteArchives(0))
        repository.update(id) { it.copy(archiveState = ArchiveState.VERIFIED, archivedAudioUri = "content://synthetic/audio") }
        assertTrue(openRepository().dueNoteArchives(Long.MAX_VALUE).isEmpty())
    }

    @Test fun transferPagesAreBoundedDueOrderedAndMoveFailedOldestBehindUntouchedWork() {
        val ids = (0 until 40).map { "123e4567-e89b-42d3-a456-${it.toString().padStart(12, '0')}" }
        ids.forEachIndexed { index, captureId ->
            repository.save(CaptureRecord(captureId, createdAtUtc = "2026-01-02T03:04:${index.toString().padStart(2, '0')}Z",
                captureState = CaptureState.INTERRUPTED, localAudioPath = "/synthetic/$captureId.wav",
                finalizedByteLength = 48, sha256 = "c".repeat(64)))
        }
        val first = repository.dueNoteTransfers(0)
        assertEquals(ids.take(16), first.map { it.captureId })
        first.forEach { record -> repository.update(record.captureId) {
            it.copy(transferState = TransferState.SENDING, nextRetryAtUtc = "2026-01-03T00:00:00Z")
        } }
        assertEquals(ids.drop(16).take(16), openRepository().dueNoteTransfers(0).map { it.captureId })
        assertEquals(0L, repository.earliestNoteTransferDue())
        repository.update(ids.last()) { it.copy(transferState = TransferState.PHONE_ARCHIVED) }
        assertFalse(repository.dueNoteTransfers(Long.MAX_VALUE, 100).any { it.captureId == ids.last() })
    }

    @Test fun verifiedArchiveHandoffSurvivesReopenWithoutOverwritingLiveAuthorityOrSourceBinding() {
        val live = CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.RECORDING,
            observedFrames = 12, previewText = "Synthetic live preview", recognitionState = RecognitionState.RUNNING)
        repository.admitPhoneCapture(live, "synthetic-origin")
        val archive = live.copy(captureState = CaptureState.INTERRUPTED, observedFrames = 20, durableFrames = 20,
            localAudioPath = "/synthetic/audio.wav", finalizedByteLength = 84, sha256 = "d".repeat(64),
            archiveState = ArchiveState.STAGED)
        repository.stagePhoneArchive(archive, "synthetic-origin")
        assertEquals(live, openRepository().get(id))
        assertEquals(listOf(archive to "synthetic-origin"), openRepository().pendingPhoneArchives())
        assertThrows(IllegalStateException::class.java) { repository.stagePhoneArchive(archive, "synthetic-impostor") }
        assertThrows(IllegalStateException::class.java) {
            repository.stagePhoneArchive(archive.copy(sha256 = "e".repeat(64)), "synthetic-origin")
        }
        assertEquals(listOf(archive to "synthetic-origin"), openRepository().pendingPhoneArchives())
        repository.completePhoneArchiveHandoff(id)
        assertTrue(openRepository().pendingPhoneArchives().isEmpty())
        assertEquals("synthetic-origin", repository.phoneOrigin(id))
    }

    @Test fun recognitionAndOrgHaveIndependentDurableEligibilityDeadlinesAndVersionFences() {
        val record = CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.SAVED,
            localAudioPath = "/synthetic/audio.wav", finalizedByteLength = 48, sha256 = "a".repeat(64),
            retryCount = 2, nextRetryAtUtc = "2026-01-02T03:05:00Z")
        repository.save(record)
        assertTrue(repository.dueNoteRecognition(0).isEmpty())
        assertEquals(listOf(record), openRepository().dueNoteRecognition(Long.MAX_VALUE))
        repository.update(id) { it.copy(recognitionState = RecognitionState.RUNNING, retryCount = 5) }
        assertEquals(1, repository.dueNoteRecognition(0).size) // Restart must recover the final admitted attempt too.
        repository.update(id) { it.copy(recognitionState = RecognitionState.QUEUED) }
        assertTrue(repository.dueNoteRecognition(Long.MAX_VALUE).isEmpty())
        val final = repository.update(id) { it.copy(recognitionState = RecognitionState.FINAL, finalText = "Synthetic final",
            archiveState = ArchiveState.VERIFIED, archivedAudioUri = "content://synthetic/audio", transcriptVersion = 1) }!!
        assertEquals(listOf(final), repository.dueNoteOrg(0))
        repository.deferNoteOrg(id, 1, 100, false)
        assertEquals(5_100L, openRepository().earliestNoteOrgDue())
        assertEquals(5, repository.get(id)!!.retryCount) // Org did not consume recognition's budget.
        val corrected = repository.correctTranscript(id, "Synthetic corrected", 1)
        assertEquals(listOf(corrected), repository.dueNoteOrg(0))
        repository.deferNoteOrg(id, 1, 200, true) // Old version must not delay the correction.
        assertEquals(0L, repository.earliestNoteOrgDue())
        repository.update(id) { it.copy(captureState = CaptureState.INTERRUPTED) }
        assertTrue(repository.dueNoteOrg(Long.MAX_VALUE).isEmpty())
        assertTrue(repository.dueNoteRecognition(Long.MAX_VALUE).isEmpty())
    }

    @Test fun handoffBlockedOldestMovesAsideAndMetadataInsertFailureDoesNotConsumeBodyOrBindAnotherSource() {
        val records = (0 until 20).map { index ->
            CaptureRecord("123e4567-e89b-42d3-a456-${index.toString().padStart(12, '0')}", createdAtUtc = "2026-01-02T03:04:05Z",
                captureState = CaptureState.INTERRUPTED, localAudioPath = "/synthetic/audio.wav", finalizedByteLength = 48,
                sha256 = "a".repeat(64), archiveState = ArchiveState.STAGED)
        }
        records.forEach { repository.admitPhoneCapture(it, "synthetic-origin", true); repository.stagePhoneArchive(it, "synthetic-origin") }
        val first = repository.duePhoneArchives(0)
        assertEquals(16, first.size)
        first.forEach { repository.deferPhoneArchive(it.first.captureId, waitingForLive = true) }
        assertEquals(records.drop(16).map { it.captureId }, openRepository().duePhoneArchives(0).map { it.first.captureId })
        repository.wakePhoneArchives()
        assertEquals(16, repository.duePhoneArchives(0).size)
        repository.completePhoneArchiveHandoff(records.first().captureId)
        context.openOrCreateDatabase("captures.db", 0, null).use {
            it.execSQL("CREATE TRIGGER synthetic_handoff_failure BEFORE INSERT ON phone_pending_archives BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END")
        }
        assertThrows(android.database.SQLException::class.java) { repository.stagePhoneArchive(records.first(), "synthetic-origin") }
        assertNull(repository.pendingPhoneArchive(records.first().captureId))
        assertEquals(records.first(), repository.get(records.first().captureId))
        assertEquals("synthetic-origin", repository.phoneOrigin(records.first().captureId))
    }

    @Test fun metadataCommitFailurePreservesBothPcmAndPublishedWavAndLeavesEligibilityUnchanged() {
        val spool = repository.createPcmSpool(id)
        val record = repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z",
            captureState = CaptureState.INTERRUPTED, localAudioPath = spool.absolutePath))
        val wav = File(spool.parentFile, "audio.wav")
        val manifest = PcmRecordingWriter(spool, CaptureFormat()).use {
            it.append(byteArrayOf(1, 2, 3, 4)); it.finish(wav)
        }
        context.openOrCreateDatabase("captures.db", 0, null).use {
            it.execSQL("CREATE TRIGGER synthetic_finalization_failure BEFORE UPDATE ON captures BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END")
        }
        assertThrows(android.database.SQLException::class.java) { repository.update(id) {
            it.copy(localAudioPath = wav.absolutePath, sha256 = manifest.sha256, finalizedByteLength = manifest.wavByteLength)
        } }
        assertEquals(record, openRepository().get(id))
        assertEquals(4L, spool.length())
        assertEquals(48L, wav.length())
        assertTrue(repository.dueNoteArchives(Long.MAX_VALUE).isEmpty())
        assertTrue(repository.dueNoteTransfers(Long.MAX_VALUE).isEmpty())
    }

    @Test fun explicitRecognitionRequestSurvivesReopenAndDoesNotResetAutomaticAttemptCounter() {
        repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.SAVED,
            retryCount = 5, nextRetryAtUtc = "2026-01-03T00:00:00Z"))
        repository.requestNoteRecognition(id)
        assertEquals(1, openRepository().dueNoteRecognition(0).size)
        assertTrue(openRepository().noteRecognitionRequested(id))
        repository.update(id) { it.copy(nextRetryAtUtc = "2026-01-04T00:00:00Z", lastErrorStage = "execution") }
        assertTrue(repository.dueNoteRecognition(0).isEmpty())
        repository.update(id) { it.copy(archiveState = ArchiveState.RETRYABLE_ERROR) }
        assertTrue(repository.dueNoteRecognition(0).isEmpty()) // Unrelated effects cannot erase preflight backoff.
        repository.update(id) { it.copy(recognitionState = RecognitionState.RUNNING,
            recognitionSessionId = "123e4567-e89b-42d3-a456-426614174091") }
        assertFalse(openRepository().noteRecognitionRequested(id))
        assertEquals(5, repository.get(id)!!.retryCount)
    }

    @Test fun malformedEligibleBodyIsDeferredWithoutDeletionAndLaterRowsAndRepairRemainDiscoverable() {
        val first = repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.SAVED))
        val second = repository.save(first.copy(captureId = "123e4567-e89b-42d3-a456-426614174001"))
        val body = context.openOrCreateDatabase("captures.db", 0, null).use { db ->
            val original = db.rawQuery("SELECT body FROM captures WHERE capture_id = ?", arrayOf(id)).use {
                it.moveToFirst(); it.getString(0)
            }
            db.execSQL("UPDATE captures SET body = ? WHERE capture_id = ?", arrayOf("{synthetic-malformed", id))
            original
        }
        assertTrue(repository.dueNoteRecognition(0, 1).isEmpty())
        repeat(100) { assertEquals(listOf(second), repository.dueNoteRecognition(0, 1)) }
        context.openOrCreateDatabase("captures.db", 0, null).use { db ->
            db.rawQuery("SELECT body, recognition_due FROM captures WHERE capture_id = ?", arrayOf(id)).use {
                assertTrue(it.moveToFirst()); assertEquals("{synthetic-malformed", it.getString(0)); assertEquals(60_000L, it.getLong(1))
            }
            db.execSQL("UPDATE captures SET body = ? WHERE capture_id = ?", arrayOf(body, id))
        }
        assertEquals(listOf(second), openRepository().dueNoteRecognition(59_999, 1))
        assertEquals(setOf(first, second), openRepository().dueNoteRecognition(60_000).toSet())
        assertEquals(0, repository.get(id)!!.retryCount)
    }

    @Test fun runtimeWakeSelectsOnlyExecutionBlockedRecognitionWithoutResettingItsBudgetOrOtherDeadlines() {
        val future = "2026-01-03T00:00:00Z"
        val blocked = repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.SAVED,
            recognitionState = RecognitionState.QUEUED, lastErrorStage = "execution", lastErrorCode = "connected-device-service-not-ready",
            nextRetryAtUtc = future, retryCount = 2))
        val ordinary = repository.save(blocked.copy(captureId = "123e4567-e89b-42d3-a456-426614174001",
            recognitionState = RecognitionState.FAILED, lastErrorStage = "recognition", lastErrorCode = "synthetic-provider-failure"))
        val exhausted = repository.save(blocked.copy(captureId = "123e4567-e89b-42d3-a456-426614174002", retryCount = 5))
        repository.wakeRuntimeRecognition()
        assertEquals(listOf(blocked), openRepository().dueNoteRecognition(0))
        assertEquals(ordinary, repository.get(ordinary.captureId))
        assertEquals(exhausted, repository.get(exhausted.captureId))
        assertEquals(2, repository.get(id)!!.retryCount)
    }

    @Test fun versionOneMigrationBackfillsLegacyDefaultsWithoutChangingBodiesOrOriginBindings() {
        val record = CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z",
            captureState = CaptureState.INTERRUPTED, localAudioPath = "/synthetic/audio.wav",
            finalizedByteLength = 48, sha256 = "b".repeat(64))
        assertNotNull(repository.admitPhoneCapture(record, "synthetic-origin", verifiedNewArchive = true))
        val helper = CaptureRepository::class.java.getDeclaredField("helper").apply { isAccessible = true }
        (helper.get(repository) as android.database.sqlite.SQLiteOpenHelper).close()
        context.openOrCreateDatabase("captures.db", 0, null).use { db ->
            val body = db.rawQuery("SELECT body FROM captures", null).use { cursor ->
                cursor.moveToFirst(); org.json.JSONObject(cursor.getString(0)).apply { remove("purpose") }.toString()
            }
            db.execSQL("CREATE TABLE legacy_captures (capture_id TEXT PRIMARY KEY NOT NULL, created_at_utc TEXT NOT NULL, capture_state TEXT NOT NULL, body TEXT NOT NULL)")
            db.execSQL("INSERT INTO legacy_captures VALUES (?, ?, ?, ?)", arrayOf(id, record.createdAtUtc, "INTERRUPTED", body))
            db.execSQL("DROP TABLE captures")
            db.execSQL("ALTER TABLE legacy_captures RENAME TO captures")
            db.execSQL("CREATE INDEX captures_created ON captures(created_at_utc DESC, capture_id DESC)")
            db.version = 1
        }
        val reopened = openRepository()
        assertEquals(record, reopened.get(id))
        assertEquals("synthetic-origin", reopened.phoneOrigin(id))
        assertEquals(listOf(record), reopened.dueNoteArchives(0))
        context.openOrCreateDatabase("captures.db", 0, null).use { db ->
            for (stage in listOf("archive", "transfer", "recognition", "org")) {
                db.rawQuery("EXPLAIN QUERY PLAN SELECT body FROM captures WHERE ${stage}_eligible = 1 AND ${stage}_due <= 0 ORDER BY ${stage}_due, created_at_utc, capture_id LIMIT 16", null).use { cursor ->
                    val details = buildList { while (cursor.moveToNext()) add(cursor.getString(3)) }.joinToString(" ")
                    assertTrue(details, details.contains("captures_${stage}_due"))
                    assertFalse(details, details.contains("TEMP B-TREE"))
                }
            }
        }
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

    @Test fun correctionComparesTheSelectedVersionAndRejectedAttemptsRollBackEveryField() {
        val original = repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z",
            captureState = CaptureState.SAVED, recognitionState = RecognitionState.RUNNING,
            recognitionSessionId = "123e4567-e89b-42d3-a456-426614174096", transcriptVersion = 3,
            orgState = OrgState.EXPORTED, exportTranscriptVersion = 2, finalText = "Synthetic baseline"))
        assertThrows(CaptureTranscriptConflictException::class.java) { repository.correctTranscript(id, "Synthetic stale correction", 2) }
        assertEquals(original, openRepository().get(id))
        assertThrows(IllegalArgumentException::class.java) { repository.correctTranscript(id, "Synthetic negative version", -1) }
        assertEquals(original, openRepository().get(id))
        val accepted = repository.correctTranscript(id, "Synthetic matched correction", 3)
        assertEquals(4L, accepted.transcriptVersion)
        assertNull(accepted.recognitionSessionId)
        assertEquals(OrgState.EXPORTED, accepted.orgState)
        assertEquals(2L, accepted.exportTranscriptVersion)
        assertThrows(CaptureTranscriptConflictException::class.java) { openRepository().correctTranscript(id, "Synthetic later stale correction", 3) }
        assertEquals(accepted, repository.get(id))
        val legacy = repository.correctTranscript(id, "Synthetic legacy edit")
        assertEquals(5L, legacy.transcriptVersion)
    }

    @Test fun separateRepositoryWritersCannotBothCommitTheSameSelectedVersion() {
        repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.SAVED))
        val writers = listOf(openRepository(), openRepository())
        val entered = java.util.concurrent.CountDownLatch(2)
        val release = java.util.concurrent.CountDownLatch(1)
        val executor = java.util.concurrent.Executors.newFixedThreadPool(2)
        try {
            val attempts = writers.mapIndexed { index, writer -> executor.submit<Result<CaptureRecord>> {
                entered.countDown()
                check(release.await(3, java.util.concurrent.TimeUnit.SECONDS))
                runCatching { writer.correctTranscript(id, "Synthetic competing editor $index", 0) }
            } }
            assertTrue(entered.await(3, java.util.concurrent.TimeUnit.SECONDS))
            release.countDown()
            val results = attempts.map { it.get(5, java.util.concurrent.TimeUnit.SECONDS) }
            assertEquals(1, results.count { it.isSuccess })
            assertEquals(1, results.count { it.exceptionOrNull() is CaptureTranscriptConflictException })
            val winner = results.single { it.isSuccess }.getOrThrow()
            assertEquals(1L, winner.transcriptVersion)
            assertEquals(winner, repository.get(id))
        } finally {
            release.countDown(); executor.shutdownNow()
            assertTrue(executor.awaitTermination(3, java.util.concurrent.TimeUnit.SECONDS))
        }
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
