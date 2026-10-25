package dev.gitblame.voxgolem.capture

import android.net.Uri
import android.content.Context
import android.content.ContentValues
import android.provider.DocumentsContract
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowContentResolver
import org.robolectric.shadows.ShadowSystemClock
import java.io.File
import java.security.MessageDigest
import java.time.Duration
import java.util.Collections
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class PhoneCaptureArchiveTest {
    @Test
    @Config(sdk = [25], manifest = Config.NONE)
    fun committedJournalBatchContinuesThroughArchiveBackoffWithoutAnotherWake() {
        val fixture = residualJournalFixture()
        val repository = fixture.repository
        val attempted = CountDownLatch(1)
        val completed = CountDownLatch(1)
        val results = Collections.synchronizedList(mutableListOf<OrgExportResult>())
        val delivery = PhoneCaptureDelivery(repository, { _, _, _ -> }, { _, _, _ -> })
        val archive = PhoneCaptureArchive(repository, { fail("Already archived"); Uri.EMPTY }, { id ->
            assertEquals(fixture.pendingId, id)
            OrgExportCoordinator(RuntimeEnvironment.getApplication()).process(id).also {
                results += it
                attempted.countDown()
                if (it.state == OrgState.EXPORTED) completed.countDown()
            }
        }, delivery)
        try {
            archive.wake()
            assertTrue(attempted.await(2, TimeUnit.SECONDS))
            val drained = CountDownLatch(1)
            archive.barrier { drained.countDown() }
            assertTrue(drained.await(2, TimeUnit.SECONDS))
            assertEquals(1, results.size)
            assertEquals(OrgState.RETRYABLE_ERROR, results.single().state)
            assertEquals("export-cleanup-pending", repository.get(fixture.pendingId)!!.lastErrorCode)
            assertEquals(1, fixture.journalCount())
            assertFalse(fixture.pendingDirectory.exists())
            assertEquals(0, fixture.provider.writeCount)
            val due = checkNotNull(repository.earliestNoteOrgDue())
            assertTrue(due > System.currentTimeMillis())
            assertTrue(due < Long.MAX_VALUE)
            assertTrue(repository.dueNoteOrg(System.currentTimeMillis()).isEmpty())
            // Advance Robolectric wall time to the persisted boundary; the real archive timer owns the retry.
            ShadowSystemClock.advanceBy(Duration.ofMillis((due - System.currentTimeMillis()).coerceAtLeast(0L)))
            assertTrue(completed.await(8, TimeUnit.SECONDS))
            val finished = CountDownLatch(1)
            archive.barrier { finished.countDown() }
            assertTrue(finished.await(2, TimeUnit.SECONDS))
            assertEquals(listOf(OrgState.RETRYABLE_ERROR, OrgState.EXPORTED), results.map { it.state })
            assertEquals(0, fixture.journalCount())
            assertEquals(OrgState.EXPORTED, repository.get(fixture.pendingId)!!.orgState)
            assertEquals(null, repository.earliestNoteOrgDue())
            assertEquals(1, fixture.provider.writeCount)
            assertArrayEquals(OrgExport.prepend(OrgExport.formatEntry("Synthetic scheduled pending note", "2026-10-03"), fixture.original), fixture.provider.onlyInboxBytes())
        } finally { archive.shutdown(); delivery.shutdown(); ShadowContentResolver.reset() }
    }

    @Test
    @Config(sdk = [25], manifest = Config.NONE)
    fun unknownResidualJournalParksArchiveInsteadOfSchedulingCleanupRetries() {
        val fixture = residualJournalFixture(malformedLast = true)
        val repository = fixture.repository
        val attempted = CountDownLatch(1)
        val attempts = AtomicInteger()
        val delivery = PhoneCaptureDelivery(repository, { _, _, _ -> }, { _, _, _ -> })
        val archive = PhoneCaptureArchive(repository, { fail("Already archived"); Uri.EMPTY }, { id ->
            attempts.incrementAndGet()
            OrgExportCoordinator(RuntimeEnvironment.getApplication()).process(id).also { attempted.countDown() }
        }, delivery)
        try {
            archive.wake()
            assertTrue(attempted.await(2, TimeUnit.SECONDS))
            val drained = CountDownLatch(1)
            archive.barrier { drained.countDown() }
            assertTrue(drained.await(2, TimeUnit.SECONDS))
            assertEquals(1, attempts.get())
            assertEquals(OrgState.NEEDS_REVIEW, repository.get(fixture.pendingId)!!.orgState)
            assertEquals(Long.MAX_VALUE, repository.earliestNoteOrgDue())
            assertTrue(repository.dueNoteOrg(System.currentTimeMillis()).isEmpty())
            assertEquals(33, fixture.journalCount())
            assertFalse(fixture.pendingDirectory.exists())
            assertEquals(0, fixture.provider.writeCount)
            assertArrayEquals(fixture.original, fixture.provider.onlyInboxBytes())
        } finally { archive.shutdown(); delivery.shutdown(); ShadowContentResolver.reset() }
    }

    private data class ResidualFixture(
        val repository: CaptureRepository,
        val provider: OrgExportDocumentsProviderTest.SyntheticDocumentsProvider,
        val pendingId: String,
        val pendingDirectory: File,
        val original: ByteArray,
        val journalCount: () -> Int,
    )

    private fun residualJournalFixture(malformedLast: Boolean = false): ResidualFixture {
        val context: Context = RuntimeEnvironment.getApplication()
        context.deleteDatabase("captures.db")
        context.deleteDatabase("capture_org_journal.db")
        context.getSharedPreferences("capture_notes", Context.MODE_PRIVATE).edit().clear().commit()
        val authority = OrgExportDocumentsProviderTest.SyntheticDocumentsProvider.AUTHORITY
        val tree = DocumentsContract.buildTreeDocumentUri(authority, OrgExportDocumentsProviderTest.SyntheticDocumentsProvider.ROOT_ID)
        val provider = OrgExportDocumentsProviderTest.SyntheticDocumentsProvider(context.cacheDir)
        provider.attachInfo(context, android.content.pm.ProviderInfo().apply {
            this.authority = authority
            exported = true
            grantUriPermissions = true
            readPermission = "android.permission.MANAGE_DOCUMENTS"
            writePermission = "android.permission.MANAGE_DOCUMENTS"
        })
        ShadowContentResolver.registerProviderInternal(authority, provider)
        context.grantUriPermission(context.packageName, tree,
            android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION or android.content.Intent.FLAG_GRANT_WRITE_URI_PERMISSION)
        val destination = SafNotesStore(context).select(tree)
        val repository = CaptureRepository(context)
        fun hash(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
        fun directory(id: String) = File(File(context.filesDir, "capture-org"), "journal.${hash("$id:${destination.id}".toByteArray())}")
        var content = "* Synthetic retained scheduler note\n".toByteArray()
        provider.addExistingDocument("ai-inbox.org", content)
        val target = DocumentsContract.buildDocumentUriUsingTree(tree, provider.inboxDocuments().single().id)
        context.openOrCreateDatabase("capture_org_journal.db", Context.MODE_PRIVATE, null).use { database ->
            database.execSQL("CREATE TABLE org_journal (capture_id TEXT NOT NULL, destination_id TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY(capture_id, destination_id))")
            database.version = 1
            for (index in 0..32) {
                val id = "123e4567-e89b-42d3-a456-${index.toString().padStart(12, '0')}"
                val text = "Synthetic committed scheduler note $index"
                repository.save(CaptureRecord(id, createdAtUtc = "2026-10-03T12:00:00Z", finishedLocalDate = "2026-10-03",
                    captureState = CaptureState.SAVED, recognitionState = RecognitionState.FINAL, finalText = text,
                    transcriptVersion = 1, archiveState = ArchiveState.VERIFIED, archivedAudioUri = "content://synthetic/audio/$id",
                    orgState = OrgState.EXPORTED, exportDestinationId = destination.id, exportTranscriptVersion = 1))
                val before = content
                content = OrgExport.prepend(OrgExport.formatEntry(text, "2026-10-03"), before)
                val journal = JSONObject().put("captureId", id).put("destinationId", destination.id)
                    .put("phase", "WRITING").put("transcriptVersion", 1).put("targetName", ORG_INBOX_NAME)
                    .put("targetUri", target.toString()).put("targetWasAbsent", false)
                    .put("preHash", hash(before)).put("postHash", hash(content))
                    .put("preLength", before.size).put("postLength", content.size)
                    .put("preimage", File(directory(id), "preimage.bin").absolutePath)
                    .put("postimage", File(directory(id), "postimage.bin").absolutePath)
                    .put("entry", File(directory(id), "entry.bin").absolutePath)
                database.insertOrThrow("org_journal", null, ContentValues().apply {
                    put("capture_id", id); put("destination_id", destination.id)
                    put("body", if (malformedLast && index == 32) "{invalid" else journal.toString())
                })
            }
        }
        provider.replaceBytes(provider.inboxDocuments().single().id, content)
        val pending = "123e4567-e89b-42d3-a456-999999999999"
        repository.save(CaptureRecord(pending, createdAtUtc = "2026-10-03T12:00:00Z", finishedLocalDate = "2026-10-03",
            captureState = CaptureState.SAVED, recognitionState = RecognitionState.FINAL,
            finalText = "Synthetic scheduled pending note", transcriptVersion = 1, archiveState = ArchiveState.VERIFIED,
            archivedAudioUri = "content://synthetic/audio/$pending", orgState = OrgState.READY))
        return ResidualFixture(repository, provider, pending, directory(pending), content) {
            context.openOrCreateDatabase("capture_org_journal.db", Context.MODE_PRIVATE, null).use { database ->
                database.rawQuery("SELECT COUNT(*) FROM org_journal", null).use { check(it.moveToFirst()); it.getInt(0) }
            }
        }
    }

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
