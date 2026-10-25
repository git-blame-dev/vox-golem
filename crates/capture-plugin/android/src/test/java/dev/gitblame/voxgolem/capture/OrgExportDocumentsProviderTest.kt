package dev.gitblame.voxgolem.capture

import android.content.Context
import android.content.ContentValues
import android.database.Cursor
import android.database.MatrixCursor
import android.database.sqlite.SQLiteDatabase
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.DocumentsProvider
import android.webkit.MimeTypeMap
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.json.JSONObject
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowContentResolver
import java.io.File
import java.security.MessageDigest
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [25])
class OrgExportDocumentsProviderTest {
    private val context: Context = RuntimeEnvironment.getApplication()
    private val authority = SyntheticDocumentsProvider.AUTHORITY
    private val treeUri = DocumentsContract.buildTreeDocumentUri(authority, SyntheticDocumentsProvider.ROOT_ID)
    private lateinit var provider: SyntheticDocumentsProvider
    private lateinit var repository: CaptureRepository

    @Before
    fun setUp() {
        context.deleteDatabase("captures.db")
        context.deleteDatabase("capture_org_journal.db")
        context.getSharedPreferences("capture_notes", Context.MODE_PRIVATE).edit().clear().commit()
        context.cacheDir.listFiles()?.filter { it.name.startsWith("synthetic-org-") }?.forEach(File::deleteRecursively)
        provider = SyntheticDocumentsProvider(context.cacheDir).apply { appendPlainTextExtension = true }
        provider.attachInfo(context, android.content.pm.ProviderInfo().apply {
            authority = this@OrgExportDocumentsProviderTest.authority
            exported = true
            grantUriPermissions = true
            readPermission = "android.permission.MANAGE_DOCUMENTS"
            writePermission = "android.permission.MANAGE_DOCUMENTS"
        })
        ShadowContentResolver.registerProviderInternal(authority, provider)
        context.grantUriPermission(
            context.packageName,
            treeUri,
            android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION or android.content.Intent.FLAG_GRANT_WRITE_URI_PERMISSION,
        )
        SafNotesStore(context).select(treeUri)
        repository = CaptureRepository(context)
    }

    @After
    fun tearDown() {
        ShadowContentResolver.reset()
    }

    @Test
    fun syntheticProviderHonorsGrantedTreeDocumentAccessWithoutCoordinator() {
        provider.addExistingDocument("ai-inbox.org", "Synthetic provider contract".toByteArray())
        val target = DocumentsContract.buildDocumentUriUsingTree(treeUri, provider.inboxDocuments().single().id)
        context.contentResolver.query(target, arrayOf(DocumentsContract.Document.COLUMN_DISPLAY_NAME), null, null, null)!!.use {
            assertTrue(it.moveToFirst())
            assertEquals("ai-inbox.org", it.getString(0))
        }
        assertEquals("Synthetic provider contract", context.contentResolver.openInputStream(target)!!.bufferedReader().use { it.readText() })
    }

    @Test
    fun currentJournalCannotReadWriteOrAcknowledgeTargetsOutsideSelectedInbox() {
        val original = "* Synthetic outside note\n".toByteArray()
        provider.addExistingDocument("ai-inbox.org", original)
        provider.hideChildren = true
        val targetId = provider.allDocuments().single().id
        val otherProvider = SyntheticDocumentsProvider(context.cacheDir).apply {
            addExistingDocument("ai-inbox.org", original)
        }
        otherProvider.attachInfo(context, android.content.pm.ProviderInfo().apply {
            authority = "synthetic.other.provider"
            exported = true
            grantUriPermissions = true
            readPermission = "android.permission.MANAGE_DOCUMENTS"
            writePermission = "android.permission.MANAGE_DOCUMENTS"
        })
        ShadowContentResolver.registerProviderInternal("synthetic.other.provider", otherProvider)
        val otherTree = DocumentsContract.buildTreeDocumentUri(authority, "synthetic-other-root")
        val otherProviderTree = DocumentsContract.buildTreeDocumentUri("synthetic.other.provider", SyntheticDocumentsProvider.ROOT_ID)
        val targets = listOf(
            DocumentsContract.buildDocumentUriUsingTree(otherTree, targetId),
            DocumentsContract.buildDocumentUriUsingTree(otherProviderTree, targetId),
            DocumentsContract.buildDocumentUri(authority, targetId),
            DocumentsContract.buildDocumentUriUsingTree(treeUri, targetId),
            android.net.Uri.parse("file:///synthetic/ai-inbox.org"),
        )
        for (target in targets) for (postimage in listOf(false, true)) {
            val id = saveCapture("synthetic outside recovery", "2026-10-03")
            writeJournal(id, targetId, original, "WRITING", targetWasAbsent = false)
            val journal = readJournal(id).put("targetUri", target.toString())
            storeJournal(id, journal)
            val bytes = if (postimage) File(journal.getString("postimage")).readBytes() else original
            provider.replaceBytes(targetId, bytes)
            otherProvider.replaceBytes(targetId, bytes)
            val queries = provider.documentQueryCount
            val otherQueries = otherProvider.documentQueryCount
            assertRecoveryHeld(id)
            assertEquals(queries, provider.documentQueryCount)
            assertEquals(otherQueries, otherProvider.documentQueryCount)
            assertEquals(0, otherProvider.writeCount)
            assertTrue(otherProvider.bytesAtRename.isEmpty())
            assertArrayEquals(bytes, otherProvider.bytes(targetId))
        }
    }

    @Test
    fun committedResidualJournalsAreRetiredAtNewAdmissionAcrossCrashBoundaries() {
        for (deletedStages in 0..3) {
            val first = saveCapture("synthetic committed owner $deletedStages", "2026-10-03")
            val existing = provider.inboxDocuments().singleOrNull()
            val original = existing?.let { provider.bytes(it.id) } ?: byteArrayOf()
            if (existing == null) provider.addExistingDocument("ai-inbox.org", original)
            val targetId = provider.inboxDocuments().single().id
            writeJournal(first, targetId, original, "WRITING", targetWasAbsent = false)
            val journal = readJournal(first)
            val stages = listOf("preimage", "postimage", "entry").map { File(journal.getString(it)) }
            provider.replaceBytes(targetId, stages[1].readBytes())
            val destinationId = checkNotNull(SafNotesStore(context).selected()).id
            repository.update(first) { it.copy(orgState = OrgState.EXPORTED, exportDestinationId = destinationId, exportTranscriptVersion = it.transcriptVersion) }
            val corrected = repository.correctTranscript(first, "synthetic corrected after export $deletedStages", expectedVersion = 1)
            assertEquals(OrgState.EXPORTED, corrected.orgState)
            assertEquals(2L, corrected.transcriptVersion)
            assertEquals(1L, corrected.exportTranscriptVersion)
            stages.take(deletedStages).forEach { assertTrue(it.delete()) }
            val before = provider.onlyInboxBytes()
            val second = saveCapture("synthetic admitted after crash $deletedStages", "2026-10-04")

            assertEquals(OrgState.EXPORTED, coordinator().process(second).state)
            assertEquals(0, journalCount(first))
            stages.forEach { assertFalse(it.exists()) }
            assertArrayEquals(OrgExport.prepend(OrgExport.formatEntry("synthetic admitted after crash $deletedStages", "2026-10-04"), before), provider.onlyInboxBytes())
            assertEquals(1, countOccurrences(String(provider.onlyInboxBytes()), "* TODO synthetic committed owner $deletedStages\n"))
            assertFalse(String(provider.onlyInboxBytes()).contains("synthetic corrected after export"))
        }
    }

