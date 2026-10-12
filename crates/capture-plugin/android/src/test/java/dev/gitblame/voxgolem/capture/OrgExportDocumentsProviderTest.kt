package dev.gitblame.voxgolem.capture

import android.content.Context
import android.content.ContentValues
import android.content.ContentResolver
import android.database.Cursor
import android.database.MatrixCursor
import android.database.sqlite.SQLiteDatabase
import android.os.CancellationSignal
import android.os.Bundle
import android.net.Uri
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
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.annotation.RealObject
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
    fun journalWithoutTargetNameCannotReplayOrBlockNewAiInboxCreation() {
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
        assertEquals("provider-cannot-name-inbox-exactly-rename-to-ai-inbox.org-or-choose-another-folder", result.reason)
        assertEquals("synthetic-user-notes.org", provider.allDocuments().single().displayName)
        assertArrayEquals(original, provider.bytes(targetId))
        assertTrue(provider.bytesAtRename.isEmpty())
        assertTrue(provider.createdMimeTypes.isEmpty())
        assertEquals(0, provider.writeCount)
    }

    @Test
    @Config(sdk = [35], shadows = [DocumentsProviderQueryBridge::class])
    fun committedLegacyWritesAreAcknowledgedAcrossPhasesWithoutProviderMutationOrDuplicates() {
        provider.addExistingDocument("ai-inbox.org", "* Synthetic independent AI inbox\n".toByteArray())
        for (phase in listOf("PREPARED", "WRITING", "NEEDS_REVIEW")) {
            for (targetName in listOf(null, "inbox.org", "ai-inbox.org")) {
                for (documentName in listOf("inbox.org", "inbox.org.txt")) {
                    val (id, _) = prepareCommittedLegacyJournal(phase, targetName, documentName)
                    val journal = readJournal(id)
                    val stages = listOf("preimage", "postimage", "entry").map { File(journal.getString(it)) }
                    val documentsBefore = provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() }

                    repeat(2) {
                        assertEquals(OrgState.EXPORTED, coordinator().process(id).state)
                        val record = checkNotNull(repository.get(id))
                        assertEquals(record.transcriptVersion, record.exportTranscriptVersion)
                        assertEquals(sha256(treeUri.toString().toByteArray()), record.exportDestinationId)
                        assertEquals(documentsBefore, provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() })
                        assertTrue(provider.createdMimeTypes.isEmpty())
                        assertTrue(provider.bytesAtRename.isEmpty())
                        assertEquals(0, provider.writeCount)
                    }
                    stages.forEach { assertFalse(it.exists()) }
                    assertEquals(0, journalCount(id))
                }
            }
        }
    }

    @Test
    @Config(sdk = [35], shadows = [DocumentsProviderQueryBridge::class])
    fun legacyPreimagesAndUnknownProviderContentsRemainHeldAcrossPhases() {
        for (phase in listOf("PREPARED", "WRITING", "NEEDS_REVIEW")) {
            for (usePreimage in listOf(true, false)) {
                val (id, targetId) = prepareCommittedLegacyJournal(phase)
                val bytes = if (usePreimage) File(readJournal(id).getString("preimage")).readBytes()
                    else "* Synthetic externally changed inbox\n".toByteArray()
                provider.replaceBytes(targetId, bytes)
                assertLegacyRecoveryHeld(id)
            }
        }
    }

    @Test
    @Config(sdk = [35], shadows = [DocumentsProviderQueryBridge::class])
    fun legacyPostHashRequiresMatchingIdentityVersionPhaseAndLengths() {
        val changes: List<(JSONObject) -> Unit> = listOf(
            { it.put("captureId", UUID.randomUUID().toString()) },
            { it.put("destinationId", "synthetic-other-destination") },
            { it.put("transcriptVersion", 2) },
            { it.remove("transcriptVersion") },
            { it.put("phase", "UNKNOWN") },
            { it.remove("phase") },
            { it.remove("targetWasAbsent") },
            { it.put("targetWasAbsent", true) },
            { it.put("preLength", it.getLong("preLength") + 1) },
            { it.put("postLength", it.getLong("postLength") + 1) },
            { it.put("preHash", "synthetic-invalid-hash") },
            { it.remove("postHash") },
            { it.put("targetUri", "") },
            { it.put("targetUri", "content://synthetic.other/tree/root/document/other") },
        )
        changes.forEach { change ->
            val (id, _) = prepareCommittedLegacyJournal()
            val journal = readJournal(id).also(change)
            storeJournal(id, journal)
            assertLegacyRecoveryHeld(id)
        }
    }

    @Test
    @Config(sdk = [35], shadows = [DocumentsProviderQueryBridge::class])
    fun legacyPostHashRejectsMissingCorruptOrUnownedRecoveryStages() {
        for (field in listOf("preimage", "postimage", "entry")) {
            for (damage in listOf("missing", "corrupt", "unowned", "symlink", "alias")) {
                val (id, _) = prepareCommittedLegacyJournal()
                val journal = readJournal(id)
                val stage = File(journal.getString(field))
                when (damage) {
                    "missing" -> assertTrue(stage.delete())
                    "corrupt" -> stage.writeBytes("Synthetic corrupt stage".toByteArray())
                    "unowned", "symlink" -> {
                        val other = File(context.cacheDir, "synthetic-org-unowned-${UUID.randomUUID()}").apply { writeBytes(stage.readBytes()) }
                        if (damage == "symlink") {
                            assertTrue(stage.delete())
                            java.nio.file.Files.createSymbolicLink(stage.toPath(), other.toPath())
                        } else {
                            journal.put(field, other.absolutePath)
                            storeJournal(id, journal)
                        }
                    }
                    "alias" -> storeJournal(id, journal.put(field, "${stage.parent}/../${checkNotNull(stage.parentFile).name}/${stage.name}"))
                }
                assertLegacyRecoveryHeld(id)
            }
        }
    }

    @Test
    @Config(sdk = [35], shadows = [DocumentsProviderQueryBridge::class])
    fun legacyPostHashMustRepresentTheCurrentEntryPrependedToItsPreimage() {
        val (id, targetId) = prepareCommittedLegacyJournal()
        val journal = readJournal(id)
        val wrongPostimage = File(journal.getString("postimage")).readBytes().also { it[it.lastIndex] = '!'.code.toByte() }
        File(journal.getString("postimage")).writeBytes(wrongPostimage)
        journal.put("postLength", wrongPostimage.size).put("postHash", sha256(wrongPostimage))
        storeJournal(id, journal)
        provider.replaceBytes(targetId, wrongPostimage)
        assertLegacyRecoveryHeld(id)

        val (changedId, _) = prepareCommittedLegacyJournal()
        repository.update(changedId) { it.copy(finalText = "Synthetic different current task") }
        assertLegacyRecoveryHeld(changedId)
    }

    @Test
    @Config(sdk = [35], shadows = [DocumentsProviderQueryBridge::class])
    fun legacyPostHashWithUnknownOrUnreadableTargetRemainsHeld() {
        val (unknownId, _) = prepareCommittedLegacyJournal(documentName = "synthetic-unrelated.org")
        assertLegacyRecoveryHeld(unknownId)
        for (phase in listOf("PREPARED", "WRITING", "NEEDS_REVIEW")) {
            val (id, _) = prepareCommittedLegacyJournal(phase, targetName = "ai-inbox.org")
            provider.denyReads = true
            assertLegacyRecoveryHeld(id)
            provider.denyReads = false
            provider.hideDocumentNames = true
            assertLegacyRecoveryHeld(id)
            provider.hideDocumentNames = false
        }
    }

    @Test
    @Config(sdk = [35], shadows = [DocumentsProviderQueryBridge::class])
    fun transcriptVersionChangeDuringLegacyReadbackRetainsRecoveryEvidence() {
        val (id, _) = prepareCommittedLegacyJournal()
        provider.onRead = { repository.update(id) { it.copy(transcriptVersion = it.transcriptVersion + 1) } }
        assertLegacyRecoveryHeld(id)
        assertEquals(2L, repository.get(id)?.transcriptVersion)
    }

    @Test
    @Config(sdk = [35], shadows = [DocumentsProviderQueryBridge::class])
    fun failedLegacyMetadataCommitRetainsStagesAndCanBeAcknowledgedLater() {
        val (id, _) = prepareCommittedLegacyJournal(phase = "PREPARED")
        context.openOrCreateDatabase("captures.db", Context.MODE_PRIVATE, null).use { database ->
            database.execSQL("CREATE TRIGGER synthetic_org_commit_failure BEFORE UPDATE ON captures " +
                "WHEN NEW.body LIKE '%\"orgState\":\"EXPORTED\"%' " +
                "BEGIN SELECT RAISE(ABORT, 'Synthetic metadata commit failure'); END")
        }
        assertLegacyRecoveryHeld(id)
        context.openOrCreateDatabase("captures.db", Context.MODE_PRIVATE, null).use { database ->
            database.execSQL("DROP TRIGGER synthetic_org_commit_failure")
        }
        assertEquals(OrgState.EXPORTED, coordinator().process(id).state)
        assertEquals(0, provider.writeCount)
        assertTrue(provider.createdMimeTypes.isEmpty())
        assertTrue(provider.bytesAtRename.isEmpty())
    }

    @Test
    fun legacyPostHashRemainsHeldWhenTreeMembershipApiIsUnavailable() {
        val (id, _) = prepareCommittedLegacyJournal()
        assertLegacyRecoveryHeld(id)
    }

    @Test
    @Config(sdk = [35], shadows = [DocumentsProviderQueryBridge::class])
    fun legacyPostHashRemainsHeldWhenProviderDeniesTreeMembership() {
        val (id, _) = prepareCommittedLegacyJournal()
        provider.denyChildMembership = true
        assertLegacyRecoveryHeld(id)
    }

    private fun prepareCommittedLegacyJournal(
        phase: String = "WRITING",
        targetName: String? = null,
        documentName: String = "inbox.org",
    ): Pair<String, String> {
        val original = if (phase == "NEEDS_REVIEW") byteArrayOf()
            else byteArrayOf(0xef.toByte(), 0xbb.toByte(), 0xbf.toByte()) + "* Synthetic old inbox\r\n☃".toByteArray()
        provider.addExistingDocument(documentName, original)
        val targetId = provider.allDocuments().last().id
        val id = saveCapture("synthetic committed legacy task", "2026-10-03")
        writeJournal(id, targetId, original, phase, targetWasAbsent = original.isEmpty(), targetName = targetName)
        provider.replaceBytes(targetId, File(readJournal(id).getString("postimage")).readBytes())
        return id to targetId
    }

    private fun assertLegacyRecoveryHeld(id: String) {
        val journalBefore = readJournal(id).toString()
        val documentsBefore = provider.allDocuments().associate { it.id to provider.bytes(it.id).toList() }
        val journal = readJournal(id)
        val stages = listOf("preimage", "postimage", "entry").map { File(journal.getString(it)) } +
            listOf("preimage.bin", "postimage.bin", "entry.bin").map { File(recoveryDirectory(id), it) }
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
            assertEquals("export-journal-target-changed-requires-review", result.reason)
            assertEquals(OrgState.NEEDS_REVIEW, repository.get(id)?.orgState)
            assertEquals(journalBefore, readJournal(id).toString())
            assertArrayEquals(original, provider.bytes(targetId))
            assertEquals(documentName, provider.allDocuments().single().displayName)
            assertTrue(provider.createdMimeTypes.isEmpty())
            assertTrue(provider.bytesAtRename.isEmpty())
            assertEquals(0, provider.writeCount)
        }

        assertEquals(OrgState.EXPORTED, coordinator().process(saveCapture("synthetic independent new task", "2026-10-04")).state)
        assertArrayEquals(original, provider.bytes(targetId))
        assertEquals("ai-inbox.org", provider.inboxDocuments().single().displayName)
        assertArrayEquals(OrgExport.formatEntry("synthetic independent new task", "2026-10-04"), provider.onlyInboxBytes())
        assertEquals(journalBefore, readJournal(id).toString())
        assertEquals(recoveryFiles, recoveryFiles.keys.associateWith { File(it).readBytes().toList() })
        assertTrue(provider.bytesAtRename.isEmpty())
        assertEquals(1, provider.writeCount)
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

    @Implements(value = DocumentsProvider::class, minSdk = 26)
    class DocumentsProviderQueryBridge {
        @RealObject
        private lateinit var provider: DocumentsProvider

        // ShadowContentResolver bypasses Android's legacy-query to Bundle-query conversion.
        @Implementation(minSdk = 26)
        fun query(uri: Uri, projection: Array<out String>?, selection: String?, selectionArgs: Array<out String>?, sortOrder: String?): Cursor? {
            val arguments = Bundle().apply {
                putString(ContentResolver.QUERY_ARG_SQL_SELECTION, selection)
                putStringArray(ContentResolver.QUERY_ARG_SQL_SELECTION_ARGS, selectionArgs)
                putString(ContentResolver.QUERY_ARG_SQL_SORT_ORDER, sortOrder)
            }
            return provider.query(uri, projection, arguments, null)
        }
    }

    class SyntheticDocumentsProvider(private val storageDirectory: File) : DocumentsProvider() {
        private val documents = linkedMapOf<String, SyntheticDocument>()
        private val storage = mutableMapOf<String, File>()
        val createdMimeTypes = mutableListOf<String>()
        var appendPlainTextExtension = false
        var appendOrgExtension = false
        var denyRename = false
        var returnWrongRenameName = false
        var failReadAfterRename = false
        var denyReads = false
        var hideDocumentNames = false
        var denyChildMembership = false
        var onRead: (() -> Unit)? = null
        private var failNextRead = false
        val bytesAtRename = mutableListOf<ByteArray>()
        var writeCount = 0
            private set

        override fun onCreate() = true

        override fun queryRoots(projection: Array<out String>): Cursor = MatrixCursor(projection).apply {
            addRow(projection.map { if (it == DocumentsContract.Root.COLUMN_ROOT_ID) ROOT_ID else null }.toTypedArray())
        }

        override fun queryDocument(documentId: String, projection: Array<out String>): Cursor = MatrixCursor(projection).apply {
            val document = if (documentId == ROOT_ID) SyntheticDocument(ROOT_ID, "Synthetic notes", DocumentsContract.Document.MIME_TYPE_DIR) else documents[documentId]
            if (document != null && !hideDocumentNames) addDocumentRow(this, projection, document)
        }

        override fun queryChildDocuments(parentDocumentId: String, projection: Array<out String>, sortOrder: String?): Cursor = MatrixCursor(projection).apply {
            documents.values.filter { it.id != ROOT_ID }.forEach { addDocumentRow(this, projection, it) }
        }

        override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String {
            createdMimeTypes += mimeType
            val shouldAppendExtension = (mimeType == "text/plain" && appendPlainTextExtension) || (mimeType == ORG_DOCUMENT_MIME_TYPE && appendOrgExtension)
            val actualName = if (shouldAppendExtension && !displayName.endsWith(".txt")) "$displayName.txt" else displayName
            val id = "document-${documents.size}"
            documents[id] = SyntheticDocument(id, actualName, mimeType)
            check(parentDocumentId == ROOT_ID)
            return id
        }

        override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean =
            !denyChildMembership && parentDocumentId == ROOT_ID && documents.containsKey(documentId)

        override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
            check(documents.containsKey(documentId)) { "Unknown document ID $documentId" }
            if (!mode.contains("w")) {
                if (denyReads) throw java.io.FileNotFoundException("Synthetic read denial")
                onRead?.also { onRead = null }?.invoke()
            }
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
