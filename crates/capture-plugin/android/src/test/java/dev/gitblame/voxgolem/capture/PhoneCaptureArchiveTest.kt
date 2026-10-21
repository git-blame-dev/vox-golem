package dev.gitblame.voxgolem.capture

import android.net.Uri
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class PhoneCaptureArchiveTest {
    @Test fun configurationWakeRacingAnOldMissingFolderResultCannotStrandTheRow() {
        val repository = CaptureRepository(RuntimeEnvironment.getApplication())
        val id = "123e4567-e89b-42d3-a456-426614174098"
        repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.INTERRUPTED,
            localAudioPath = "/synthetic/audio.wav", finalizedByteLength = 48, sha256 = "a".repeat(64)))
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val delivery = PhoneCaptureDelivery(repository, { _, _, _ -> }, { _, _, _ -> })
        val archive = PhoneCaptureArchive(repository, {
            entered.countDown(); check(release.await(5, TimeUnit.SECONDS))
            throw IllegalStateException("notes-folder-not-selected")
        }, { fail("Interrupted Notes cannot export"); OrgExportResult(OrgState.EXPORTED, null) }, delivery)
        try {
            archive.wake()
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            archive.wake(configurationChanged = true)
            release.countDown()
            val done = CountDownLatch(1)
            archive.barrier { done.countDown() }
            assertTrue(done.await(2, TimeUnit.SECONDS))
            assertTrue(repository.earliestNoteArchiveDue()!! < Long.MAX_VALUE)
        } finally { release.countDown(); archive.shutdown(); delivery.shutdown() }
    }

    @Test fun blockedOrgFloodUsesOneWriterAndFailingFirstRowDoesNotStarveLaterPages() {
        val repository = CaptureRepository(RuntimeEnvironment.getApplication())
        val records = (0 until 40).map { index ->
            CaptureRecord("123e4567-e89b-42d3-a456-${index.toString().padStart(12, '0')}",
                createdAtUtc = "2026-01-02T03:04:${index.toString().padStart(2, '0')}Z", captureState = CaptureState.SAVED,
                recognitionState = RecognitionState.FINAL, finalText = "Synthetic note", transcriptVersion = 1,
                archiveState = ArchiveState.VERIFIED, archivedAudioUri = "content://synthetic/audio", orgState = OrgState.READY)
                .also(repository::save)
        }
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val completed = CountDownLatch(39)
        val effects = AtomicInteger()
        val peak = AtomicInteger()
        val delivery = PhoneCaptureDelivery(repository, { _, _, _ -> }, { _, _, _ -> })
        val archive = PhoneCaptureArchive(repository, { fail("Already archived"); Uri.EMPTY }, { id ->
            val active = effects.incrementAndGet()
            peak.accumulateAndGet(active, ::maxOf)
            try {
                if (id == records.first().captureId) {
                    entered.countDown(); check(release.await(5, TimeUnit.SECONDS))
                    throw IllegalStateException("synthetic-export-failure")
                }
                repository.update(id) { it.copy(orgState = OrgState.EXPORTED) }
                completed.countDown()
                OrgExportResult(OrgState.EXPORTED, "synthetic-destination")
            } finally { effects.decrementAndGet() }
        }, delivery)
        try {
            archive.wake()
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            repeat(1_000) { archive.maybeExport(records.first()) }
            assertEquals(1, effects.get())
            release.countDown()
            assertTrue(completed.await(5, TimeUnit.SECONDS))
            assertEquals(1, peak.get())
            assertEquals(OrgState.RETRYABLE_ERROR, repository.get(records.first().captureId)!!.orgState)
            assertTrue(repository.dueNoteOrg(System.currentTimeMillis()).isEmpty())
        } finally { release.countDown(); archive.shutdown(); delivery.shutdown() }
    }

    @Test fun interruptedArchiveWaitsForConfigurationAndRecoversWithFreshOwnerWithoutRecognitionOrOrg() {
        val context = RuntimeEnvironment.getApplication()
        val repository = CaptureRepository(context)
        val id = "123e4567-e89b-42d3-a456-426614174099"
        repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", captureState = CaptureState.INTERRUPTED,
            localAudioPath = "/synthetic/audio.wav", finalizedByteLength = 48, sha256 = "a".repeat(64),
            recognitionState = RecognitionState.FINAL, finalText = "Synthetic interrupted text", archiveState = ArchiveState.STAGED))
        val attempted = CountDownLatch(1)
        val delivery = PhoneCaptureDelivery(repository, { _, _, _ -> }, { _, _, _ -> })
        val first = PhoneCaptureArchive(repository, { attempted.countDown(); throw IllegalStateException("notes-folder-not-selected") },
            { fail("Interrupted Notes cannot export"); OrgExportResult(OrgState.EXPORTED, null) }, delivery)
        try {
            first.wake()
            assertTrue(attempted.await(2, TimeUnit.SECONDS))
            val barrier = CountDownLatch(1)
            first.barrier { barrier.countDown() }
            assertTrue(barrier.await(2, TimeUnit.SECONDS))
            assertEquals(Long.MAX_VALUE, repository.earliestNoteArchiveDue())
        } finally { first.shutdown() }
        val archived = CountDownLatch(1)
        val reopened = CaptureRepository(context)
        val second = PhoneCaptureArchive(reopened, { Uri.parse("content://synthetic/recovered").also { archived.countDown() } },
            { fail("Interrupted Notes cannot export"); OrgExportResult(OrgState.EXPORTED, null) }, delivery)
        try {
            second.wake(configurationChanged = true)
            assertTrue(archived.await(2, TimeUnit.SECONDS))
            val barrier = CountDownLatch(1)
            second.barrier { barrier.countDown() }
            assertTrue(barrier.await(2, TimeUnit.SECONDS))
            assertEquals(ArchiveState.VERIFIED, reopened.get(id)!!.archiveState)
            assertTrue(reopened.dueNoteRecognition(Long.MAX_VALUE).isEmpty())
            assertTrue(reopened.dueNoteOrg(Long.MAX_VALUE).isEmpty())
        } finally { second.shutdown(); delivery.shutdown() }
    }
}