    @Test
    fun committedCleanupUsesOnlyDerivedPrivatePathsAndNeedsNoProviderReadOrStages() {
        val (id, _) = prepareCommittedJournal()
        val journal = readJournal(id)
        val stages = listOf("preimage", "postimage", "entry").map { File(journal.getString(it)) }
        val outside = File(context.cacheDir, "synthetic-org-not-a-stage-${UUID.randomUUID()}").apply {
            writeBytes("Synthetic unrelated file".toByteArray())
        }
        for (field in listOf("preimage", "postimage", "entry")) journal.put(field, outside.absolutePath)
        storeJournal(id, journal)
        markCommitted(id)
        repository.correctTranscript(id, "synthetic corrected but not reexported", expectedVersion = 1)
        stages.forEach { assertTrue(it.delete()) }
        provider.hideChildren = true
        provider.failReads = true
        val before = provider.onlyInboxBytes()
        val queries = provider.documentQueryCount

        assertEquals(OrgState.EXPORTED, coordinator().process(id).state)
        assertEquals(0, journalCount(id))
        stages.forEach { assertFalse(it.exists()) }
        assertEquals("Synthetic unrelated file", outside.readText())
        assertArrayEquals(before, provider.onlyInboxBytes())
        assertEquals(queries, provider.documentQueryCount)
        assertEquals(0, provider.writeCount)
        assertTrue(provider.bytesAtRename.isEmpty())
    }

