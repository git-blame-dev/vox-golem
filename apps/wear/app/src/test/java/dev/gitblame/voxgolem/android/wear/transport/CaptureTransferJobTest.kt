package dev.gitblame.voxgolem.android.wear.transport

import android.app.job.JobScheduler
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureRepository
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.TransferState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.UUID
import java.time.Instant

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureTransferJobTest {
    @Test
    fun olderPendingCaptureRemainsInTheOutboxBeyondTheRecentHistoryPage() {
        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val oldId = "123e4567-e89b-42d3-a456-426614174050"
        shared.save(CaptureRecord(captureId = oldId, createdAtUtc = "2026-01-01T00:00:00Z",
            captureState = CaptureState.SAVED, transferState = TransferState.PENDING, localAudioPath = "/synthetic/audio.wav"))
        repeat(501) { index -> shared.save(CaptureRecord(
            captureId = java.util.UUID.randomUUID().toString(),
            createdAtUtc = Instant.parse("2026-01-02T00:00:00Z").plusSeconds(index.toLong()).toString(),
            captureState = CaptureState.SAVED, transferState = TransferState.PHONE_ARCHIVED,
        )) }
        val watch = dev.gitblame.voxgolem.android.wear.capture.CaptureRepository(context)
        assertTrue(watch.records().none { it.captureId == oldId })
        assertTrue(watch.pendingTransfers().any { it.captureId == oldId })
        CaptureTransferJob.schedule(context)
        assertTrue(context.getSystemService(JobScheduler::class.java).allPendingJobs.isNotEmpty())
    }

    @Test
    fun repeatedPhoneStagedReceiptsBackOffStatusPolling() {
        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val watch = dev.gitblame.voxgolem.android.wear.capture.CaptureRepository(context)
        val id = "123e4567-e89b-42d3-a456-426614174041"
        shared.save(CaptureRecord(captureId = id, createdAtUtc = "2026-01-02T03:04:05Z"))
        watch.updateTransfer(id, TransferState.PHONE_STAGED)
        val first = Instant.parse(shared.get(id)!!.nextRetryAtUtc)
        watch.updateTransfer(id, TransferState.PHONE_STAGED)
        val second = Instant.parse(shared.get(id)!!.nextRetryAtUtc)
        assertTrue(second.isAfter(first.plusSeconds(20)))
        repeat(10) { watch.updateTransfer(id, TransferState.PHONE_STAGED) }
        val capped = Instant.parse(shared.get(id)!!.nextRetryAtUtc)
        assertTrue(capped.isBefore(Instant.now().plusSeconds(301)))
    }

    @Test
    fun pendingRecordingGetsAPersistedRetryWithoutRequiringAnActivityResume() {
        val context = RuntimeEnvironment.getApplication()
        val repository = CaptureRepository(context)
        val id = "123e4567-e89b-42d3-a456-426614174040"
        repository.save(CaptureRecord(
            captureId = id,
            createdAtUtc = "2026-01-02T03:04:05Z",
            captureState = CaptureState.SAVED,
            transferState = TransferState.SENDING,
            localAudioPath = "/synthetic/audio.wav",
            nextRetryAtUtc = Instant.now().plusSeconds(30).toString(),
        ))

        CaptureTransferJob.schedule(context)
        val scheduler = context.getSystemService(JobScheduler::class.java)
        val job = scheduler.allPendingJobs.single()
        assertTrue(job.isPersisted)
        assertTrue(job.minLatencyMillis in 25_000L..30_000L)

        repository.update(id) { it.copy(transferState = TransferState.PHONE_ARCHIVED, completionNotified = true) }
        CaptureTransferJob.schedule(context)
        assertEquals(0, scheduler.allPendingJobs.size)
    }

    @Test
    fun offlineArchiveAttemptPersistsThirtySecondDeadlineBeforeCapabilityLookup() {
        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        val started = Instant.parse("2026-01-02T03:04:05Z")
        shared.save(CaptureRecord(
            captureId = id,
            createdAtUtc = started.toString(),
            captureState = CaptureState.INTERRUPTED,
            transferState = TransferState.PENDING,
            localAudioPath = "/synthetic/$id/audio.wav",
            finalizedByteLength = 44,
            sha256 = "a".repeat(64),
        ))

        WearCaptureTransport(context).transferPending(started)

        val attempted = shared.get(id)!!
        assertEquals(TransferState.SENDING, attempted.transferState)
        assertEquals(started.plusSeconds(30).toString(), attempted.nextRetryAtUtc)
        assertEquals(1, attempted.retryCount)
    }

    @Test
    fun lostPhoneStagedStatusResponseCannotPollBeforeAdvancedDeadline() {
        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        val start = Instant.parse("2026-01-02T03:04:05Z")
        shared.save(CaptureRecord(
            captureId = id,
            createdAtUtc = start.toString(),
            captureState = CaptureState.INTERRUPTED,
            transferState = TransferState.PHONE_STAGED,
            localAudioPath = "/synthetic/$id/audio.wav",
            finalizedByteLength = 44,
            sha256 = "b".repeat(64),
            retryCount = 0,
            nextRetryAtUtc = start.plusSeconds(30).toString(),
        ))
        val transport = WearCaptureTransport(context)

        transport.transferPending(start.plusSeconds(29))
        assertEquals(start.plusSeconds(30).toString(), shared.get(id)!!.nextRetryAtUtc)

        transport.transferPending(start.plusSeconds(30))
        val polled = shared.get(id)!!
        assertEquals(TransferState.PHONE_STAGED, polled.transferState)
        assertEquals(start.plusSeconds(90).toString(), polled.nextRetryAtUtc)
        assertEquals(1, polled.retryCount)

        transport.transferPending(start.plusSeconds(31))
        assertEquals(start.plusSeconds(90).toString(), shared.get(id)!!.nextRetryAtUtc)
    }

    @Test
    fun persistedTransferBackoffGrowsFromThirtySecondsToFiveMinutes() {
        val start = Instant.parse("2026-01-02T03:04:05Z")
        assertEquals(30L, dev.gitblame.voxgolem.android.wear.capture.transferRetryDelaySeconds(0))
        assertEquals(60L, dev.gitblame.voxgolem.android.wear.capture.transferRetryDelaySeconds(1))
        assertEquals(300L, dev.gitblame.voxgolem.android.wear.capture.transferRetryDelaySeconds(8))

        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val watch = dev.gitblame.voxgolem.android.wear.capture.CaptureRepository(context)
        val id = UUID.randomUUID().toString()
        shared.save(CaptureRecord(captureId = id, createdAtUtc = start.toString()))

        val first = watch.beginTransferAttempt(id, start)!!
        assertEquals(start.plusSeconds(30).toString(), first.nextRetryAtUtc)
        watch.updateTransfer(id, TransferState.PENDING, start.plusSeconds(1))
        assertEquals(start.plusSeconds(30).toString(), shared.get(id)!!.nextRetryAtUtc)
        assertEquals(TransferState.PENDING, shared.get(id)!!.transferState)

        val retry = watch.beginTransferAttempt(id, Instant.parse(shared.get(id)!!.nextRetryAtUtc))!!
        assertEquals(start.plusSeconds(90).toString(), retry.nextRetryAtUtc)
        repeat(10) { watch.beginTransferAttempt(id, Instant.parse(shared.get(id)!!.nextRetryAtUtc)) }
        val beforeCappedAttempt = Instant.parse(shared.get(id)!!.nextRetryAtUtc)
        watch.beginTransferAttempt(id, beforeCappedAttempt)
        assertEquals(beforeCappedAttempt.plusSeconds(300).toString(), shared.get(id)!!.nextRetryAtUtc)
    }

    @Test fun offlineFailureOfOneCaptureDoesNotPreventAnotherDueCaptureFromPersistingItsOwnRetry() {
        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val at = Instant.parse("2026-10-05T12:30:00Z")
        fun pending(offset: Long, retryAt: String?, state: TransferState = TransferState.PENDING): CaptureRecord = shared.save(CaptureRecord(
            captureId = UUID.randomUUID().toString(), createdAtUtc = at.plusSeconds(offset).toString(),
            captureState = CaptureState.INTERRUPTED, transferState = state,
            localAudioPath = "/synthetic/offline-$offset/audio.wav", finalizedByteLength = 48,
            durableFrames = 2, sha256 = "a".repeat(64), nextRetryAtUtc = retryAt,
        ))
        val first = pending(0, null)
        val malformedDeadline = pending(1, "synthetic invalid timestamp")
        val future = pending(2, at.plusSeconds(90).toString())
        val staged = pending(3, at.toString(), TransferState.PHONE_STAGED)
        WearCaptureTransport(context).transferPending(at)
        for (record in listOf(first, malformedDeadline)) {
            val attempted = shared.get(record.captureId)!!
            assertEquals(TransferState.SENDING, attempted.transferState)
            assertEquals(1, attempted.retryCount)
            assertEquals(at.plusSeconds(30).toString(), attempted.nextRetryAtUtc)
        }
        assertEquals(future, shared.get(future.captureId))
        assertEquals(TransferState.PHONE_STAGED, shared.get(staged.captureId)!!.transferState)
        assertEquals(at.plusSeconds(60).toString(), shared.get(staged.captureId)!!.nextRetryAtUtc)
        assertEquals(4, dev.gitblame.voxgolem.android.wear.capture.CaptureRepository(context).pendingTransfers().size)
    }

    @Test fun archivedAndNoSpeechCompletionCancelOnlyTheirOwnOutboxEntries() {
        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val at = Instant.parse("2026-10-05T12:30:00Z")
        fun saved(offset: Long) = shared.save(CaptureRecord(
            captureId = UUID.randomUUID().toString(), createdAtUtc = at.plusSeconds(offset).toString(),
            captureState = CaptureState.SAVED, transferState = TransferState.PHONE_ARCHIVED,
            localAudioPath = "/synthetic/$offset/audio.wav",
        ))
        val completed = saved(0)
        val noSpeech = saved(1)
        val stillWaiting = saved(2)
        shared.update(completed.captureId) { it.copy(completionNotified = true) }
        shared.update(noSpeech.captureId) { it.copy(recognitionState = dev.gitblame.voxgolem.capture.RecognitionState.NO_SPEECH) }
        val watch = dev.gitblame.voxgolem.android.wear.capture.CaptureRepository(context)
        assertEquals(listOf(stillWaiting.captureId), watch.pendingTransfers().map { it.captureId })
        CaptureTransferJob.schedule(context, immediate = true)
        assertEquals(0L, context.getSystemService(JobScheduler::class.java).allPendingJobs.single().minLatencyMillis)
        assertTrue(shared.get(completed.captureId)!!.completionNotified)
    }

    @Test fun stagedOrArchivedCapturesCannotBeginAnArchiveResendOrLoseReceiptState() {
        val context = RuntimeEnvironment.getApplication()
        val shared = CaptureRepository(context)
        val watch = dev.gitblame.voxgolem.android.wear.capture.CaptureRepository(context)
        val at = Instant.parse("2026-10-05T12:30:00Z")
        for (status in listOf(TransferState.PHONE_STAGED, TransferState.PHONE_ARCHIVED)) {
            val record = shared.save(CaptureRecord(UUID.randomUUID().toString(), createdAtUtc = at.toString(), transferState = status))
            assertEquals(null, watch.beginTransferAttempt(record.captureId, at))
            assertEquals(record, shared.get(record.captureId))
            watch.updateTransfer(record.captureId, TransferState.PENDING, at)
            assertEquals(status, shared.get(record.captureId)!!.transferState)
        }
    }
}