    @Test
    fun committedCleanupDoesNotFollowPrivateStageSymlinks() {
        val (id, _) = prepareCommittedJournal()
        markCommitted(id)
        val stage = File(readJournal(id).getString("postimage"))
        val outside = File(context.cacheDir, "synthetic-org-cleanup-outside-${UUID.randomUUID()}").apply {
            writeBytes(stage.readBytes())
        }
        val outsideBytes = outside.readBytes()
        assertTrue(stage.delete())
        java.nio.file.Files.createSymbolicLink(stage.toPath(), outside.toPath())
        val journalBefore = readJournal(id).toString()
        val second = saveCapture("synthetic blocked unsafe cleanup", "2026-10-04")

        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(second).state)
        assertEquals(journalBefore, readJournal(id).toString())
        assertTrue(java.nio.file.Files.isSymbolicLink(stage.toPath()))
        assertArrayEquals(outsideBytes, outside.readBytes())
        assertEquals(0, journalCount(second))
        assertFalse(recoveryDirectory(second).exists())
        assertEquals(0, provider.writeCount)
    }

    @Test
    fun residualJournalRetirementRequiresExactDurableOwnerIdentityAndVersion() {
        val changes: List<(String, JSONObject) -> Unit> = listOf(
            { id, _ -> repository.update(id) { it.copy(orgState = OrgState.NEEDS_REVIEW) } },
            { id, _ -> repository.update(id) { it.copy(exportDestinationId = "synthetic-other-destination") } },
            { id, _ -> repository.update(id) { it.copy(exportTranscriptVersion = 2) } },
            { id, _ -> repository.update(id) { it.copy(transcriptVersion = 0) } },
            { id, journal -> storeJournal(id, journal.put("captureId", "synthetic-other-owner")) },
            { id, journal -> storeJournal(id, journal.put("destinationId", "synthetic-other-destination")) },
            { id, journal -> storeJournal(id, journal.put("targetName", "inbox.org")) },
            { id, journal -> storeJournal(id, journal.also { it.remove("postHash") }) },
        )
        for (change in changes) {
            val (id, _) = prepareCommittedJournal()
            markCommitted(id)
            change(id, readJournal(id))
            val journalBefore = readJournal(id).toString()
            val stages = listOf("preimage.bin", "postimage.bin", "entry.bin").map { File(recoveryDirectory(id), it) }
            val stageBytes = stages.map { it.readBytes().toList() }
            val before = provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() }
            val second = saveCapture("synthetic blocked unproven commit", "2026-10-04")
            repeat(2) { assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(second).state) }
            assertEquals(journalBefore, readJournal(id).toString())
            assertEquals(stageBytes, stages.map { it.readBytes().toList() })
            assertEquals(before, provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() })
            assertEquals(0, journalCount(second))
            assertFalse(recoveryDirectory(second).exists())
            assertEquals(0, provider.writeCount)
        }
    }

    @Test
    fun admissionRetiresOnlyABoundedBatchAndLeavesExcessRowsFenced() {
        provider.addExistingDocument("ai-inbox.org", "* Synthetic retained\n".toByteArray())
        val target = provider.inboxDocuments().single().id
        val owners = (0..32).map {
            saveCapture("synthetic committed batch $it", "2026-10-03").also { id ->
                writeJournal(id, target, provider.bytes(target), "WRITING", targetWasAbsent = false)
                provider.replaceBytes(target, File(readJournal(id).getString("postimage")).readBytes())
                markCommitted(id)
            }
        }
        val second = saveCapture("synthetic bounded admission", "2026-10-04")
        val result = coordinator().process(second)
        assertEquals(OrgState.RETRYABLE_ERROR, result.state)
        assertEquals("export-cleanup-pending", result.reason)
        assertEquals(1, owners.sumOf { journalCount(it) })
        assertEquals(0, journalCount(second))
        assertFalse(recoveryDirectory(second).exists())
        assertEquals(0, provider.writeCount)
    }

    private fun markCommitted(id: String) {
        val destinationId = checkNotNull(SafNotesStore(context).selected()).id
        repository.update(id) { it.copy(orgState = OrgState.EXPORTED, exportDestinationId = destinationId, exportTranscriptVersion = it.transcriptVersion) }
    }

    @Test
    fun currentRecoveryRejectsMissingCorruptAndUnownedStages() {
        for (field in listOf("preimage", "postimage", "entry")) {
            for (damage in listOf("missing", "corrupt", "unowned", "symlink", "alias")) {
                val (id, _) = prepareCommittedJournal()
                val journal = readJournal(id)
                val stage = File(journal.getString(field))
                val outside = File(context.cacheDir, "synthetic-org-outside-${UUID.randomUUID()}").apply { writeBytes(stage.readBytes()) }
                when (damage) {
                    "missing" -> assertTrue(stage.delete())
                    "corrupt" -> stage.writeBytes("Synthetic corrupt stage".toByteArray())
                    "unowned" -> storeJournal(id, journal.put(field, outside.absolutePath))
                    "symlink" -> {
                        assertTrue(stage.delete())
                        java.nio.file.Files.createSymbolicLink(stage.toPath(), outside.toPath())
                    }
                    "alias" -> storeJournal(id, journal.put(field, "${stage.parent}/../${checkNotNull(stage.parentFile).name}/${stage.name}"))
                }
                val outsideBytes = outside.readBytes()
                assertRecoveryHeld(id)
                assertArrayEquals(outsideBytes, outside.readBytes())
            }
        }
    }

    @Test
    fun currentRecoveryRejectsInvalidHashesLengthsIdentityAndPostimageComposition() {
        val changes: List<(JSONObject) -> Unit> = listOf(
            { it.put("captureId", "synthetic-different-owner") },
            { it.put("destinationId", "synthetic-other-destination") },
            { it.put("transcriptVersion", 2) },
            { it.put("phase", "UNKNOWN") },
            { it.put("targetWasAbsent", true) },
            { it.put("preLength", it.getLong("preLength") + 1) },
            { it.put("postLength", it.getLong("postLength") + 1) },
            { it.put("preHash", "synthetic-invalid-hash") },
            { it.put("postHash", "synthetic-invalid-hash") },
        )
        changes.forEach { change ->
            val (id, _) = prepareCommittedJournal()
            storeJournal(id, readJournal(id).also(change))
            assertRecoveryHeld(id)
        }
        val (id, targetId) = prepareCommittedJournal()
        val journal = readJournal(id)
        val wrongPostimage = File(journal.getString("postimage")).readBytes().also { it[it.lastIndex] = '!'.code.toByte() }
        File(journal.getString("postimage")).writeBytes(wrongPostimage)
        storeJournal(id, journal.put("postHash", sha256(wrongPostimage)))
        provider.replaceBytes(targetId, wrongPostimage)
        assertRecoveryHeld(id)
    }

    @Test
    fun partialExistingWriteFencesLaterCaptureWithoutCreatingItsJournalOrStages() {
        provider.addExistingDocument("ai-inbox.org", "* Synthetic retained\n".toByteArray())
        val first = saveCapture("synthetic interrupted task", "2026-10-03")
        provider.partialWrite = true
        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(first).state)
        provider.partialWrite = false
        val partial = provider.onlyInboxBytes()
        val writes = provider.writeCount
        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(first).state)
        assertArrayEquals(partial, provider.onlyInboxBytes())
        val second = saveCapture("synthetic blocked task", "2026-10-04")

        repeat(2) { assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(second).state) }
        assertArrayEquals(partial, provider.onlyInboxBytes())
        assertEquals(writes, provider.writeCount)
        assertEquals(0, journalCount(second))
        assertFalse(recoveryDirectory(second).exists())
        assertTrue(provider.createdMimeTypes.isEmpty())
        assertTrue(provider.bytesAtRename.isEmpty())
    }

    @Test
    fun missingInboxPendingJournalBlocksPreparationButOwnJournalCanCreateAndRecover() {
        val first = saveCapture("synthetic pending creation", "2026-10-03")
        writeJournal(first, null, byteArrayOf(), "PREPARED", targetWasAbsent = true)
        val second = saveCapture("synthetic blocked creation", "2026-10-04")
        repeat(2) { assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(second).state) }
        assertTrue(provider.allDocuments().isEmpty())
        assertEquals(0, journalCount(second))
        assertFalse(recoveryDirectory(second).exists())
        assertEquals(OrgState.EXPORTED, coordinator().process(first).state)
        assertEquals(OrgState.EXPORTED, coordinator().process(second).state)
        assertEquals(1, provider.createdMimeTypes.size)
    }

    @Test
    fun textExtensionPendingJournalBlocksRenameAndPreparationForOtherCapture() {
        provider.addExistingDocument("ai-inbox.org.txt", "* Synthetic retained\n".toByteArray())
        val target = provider.inboxDocuments().single().id
        val first = saveCapture("synthetic owner", "2026-10-03")
        writeJournal(first, target, provider.bytes(target), "PREPARED", targetWasAbsent = false)
        val second = saveCapture("synthetic blocked rename", "2026-10-04")
        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(second).state)
        assertEquals(0, journalCount(second))
        assertFalse(recoveryDirectory(second).exists())
        assertTrue(provider.bytesAtRename.isEmpty())
        assertEquals(0, provider.writeCount)
        assertEquals(OrgState.EXPORTED, coordinator().process(first).state)
        assertEquals(OrgState.EXPORTED, coordinator().process(second).state)
        assertEquals(1, provider.bytesAtRename.size)
    }

    @Test
    fun malformedJournalBlocksItsOwnerAndOtherCapturesWithOrWithoutInbox() {
        for (existing in listOf(false, true)) {
            if (existing) provider.addExistingDocument("ai-inbox.org", "* Synthetic retained\n".toByteArray())
            val first = saveCapture("synthetic malformed journal", "2026-10-03")
            storeJournal(first, JSONObject())
            context.openOrCreateDatabase("capture_org_journal.db", Context.MODE_PRIVATE, null).use {
                it.execSQL("UPDATE org_journal SET body = ? WHERE capture_id = ?", arrayOf("{invalid", first))
            }
            val documents = provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() }
            val second = saveCapture("synthetic blocked malformed", "2026-10-04")
            repeat(2) {
                assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(first).state)
                assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(second).state)
            }
            assertEquals(0, journalCount(second))
            assertFalse(recoveryDirectory(second).exists())
            assertEquals(documents, provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() })
            assertEquals(0, provider.writeCount)
            assertTrue(provider.createdMimeTypes.isEmpty())
            assertTrue(provider.bytesAtRename.isEmpty())
        }
    }

    @Test
    fun missingCurrentJournalFieldsFailClosedWithoutChangingRecoveryEvidence() {
        for (field in listOf("targetName", "captureId", "destinationId", "phase", "transcriptVersion", "targetUri",
            "targetWasAbsent", "preimage", "postimage", "entry", "preHash", "postHash", "preLength", "postLength")) {
            val (id, _) = prepareCommittedJournal()
            storeJournal(id, readJournal(id).also { it.remove(field) })
            assertRecoveryHeld(id)
            val blocked = saveCapture("synthetic blocked missing field", "2026-10-04")
            assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(blocked).state)
            assertEquals(0, journalCount(blocked))
            assertFalse(recoveryDirectory(blocked).exists())
        }
    }

    @Test
    fun unreadableJournalTableFailsClosedWithoutProviderMutations() {
        provider.addExistingDocument("ai-inbox.org", "* Synthetic retained\n".toByteArray())
        val id = saveCapture("synthetic query failure", "2026-10-03")
        storeJournal(id, JSONObject())
        context.openOrCreateDatabase("capture_org_journal.db", Context.MODE_PRIVATE, null).use {
            it.execSQL("ALTER TABLE org_journal RENAME TO synthetic_unreadable_journal")
        }
        repeat(2) { assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(id).state) }
        assertFalse(recoveryDirectory(id).exists())
        assertEquals(0, provider.writeCount)
        assertTrue(provider.createdMimeTypes.isEmpty())
        assertTrue(provider.bytesAtRename.isEmpty())
    }

    @Test
    fun separateCapturesPreserveExistingBytesAndReplayWithoutCreatingOrDuplicatingInbox() {
        assertEquals(null, MimeTypeMap.getSingleton().getExtensionFromMimeType(ORG_DOCUMENT_MIME_TYPE))
        provider.addExistingDocument("ai-inbox.org", "* Existing synthetic note\r\n".toByteArray())

        val firstId = saveCapture("synthetic first task", "2026-10-03")
        val firstResult = coordinator().process(firstId)
        assertEquals("$firstResult created=${provider.createdMimeTypes} inbox=${provider.inboxDocuments()}", OrgState.EXPORTED, firstResult.state)
        val secondId = saveCapture("synthetic second task", "2026-10-04")
        assertEquals(OrgState.EXPORTED, coordinator().process(secondId).state)

        val content = String(provider.onlyInboxBytes())
        assertEquals(1, provider.inboxDocuments().size)
        assertEquals(0, provider.createdMimeTypes.count { it == "text/plain" })
        assertEquals(1, countOccurrences(content, "* TODO synthetic first task"))
        assertEquals(1, countOccurrences(content, "* TODO synthetic second task"))
        assertTrue(content.contains("* TODO synthetic first task\nSCHEDULED: <2026-10-03 Sat>\n\n"))
        assertTrue(content.contains("* TODO synthetic second task\nSCHEDULED: <2026-10-04 Sun>\n\n"))
        assertTrue(content.endsWith("* Existing synthetic note\r\n"))
        val writeCountAfterTwoCaptures = provider.writeCount

        assertEquals(OrgState.EXPORTED, coordinator().process(secondId).state)
        assertEquals(content, String(provider.onlyInboxBytes()))
        assertEquals(writeCountAfterTwoCaptures, provider.writeCount)
        assertEquals(1, provider.inboxDocuments().size)
    }

    @Test
    fun normalOrgMimeCreatesOneInboxForTwoFinalCaptures() {
        val firstId = saveCapture("synthetic created first task", "2026-10-03")
        val secondId = saveCapture("synthetic created second task", "2026-10-04")

        assertEquals(OrgState.EXPORTED, coordinator().process(firstId).state)
        assertEquals(OrgState.EXPORTED, coordinator().process(secondId).state)

        assertEquals(listOf(ORG_DOCUMENT_MIME_TYPE), provider.createdMimeTypes)
        assertEquals("ai-inbox.org", provider.inboxDocuments().single().displayName)
        val content = String(provider.onlyInboxBytes())
        assertTrue(content.contains("* TODO synthetic created first task\nSCHEDULED: <2026-10-03 Sat>\n\n"))
        assertTrue(content.contains("* TODO synthetic created second task\nSCHEDULED: <2026-10-04 Sun>\n\n"))
    }

    @Test
    fun knownUniqueTextSuffixedNeedsReviewJournalRecoversOnlyWithMatchingPreimage() {
        val id = saveCapture("synthetic recovered task", "2026-10-03")
        val targetId = provider.createDocument(SyntheticDocumentsProvider.ROOT_ID, "text/plain", "ai-inbox.org")
        writeTextExtensionNeedsReviewJournal(id, targetId, byteArrayOf())

        assertEquals(OrgState.EXPORTED, coordinator().process(id).state)
        assertEquals("ai-inbox.org", provider.inboxDocuments().single().displayName)
        assertNotEquals(targetId, provider.inboxDocuments().single().id)

        val content = String(provider.onlyInboxBytes())
        assertTrue(content.startsWith("* TODO synthetic recovered task\nSCHEDULED: <2026-10-03 Sat>\n\n"))
        assertEquals(1, provider.inboxDocuments().size)
        assertEquals(1, provider.writeCount)
    }

    @Test
    fun textSuffixedJournalWithChangedPreimageRemainsInReview() {
        val id = saveCapture("synthetic changed target", "2026-10-03")
        val targetId = provider.createDocument(SyntheticDocumentsProvider.ROOT_ID, "text/plain", "ai-inbox.org")
        writeTextExtensionNeedsReviewJournal(id, targetId, byteArrayOf())
        provider.replaceBytes(targetId, "* Changed by user\n".toByteArray())

        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(id).state)
        assertEquals("* Changed by user\n", String(provider.onlyInboxBytes()))
        assertEquals(0, provider.writeCount)
    }

    @Test
    fun canonicalAiInboxWinsWithoutTouchingTextExtensionOrNumberedFiles() {
        provider.addExistingDocument("ai-inbox.org", "* First synthetic inbox\n".toByteArray())
        provider.addExistingDocument("ai-inbox.org.txt", "* Second synthetic inbox\n".toByteArray())
        provider.addExistingDocument("ai-inbox.org (1).txt", "* Numbered synthetic inbox\n".toByteArray())
        val extras = provider.allDocuments().filter { it.displayName != "ai-inbox.org" }.associate { it.id to provider.bytes(it.id).toList() }

        val result = coordinator().process(saveCapture("synthetic canonical task", "2026-10-03"))

        assertEquals(OrgState.EXPORTED, result.state)
        assertEquals(2, provider.inboxDocuments().size)
        assertEquals(1, provider.writeCount)
        assertEquals(0, provider.createdMimeTypes.size)
        assertTrue(String(provider.bytes(provider.allDocuments().single { it.displayName == "ai-inbox.org" }.id)).endsWith("* First synthetic inbox\n"))
        assertEquals(extras, provider.allDocuments().filter { it.displayName != "ai-inbox.org" }.associate { it.id to provider.bytes(it.id).toList() })
    }

    @Test
    fun uniqueAiTextExtensionRenamesWithNewIdPreservingBytesBeforePrepend() {
        val original = byteArrayOf(0xef.toByte(), 0xbb.toByte(), 0xbf.toByte()) + "* Synthetic existing\r\n☃".toByteArray()
        provider.addExistingDocument("ai-inbox.org.txt", original)
        val oldId = provider.inboxDocuments().single().id

        assertEquals(OrgState.EXPORTED, coordinator().process(saveCapture("synthetic renamed task", "2026-10-03")).state)

        assertEquals("ai-inbox.org", provider.inboxDocuments().single().displayName)
        assertNotEquals(oldId, provider.inboxDocuments().single().id)
        assertArrayEquals(original, provider.bytesAtRename.single())
        assertArrayEquals(OrgExport.prepend(OrgExport.formatEntry("synthetic renamed task", "2026-10-03"), original), provider.onlyInboxBytes())
        assertTrue(provider.createdMimeTypes.isEmpty())
    }

    @Test
    fun createdProviderSuffixIsRenamedToExactInbox() {
        provider.appendOrgExtension = true

        assertEquals(OrgState.EXPORTED, coordinator().process(saveCapture("synthetic suffix task", "2026-10-03")).state)

        assertEquals("ai-inbox.org", provider.inboxDocuments().single().displayName)
        assertEquals(1, provider.bytesAtRename.size)
        assertEquals(listOf(ORG_DOCUMENT_MIME_TYPE), provider.createdMimeTypes)
    }

    @Test
    fun deniedExactRenameRetainsFileAndJournalWithoutCreatingDuplicatesAcrossCaptures() {
        provider.appendOrgExtension = true
        provider.denyRename = true
        val first = saveCapture("synthetic denied first", "2026-10-03")

        val result = coordinator().process(first)
        assertNotEquals(OrgState.EXPORTED, result.state)
        assertEquals("provider-cannot-name-inbox-exactly-rename-to-ai-inbox.org-or-choose-another-folder", result.reason)
        assertEquals("ai-inbox.org.txt", provider.inboxDocuments().single().displayName)
        assertEquals(0, provider.onlyInboxBytes().size)
        assertEquals("ai-inbox.org", readJournal(first).getString("targetName"))
        assertEquals(provider.inboxDocuments().single().id, DocumentsContract.getDocumentId(android.net.Uri.parse(readJournal(first).getString("targetUri"))))
        assertTrue(File(readJournal(first).getString("postimage")).isFile)

        assertNotEquals(OrgState.EXPORTED, coordinator().process(first).state)
        assertNotEquals(OrgState.EXPORTED, coordinator().process(saveCapture("synthetic denied second", "2026-10-03")).state)
        assertEquals(1, provider.allDocuments().size)
        assertEquals(1, provider.createdMimeTypes.size)
        assertEquals(0, provider.writeCount)

        provider.denyRename = false
        assertEquals(OrgState.EXPORTED, coordinator().process(first).state)
    }

    @Test
    fun renamedUriIsJournaledBeforeWriteAndRecoversAfterProviderReadFailure() {
        provider.addExistingDocument("ai-inbox.org.txt", "* Synthetic retained\n".toByteArray())
        provider.failReadAfterRename = true
        val id = saveCapture("synthetic retry renamed", "2026-10-03")
        writeJournal(id, provider.inboxDocuments().single().id, "* Synthetic retained\n".toByteArray(), phase = "PREPARED", targetWasAbsent = false)

        assertNotEquals(OrgState.EXPORTED, coordinator().process(id).state)
        val renamed = provider.inboxDocuments().single()
        assertEquals("ai-inbox.org", renamed.displayName)
        assertEquals(renamed.id, DocumentsContract.getDocumentId(android.net.Uri.parse(readJournal(id).getString("targetUri"))))
        assertEquals(0, provider.writeCount)
        assertEquals(OrgState.EXPORTED, coordinator().process(id).state)
        assertEquals(1, provider.writeCount)
        assertTrue(String(provider.onlyInboxBytes()).endsWith("* Synthetic retained\n"))
    }

    @Test
    fun changedKnownJournalTargetIsNotRenamedOrWritten() {
        val original = "* Synthetic original\n".toByteArray()
        provider.addExistingDocument("ai-inbox.org.txt", original)
        val targetId = provider.inboxDocuments().single().id
        val id = saveCapture("synthetic stale journal", "2026-10-03")
        writeJournal(id, targetId, original, phase = "PREPARED", targetWasAbsent = false)
        val changed = "* Synthetic changed by user\n".toByteArray()
        provider.replaceBytes(targetId, changed)

        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(id).state)

        assertEquals(targetId, provider.inboxDocuments().single().id)
        assertEquals("ai-inbox.org.txt", provider.inboxDocuments().single().displayName)
        assertArrayEquals(changed, provider.onlyInboxBytes())
        assertTrue(provider.bytesAtRename.isEmpty())
        assertEquals(0, provider.writeCount)
    }

    @Test
    fun providerReturningWrongRenameNameRetainsChangedUriAndBlocksFurtherCreation() {
        provider.appendOrgExtension = true
        provider.returnWrongRenameName = true
        val id = saveCapture("synthetic wrong rename", "2026-10-03")

        val result = coordinator().process(id)

        assertEquals(OrgState.NEEDS_REVIEW, result.state)
        assertEquals("provider-cannot-name-inbox-exactly-rename-to-ai-inbox.org-or-choose-another-folder", result.reason)
        val renamed = provider.allDocuments().single()
        assertEquals("ai-inbox.org (1).txt", renamed.displayName)
        assertEquals(renamed.id, DocumentsContract.getDocumentId(android.net.Uri.parse(readJournal(id).getString("targetUri"))))
        assertEquals(0, provider.bytes(renamed.id).size)
        assertNotEquals(OrgState.EXPORTED, coordinator().process(id).state)
        assertNotEquals(OrgState.EXPORTED, coordinator().process(saveCapture("synthetic later capture", "2026-10-03")).state)
        assertEquals(1, provider.allDocuments().size)
        assertEquals(1, provider.createdMimeTypes.size)
        assertEquals(0, provider.writeCount)
    }

    @Test
    fun duplicateAiTextExtensionInboxesRemainUntouchedWithoutCanonicalTarget() {
        provider.addExistingDocument("ai-inbox.org.txt", "* Synthetic first extension\n".toByteArray())
        provider.addExistingDocument("ai-inbox.org.txt", "* Synthetic second extension\n".toByteArray())
        val original = provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() }

        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(saveCapture("synthetic ambiguous legacy", "2026-10-03")).state)
        assertEquals(original, provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() })
        assertTrue(provider.bytesAtRename.isEmpty())
        assertEquals(0, provider.writeCount)
        assertTrue(provider.createdMimeTypes.isEmpty())
    }

    @Test
    fun duplicateExactInboxesRemainUntouched() {
        provider.addExistingDocument("ai-inbox.org", "* Synthetic first\n".toByteArray())
        provider.addExistingDocument("ai-inbox.org", "* Synthetic second\n".toByteArray())
        val original = provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() }

        assertNotEquals(OrgState.EXPORTED, coordinator().process(saveCapture("synthetic duplicate", "2026-10-03")).state)
        assertEquals(original, provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() })
        assertEquals(0, provider.writeCount)
        assertTrue(provider.createdMimeTypes.isEmpty())
    }

    @Test
    fun newAiInboxLeavesOldInboxVariantsAndPastExportedCapturesUntouched() {
        val original = byteArrayOf(0xef.toByte(), 0xbb.toByte(), 0xbf.toByte()) + "* Synthetic old inbox\r\n☃".toByteArray()
        provider.addExistingDocument("inbox.org", original)
        provider.addExistingDocument("inbox.org.txt", "* Synthetic old extension\n".toByteArray())
        val oldDocuments = provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() }
        val pastId = saveCapture("synthetic previously exported task", "2026-10-03")
        val destinationId = checkNotNull(SafNotesStore(context).selected()).id
        repository.update(pastId) { it.copy(orgState = OrgState.EXPORTED, exportDestinationId = destinationId, exportTranscriptVersion = 1) }
        val pastRecord = repository.get(pastId)

        assertEquals(OrgState.EXPORTED, coordinator().process(pastId).state)
        assertEquals(pastRecord, repository.get(pastId))
        assertTrue(provider.inboxDocuments().isEmpty())
        val newId = saveCapture("synthetic new AI task", "2026-10-04")
        assertEquals(OrgState.EXPORTED, coordinator().process(newId).state)

        assertEquals("ai-inbox.org", provider.inboxDocuments().single().displayName)
        assertArrayEquals(OrgExport.formatEntry("synthetic new AI task", "2026-10-04"), provider.onlyInboxBytes())
        assertEquals(oldDocuments, provider.allDocuments().filter { it.id in oldDocuments }.associate { it.id to provider.bytes(it.id).toList() })
        assertTrue(provider.bytesAtRename.isEmpty())
        assertEquals(listOf(ORG_DOCUMENT_MIME_TYPE), provider.createdMimeTypes)
        assertEquals(1, provider.writeCount)
        assertEquals(OrgState.EXPORTED, coordinator().process(newId).state)
        assertEquals(1, provider.writeCount)
    }

    @Test
    fun journalWithoutTargetNameCannotReplayAndBlocksNewAiInboxCreation() {
        assertLegacyJournalHeld(targetName = null, documentName = "inbox.org", phase = "PREPARED")
    }

    @Test
    fun explicitlyOldTargetJournalCannotReplayOrRenameTextExtension() {
        assertLegacyJournalHeld(targetName = "inbox.org", documentName = "inbox.org.txt", phase = "NEEDS_REVIEW")
    }

    @Test
    fun journalDeclaringNewTargetButPointingAtOldUriCannotWriteOrRename() {
        assertLegacyJournalHeld(targetName = "ai-inbox.org", documentName = "inbox.org", phase = "WRITING")
    }

    @Test
    fun legacyJournalWithoutTargetUriCannotPrepareOrCreateNewTarget() {
        val id = saveCapture("synthetic old missing target", "2026-10-03")
        writeJournal(id, null, byteArrayOf(), phase = "PREPARED", targetWasAbsent = true, targetName = null)
        val journalBefore = readJournal(id).toString()

        val result = coordinator().process(id)

        assertEquals(OrgState.NEEDS_REVIEW, result.state)
        assertEquals("export-journal-target-changed-requires-review", result.reason)
        assertEquals(journalBefore, readJournal(id).toString())
        assertTrue(provider.allDocuments().isEmpty())
        assertTrue(provider.createdMimeTypes.isEmpty())
        assertTrue(provider.bytesAtRename.isEmpty())
        assertEquals(0, provider.writeCount)
    }

    @Test
    fun currentJournalCannotRenameAnArbitraryUserDocument() {
        val original = "* Synthetic unrelated user document\n".toByteArray()
        provider.addExistingDocument("synthetic-user-notes.org", original)
        val targetId = provider.allDocuments().single().id
        val id = saveCapture("synthetic unexpected target", "2026-10-03")
        writeJournal(id, targetId, original, phase = "PREPARED", targetWasAbsent = false)

        val result = coordinator().process(id)

        assertEquals(OrgState.NEEDS_REVIEW, result.state)
        assertEquals("export-journal-invalid", result.reason)
        assertEquals("synthetic-user-notes.org", provider.allDocuments().single().displayName)
        assertArrayEquals(original, provider.bytes(targetId))
        assertTrue(provider.bytesAtRename.isEmpty())
        assertTrue(provider.createdMimeTypes.isEmpty())
        assertEquals(0, provider.writeCount)
    }

    @Test
    fun obsoleteJournalPostimageIsNotAcknowledgedOrSkipped() {
        for (targetName in listOf(null, "inbox.org", "ai-inbox.org")) {
            val (id, _) = prepareCommittedJournal(targetName = targetName, documentName = "inbox.org")
            assertRecoveryHeld(id)
            val blocked = saveCapture("synthetic blocked obsolete journal", "2026-10-04")
            assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(blocked).state)
            assertEquals(0, journalCount(blocked))
            assertFalse(recoveryDirectory(blocked).exists())
            assertTrue(provider.inboxDocuments().isEmpty())
        }
    }

    @Test
    fun failedCurrentMetadataCommitFencesLaterCaptureUntilOwnPostimageRecovery() {
        provider.addExistingDocument("ai-inbox.org", "* Synthetic retained\n".toByteArray())
        val id = saveCapture("synthetic metadata failure", "2026-10-03")
        context.openOrCreateDatabase("captures.db", Context.MODE_PRIVATE, null).use { database ->
            database.execSQL("CREATE TRIGGER synthetic_org_commit_failure BEFORE UPDATE ON captures " +
                "WHEN NEW.body LIKE '%\"orgState\":\"EXPORTED\"%' " +
                "BEGIN SELECT RAISE(ABORT, 'Synthetic metadata commit failure'); END")
        }
        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(id).state)
        val stages = listOf("preimage", "postimage", "entry").map { File(readJournal(id).getString(it)) }
        val postimage = provider.onlyInboxBytes()
        assertEquals(1, provider.writeCount)
        stages.forEach { assertTrue(it.isFile) }
        val second = saveCapture("synthetic later task", "2026-10-04")
        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(second).state)
        assertEquals(0, journalCount(second))
        assertFalse(recoveryDirectory(second).exists())
        assertArrayEquals(postimage, provider.onlyInboxBytes())
        assertEquals(1, provider.writeCount)
        context.openOrCreateDatabase("captures.db", Context.MODE_PRIVATE, null).use { database ->
            database.execSQL("DROP TRIGGER synthetic_org_commit_failure")
        }
        assertEquals(OrgState.EXPORTED, coordinator().process(id).state)
        assertEquals(1, provider.writeCount)
        assertTrue(provider.createdMimeTypes.isEmpty())
        assertTrue(provider.bytesAtRename.isEmpty())
        assertEquals(0, journalCount(id))
        stages.forEach { assertFalse(it.exists()) }
        assertEquals(OrgState.EXPORTED, coordinator().process(second).state)
    }

    private fun prepareCommittedJournal(
        phase: String = "WRITING",
        targetName: String? = "ai-inbox.org",
        documentName: String = "ai-inbox.org",
    ): Pair<String, String> {
        val original = if (phase == "NEEDS_REVIEW") byteArrayOf()
            else byteArrayOf(0xef.toByte(), 0xbb.toByte(), 0xbf.toByte()) + "* Synthetic old inbox\r\n☃".toByteArray()
        val existing = provider.allDocuments().singleOrNull { it.displayName == documentName }
        if (existing == null) provider.addExistingDocument(documentName, original)
        val targetId = existing?.id ?: provider.allDocuments().last().id
        provider.replaceBytes(targetId, original)
        val id = saveCapture("synthetic committed task", "2026-10-03")
        writeJournal(id, targetId, original, phase, targetWasAbsent = original.isEmpty(), targetName = targetName)
        provider.replaceBytes(targetId, File(readJournal(id).getString("postimage")).readBytes())
        return id to targetId
    }

    private fun assertRecoveryHeld(id: String) {
        val journalBefore = readJournal(id).toString()
        val documentsBefore = provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() }
        val stages = listOf("preimage.bin", "postimage.bin", "entry.bin").map { File(recoveryDirectory(id), it) }
        val stageBytes = stages.associateWith { if (it.isFile) it.readBytes().toList() else null }
        repeat(2) {
            assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(id).state)
            assertEquals(OrgState.NEEDS_REVIEW, repository.get(id)?.orgState)
            assertEquals(journalBefore, readJournal(id).toString())
            assertEquals(documentsBefore, provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() })
            assertEquals(stageBytes, stages.associateWith { if (it.isFile) it.readBytes().toList() else null })
            assertTrue(provider.createdMimeTypes.isEmpty())
            assertTrue(provider.bytesAtRename.isEmpty())
            assertEquals(0, provider.writeCount)
        }
    }

    private fun journalCount(captureId: String): Int = context.openOrCreateDatabase("capture_org_journal.db", Context.MODE_PRIVATE, null).use { database ->
        database.rawQuery("SELECT COUNT(*) FROM org_journal WHERE capture_id = ?", arrayOf(captureId)).use { cursor ->
            check(cursor.moveToFirst())
            cursor.getInt(0)
        }
    }

    private fun recoveryDirectory(captureId: String): File {
        val destinationId = sha256(treeUri.toString().toByteArray())
        return File(File(context.filesDir, "capture-org"), "journal.${sha256("$captureId:$destinationId".toByteArray())}")
    }

    private fun assertLegacyJournalHeld(targetName: String?, documentName: String, phase: String) {
        val targetWasAbsent = phase == "NEEDS_REVIEW"
        val original = if (targetWasAbsent) byteArrayOf() else "* Synthetic old journal preimage\r\n".toByteArray()
        provider.addExistingDocument(documentName, original)
        val targetId = provider.allDocuments().single().id
        val id = saveCapture("synthetic held legacy task", "2026-10-03")
        writeJournal(id, targetId, original, phase, targetWasAbsent = targetWasAbsent, targetName = targetName)
        val journalBefore = readJournal(id).toString()
        val recoveryFiles = listOf("preimage", "postimage", "entry").associate { field ->
            readJournal(id).getString(field) to File(readJournal(id).getString(field)).readBytes().toList()
        }

        repeat(2) {
            val result = coordinator().process(id)
            assertEquals(OrgState.NEEDS_REVIEW, result.state)
            assertEquals(if (targetName == ORG_INBOX_NAME) "export-journal-invalid" else "export-journal-target-changed-requires-review", result.reason)
            assertEquals(OrgState.NEEDS_REVIEW, repository.get(id)?.orgState)
            assertEquals(journalBefore, readJournal(id).toString())
            assertArrayEquals(original, provider.bytes(targetId))
            assertEquals(documentName, provider.allDocuments().single().displayName)
            assertTrue(provider.createdMimeTypes.isEmpty())
            assertTrue(provider.bytesAtRename.isEmpty())
            assertEquals(0, provider.writeCount)
        }

        val blocked = saveCapture("synthetic blocked new task", "2026-10-04")
        assertEquals(OrgState.NEEDS_REVIEW, coordinator().process(blocked).state)
        assertEquals(0, journalCount(blocked))
        assertFalse(recoveryDirectory(blocked).exists())
        assertArrayEquals(original, provider.bytes(targetId))
        assertTrue(provider.inboxDocuments().isEmpty())
        assertEquals(journalBefore, readJournal(id).toString())
        assertEquals(recoveryFiles, recoveryFiles.keys.associateWith { File(it).readBytes().toList() })
        assertTrue(provider.bytesAtRename.isEmpty())
        assertEquals(0, provider.writeCount)
    }

    private fun saveCapture(text: String, date: String): String {
        val id = UUID.randomUUID().toString()
        repository.save(
            CaptureRecord(
                id,
                createdAtUtc = "2026-10-03T12:00:00Z",
                finishedLocalDate = date,
                captureState = CaptureState.SAVED,
                recognitionState = RecognitionState.FINAL,
                finalText = text,
                transcriptVersion = 1,
                archiveState = ArchiveState.VERIFIED,
                archivedAudioUri = "content://synthetic.provider/audio/$id",
            ),
        )
        return id
    }

    private fun coordinator() = OrgExportCoordinator(context)
    private fun countOccurrences(text: String, needle: String) = Regex(Regex.escape(needle)).findAll(text).count()

    private fun readJournal(captureId: String): JSONObject = context.openOrCreateDatabase("capture_org_journal.db", Context.MODE_PRIVATE, null).use { database ->
        database.rawQuery("SELECT body FROM org_journal WHERE capture_id = ?", arrayOf(captureId)).use { cursor ->
            check(cursor.moveToFirst())
            JSONObject(cursor.getString(0))
        }
    }

    private fun writeTextExtensionNeedsReviewJournal(captureId: String, targetId: String, original: ByteArray) =
        writeJournal(captureId, targetId, original, phase = "NEEDS_REVIEW", targetWasAbsent = true)

    private fun writeJournal(captureId: String, targetId: String?, original: ByteArray, phase: String, targetWasAbsent: Boolean, targetName: String? = "ai-inbox.org") {
        val destinationId = sha256(treeUri.toString().toByteArray())
        repository.update(captureId) { it.copy(orgState = OrgState.NEEDS_REVIEW, exportDestinationId = destinationId) }
        val directory = recoveryDirectory(captureId).apply { check(mkdirs() || isDirectory) }
        val preimage = File(directory, "preimage.bin").apply { writeBytes(original) }
        val capture = checkNotNull(repository.get(captureId))
        val entry = File(directory, "entry.bin").apply {
            writeBytes(OrgExport.formatEntry(checkNotNull(capture.finalText), checkNotNull(capture.finishedLocalDate)))
        }
        val postimage = File(directory, "postimage.bin").apply { writeBytes(OrgExport.prepend(entry.readBytes(), original)) }
        val targetUri = targetId?.let { DocumentsContract.buildDocumentUriUsingTree(treeUri, it).toString() } ?: ""
        val journal = JSONObject()
            .put("phase", phase)
            .put("captureId", captureId)
            .put("destinationId", destinationId)
            .put("transcriptVersion", 1)
            .put("targetUri", targetUri)
            .put("targetWasAbsent", targetWasAbsent)
            .put("preimage", preimage.absolutePath)
            .put("postimage", postimage.absolutePath)
            .put("entry", entry.absolutePath)
            .put("preHash", sha256(original))
            .put("postHash", sha256(postimage.readBytes()))
            .put("preLength", preimage.length())
            .put("postLength", postimage.length())
        if (targetName != null) journal.put("targetName", targetName)
        storeJournal(captureId, journal)
    }

    private fun storeJournal(captureId: String, journal: JSONObject) {
        val destinationId = sha256(treeUri.toString().toByteArray())
        context.openOrCreateDatabase("capture_org_journal.db", Context.MODE_PRIVATE, null).use { database ->
            database.execSQL("CREATE TABLE IF NOT EXISTS org_journal (capture_id TEXT NOT NULL, destination_id TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY(capture_id, destination_id))")
            val values = ContentValues().apply {
                put("capture_id", captureId)
                put("destination_id", destinationId)
                put("body", journal.toString())
            }
            check(database.insertWithOnConflict("org_journal", null, values, SQLiteDatabase.CONFLICT_REPLACE) != -1L)
            database.version = 1
        }
    }

    private fun sha256(bytes: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }

    data class SyntheticDocument(val id: String, val displayName: String, val mimeType: String)

    class SyntheticDocumentsProvider(private val storageDirectory: File) : DocumentsProvider() {
        private val documents = linkedMapOf<String, SyntheticDocument>()
        private val storage = mutableMapOf<String, File>()
        val createdMimeTypes = mutableListOf<String>()
        var appendPlainTextExtension = false
        var appendOrgExtension = false
        var denyRename = false
        var returnWrongRenameName = false
        var failReadAfterRename = false
        private var failNextRead = false
        val bytesAtRename = mutableListOf<ByteArray>()
        var writeCount = 0
            private set
        var partialWrite = false
        var hideChildren = false
        var failReads = false
        var documentQueryCount = 0
            private set

        override fun onCreate() = true

        override fun queryRoots(projection: Array<out String>): Cursor = MatrixCursor(projection).apply {
            addRow(projection.map { if (it == DocumentsContract.Root.COLUMN_ROOT_ID) ROOT_ID else null }.toTypedArray())
        }

        override fun queryDocument(documentId: String, projection: Array<out String>): Cursor = MatrixCursor(projection).apply {
            documentQueryCount++
            val document = if (documentId == ROOT_ID) SyntheticDocument(ROOT_ID, "Synthetic notes", DocumentsContract.Document.MIME_TYPE_DIR) else documents[documentId]
            if (document != null) addDocumentRow(this, projection, document)
        }

        override fun queryChildDocuments(parentDocumentId: String, projection: Array<out String>, sortOrder: String?): Cursor = MatrixCursor(projection).apply {
            if (!hideChildren) documents.values.filter { it.id != ROOT_ID }.forEach { addDocumentRow(this, projection, it) }
        }

        override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean =
            parentDocumentId == ROOT_ID && documents.containsKey(documentId)

        override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String {
            createdMimeTypes += mimeType
            val shouldAppendExtension = (mimeType == "text/plain" && appendPlainTextExtension) || (mimeType == ORG_DOCUMENT_MIME_TYPE && appendOrgExtension)
            val actualName = if (shouldAppendExtension && !displayName.endsWith(".txt")) "$displayName.txt" else displayName
            val id = "document-${documents.size}"
            documents[id] = SyntheticDocument(id, actualName, mimeType)
            check(parentDocumentId == ROOT_ID)
            return id
        }

        override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
            check(documents.containsKey(documentId)) { "Unknown document ID $documentId" }
            if (!mode.contains("w") && failReads) throw java.io.FileNotFoundException("Synthetic read denial")
            if (!mode.contains("w") && failNextRead) {
                failNextRead = false
                throw java.io.FileNotFoundException("Synthetic transient read failure")
            }
            val file = storage.getOrPut(documentId) {
                File(storageDirectory, "synthetic-org-${UUID.randomUUID()}").also { it.createNewFile() }
            }
            if (mode.contains("w")) writeCount++
            val flags = if (mode.contains("w")) {
                ParcelFileDescriptor.MODE_WRITE_ONLY or ParcelFileDescriptor.MODE_CREATE or ParcelFileDescriptor.MODE_TRUNCATE
            } else {
                ParcelFileDescriptor.MODE_READ_ONLY
            }
            if (mode.contains("w") && partialWrite) {
                file.writeBytes("Synthetic partial write".toByteArray())
                throw java.io.IOException("Synthetic interrupted replacement")
            }
            return ParcelFileDescriptor.open(file, flags)
        }

        override fun deleteDocument(documentId: String) = throw UnsupportedOperationException()
        override fun renameDocument(documentId: String, displayName: String): String {
            if (denyRename) throw UnsupportedOperationException("Synthetic rename denial")
            val old = checkNotNull(documents.remove(documentId))
            bytesAtRename += bytes(documentId)
            val newId = "$documentId-renamed"
            documents[newId] = old.copy(id = newId, displayName = if (returnWrongRenameName) "ai-inbox.org (1).txt" else displayName)
            storage.remove(documentId)?.let { storage[newId] = it }
            failNextRead = failReadAfterRename
            return newId
        }
        override fun getDocumentType(documentId: String): String? = documents[documentId]?.mimeType
        private fun addDocumentRow(cursor: MatrixCursor, projection: Array<out String>, document: SyntheticDocument) {
            cursor.addRow(projection.map { column ->
                when (column) {
                    DocumentsContract.Document.COLUMN_DOCUMENT_ID -> document.id
                    DocumentsContract.Document.COLUMN_DISPLAY_NAME -> document.displayName
                    DocumentsContract.Document.COLUMN_MIME_TYPE -> document.mimeType
                    else -> null
                }
            }.toTypedArray())
        }

        fun addExistingDocument(name: String, bytes: ByteArray) {
            val id = "document-${documents.size}"
            documents[id] = SyntheticDocument(id, name, "text/plain")
            replaceBytes(id, bytes)
        }

        fun allDocuments(): List<SyntheticDocument> = documents.values.toList()
        fun inboxDocuments(): List<SyntheticDocument> = documents.values.filter { it.displayName in setOf("ai-inbox.org", "ai-inbox.org.txt") }
        fun bytes(id: String): ByteArray = storage[id]?.readBytes() ?: byteArrayOf()
        fun onlyInboxBytes(): ByteArray = storage[inboxDocuments().single().id]?.readBytes() ?: byteArrayOf()
        fun replaceBytes(id: String, bytes: ByteArray) {
            val file = storage.getOrPut(id) { File(storageDirectory, "synthetic-org-${UUID.randomUUID()}") }
            file.writeBytes(bytes)
        }

        companion object {
            const val AUTHORITY = "synthetic.org.documents"
            const val ROOT_ID = "synthetic-root"
        }
    }
}
