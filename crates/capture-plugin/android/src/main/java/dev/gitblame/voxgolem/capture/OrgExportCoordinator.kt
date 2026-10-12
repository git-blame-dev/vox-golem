package dev.gitblame.voxgolem.capture

import android.content.ContentValues
import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteOpenHelper
import android.net.Uri
import android.os.Build
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import org.json.JSONObject
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.io.OutputStream
import java.security.MessageDigest

data class OrgExportResult(val state: OrgState, val destinationId: String?, val reason: String? = null)

/** Serialized, image-journaled inbox prepend. A provider that cannot be read back is never reported exported. */
class OrgExportCoordinator(context: Context) {
    private val appContext = context.applicationContext
    private val resolver = appContext.contentResolver
    private val repository = CaptureRepository(appContext)
    private val destinations = SafNotesStore(appContext)
    private val journalDatabase = OrgJournalDatabase(appContext)
    private val lock = processExportLock

    fun process(captureId: String): OrgExportResult = synchronized(lock) {
        val record = repository.get(captureId) ?: return@synchronized OrgExportResult(OrgState.RETRYABLE_ERROR, null, "capture-not-found")
        if (record.purpose != CapturePurpose.NOTE) return@synchronized OrgExportResult(OrgState.NONE, null, "not-a-note")
        if (record.orgState == OrgState.EXPORTED) return@synchronized OrgExportResult(OrgState.EXPORTED, record.exportDestinationId)
        if (record.recognitionState == RecognitionState.FINAL && record.finalText.isNullOrBlank()) {
            repository.update(captureId) { it.copy(recognitionState = RecognitionState.NO_SPEECH, orgState = OrgState.WAITING_FOR_TRANSCRIPT) }
            return@synchronized OrgExportResult(OrgState.WAITING_FOR_TRANSCRIPT, record.exportDestinationId, "no-speech")
        }
        if (record.captureState != CaptureState.SAVED || record.recognitionState != RecognitionState.FINAL || record.finalText.isNullOrBlank()) {
            val state = OrgState.WAITING_FOR_TRANSCRIPT
            repository.update(captureId) { it.copy(orgState = state) }
            return@synchronized OrgExportResult(state, record.exportDestinationId, "capture-is-not-an-exportable-final-note")
        }
        if (record.archiveState != ArchiveState.VERIFIED || record.archivedAudioUri == null) {
            return@synchronized OrgExportResult(OrgState.WAITING_FOR_FOLDER, null, "audio-archive-not-verified")
        }
        val destination = destinations.selected()
            ?: return@synchronized OrgExportResult(OrgState.WAITING_FOR_FOLDER, null, "notes-folder-not-selected")
        val exportDestinationId = record.exportDestinationId
        if (exportDestinationId != null && exportDestinationId != destination.id) {
            return@synchronized markReview(captureId, exportDestinationId, "export-destination-changed-during-pending-write")
        }
        if (record.exportDestinationId == null) {
            repository.update(captureId) { it.copy(orgState = OrgState.WRITING, exportDestinationId = destination.id) }
        }
        val journalKey = journalKey(captureId, destination.id)
        val journal = journalDatabase.get(captureId, destination.id)
        try {
            if (journal != null) {
                if (!runCatching { journalTargetsCurrentInbox(journal) }.getOrDefault(false)) {
                    if (runCatching { hasConfirmedLegacyPostimage(destination, journal, record) }.getOrDefault(false)) {
                        return@synchronized try {
                            commitExport(captureId, destination.id, record.transcriptVersion)
                        } catch (_: Exception) {
                            markReview(captureId, destination.id, "legacy-export-metadata-commit-failed")
                        }
                    }
                    return@synchronized markReview(captureId, destination.id, "export-journal-target-changed-requires-review")
                }
                if (journal.getLong("transcriptVersion") != record.transcriptVersion) {
                    return@synchronized markReview(captureId, destination.id, "transcript-changed-during-export")
                }
                if (journal.optString("phase") == "NEEDS_REVIEW") {
                    if (recoverTextExtensionReview(destination.treeUri, journal, record)) {
                        journal.put("phase", "PREPARED")
                        journalDatabase.put(captureId, destination.id, journal)
                    } else {
                        return@synchronized markReview(captureId, destination.id, "inbox-creation-requires-review")
                    }
                }
                val knownTarget = journal.optString("targetUri").takeIf(String::isNotEmpty)?.let(Uri::parse)
                    ?: recoverOrCreateJournaledInbox(destination.treeUri, journal)
                    ?: return@synchronized markReview(captureId, destination.id, "created-inbox-identity-is-ambiguous")
                val target = ensureExactInboxName(destination.treeUri, knownTarget, journal)
                val actualHash = hashDocument(target)
                return@synchronized when (actualHash) {
                    journal.getString("postHash") -> commitExport(captureId, destination.id, record.transcriptVersion)
                    journal.getString("preHash") -> writePrepared(captureId, record, destination.id, target, journal)
                    else -> markReview(captureId, destination.id, "inbox-changed-after-export-preparation")
                }
            }

            val target = findInbox(destination.treeUri)
            if (target == null && journalDatabase.hasPendingDestination(destination.id, ::journalTargetsCurrentInbox)) {
                return@synchronized markReview(captureId, destination.id, "pending-inbox-creation-requires-review")
            }
            val captureDirectory = File(File(appContext.filesDir, "capture-org"), journalKey)
            if (!captureDirectory.exists() && !captureDirectory.mkdirs()) throw IOException("Cannot create private Org recovery directory")
            val preimage = File(captureDirectory, "preimage.bin")
            val postimage = File(captureDirectory, "postimage.bin")
            val entryFile = File(captureDirectory, "entry.bin")
            val entry = OrgExport.formatEntry(record.finalText!!, record.finishedLocalDate
                ?: throw IOException("Capture finish-local-date is missing"))
            FileOutputStream(entryFile).use { it.write(entry); it.fd.sync() }
            if (target == null) FileOutputStream(preimage).use { it.fd.sync() } else copyDocument(target, preimage)
            createPostimage(preimage, entry, postimage)
            val prepared = JSONObject()
                .put("phase", "PREPARED")
                .put("captureId", captureId)
                .put("destinationId", destination.id)
                .put("transcriptVersion", record.transcriptVersion)
                .put("targetName", ORG_INBOX_NAME)
                .put("targetUri", target?.toString() ?: "")
                .put("targetWasAbsent", target == null)
                .put("preimage", preimage.absolutePath)
                .put("postimage", postimage.absolutePath)
                .put("entry", entryFile.absolutePath)
                .put("preHash", hashFile(preimage))
                .put("postHash", hashFile(postimage))
                .put("preLength", preimage.length())
                .put("postLength", postimage.length())
            journalDatabase.put(captureId, destination.id, prepared)
            val targetUri = if (target == null) createJournaledInbox(destination.treeUri, prepared)
                else ensureExactInboxName(destination.treeUri, target, prepared)
            if (hashDocument(targetUri) != prepared.getString("preHash")) {
                journalDatabase.remove(captureId, destination.id)
                return@synchronized retryable(captureId, destination.id, "inbox-changed-before-write")
            }
            writePrepared(captureId, record, destination.id, targetUri, prepared)
        } catch (failure: SecurityException) {
            if (journalHasStartedWriting(captureId, destination.id)) markReview(captureId, destination.id, "saf-permission-revoked-during-write")
            else retryable(captureId, destination.id, "saf-permission-revoked")
        } catch (failure: OrgReviewException) {
            markReview(captureId, destination.id, CaptureErrorCodes.safe(failure.message, "inbox-creation-requires-review"))
        } catch (failure: Exception) {
            if (journalHasStartedWriting(captureId, destination.id)) markReview(captureId, destination.id, "provider-failure-during-write")
            else retryable(captureId, destination.id, "provider-failure-before-write")
        }
    }

    private fun writePrepared(
        captureId: String,
        record: CaptureRecord,
        destinationId: String,
        target: Uri,
        journal: JSONObject,
    ): OrgExportResult {
        if (!isInboxDocumentName(queryDocumentName(target))) throw OrgReviewException(EXACT_INBOX_NAME_ERROR)
        val latest = repository.get(captureId)
            ?: return OrgExportResult(OrgState.RETRYABLE_ERROR, destinationId, "capture-disappeared")
        if (latest.purpose != CapturePurpose.NOTE) return OrgExportResult(OrgState.NONE, null, "not-a-note")
        if (latest.transcriptVersion != journal.getLong("transcriptVersion") || latest.captureState != CaptureState.SAVED) {
            return markReview(captureId, destinationId, "capture-changed-before-inbox-write")
        }
        if (hashDocument(target) != journal.getString("preHash")) return markReview(captureId, destinationId, "inbox-preimage-no-longer-matches")
        journal.put("phase", "WRITING")
        journalDatabase.put(captureId, destinationId, journal)
        val stagedPostimage = File(journal.getString("postimage"))
        resolver.openOutputStream(target, "wt")?.use { output ->
            FileInputStream(stagedPostimage).use { input -> input.copyTo(output, COPY_BUFFER_BYTES) }
        } ?: throw IOException("Provider cannot replace inbox document")
        if (hashDocument(target) != journal.getString("postHash")) {
            return markReview(captureId, destinationId, "inbox-readback-does-not-match-postimage")
        }
        if (record.transcriptVersion != journal.getLong("transcriptVersion")) {
            return markReview(captureId, destinationId, "transcript-changed-after-inbox-write")
        }
        return commitExport(captureId, destinationId, record.transcriptVersion)
    }

    private fun commitExport(captureId: String, destinationId: String, transcriptVersion: Long): OrgExportResult {
        val updated = repository.update(captureId) { current ->
            if (current.transcriptVersion != transcriptVersion) current.copy(orgState = OrgState.NEEDS_REVIEW)
            else current.copy(
                orgState = OrgState.EXPORTED,
                exportTranscriptVersion = transcriptVersion,
                exportDestinationId = destinationId,
                lastErrorStage = null,
                lastErrorCode = null,
            )
        } ?: return OrgExportResult(OrgState.RETRYABLE_ERROR, destinationId, "capture-disappeared")
        if (updated.orgState == OrgState.EXPORTED) {
            val journal = journalDatabase.get(captureId, destinationId)
            journal?.let { staged ->
                listOf("preimage", "postimage", "entry").forEach { staged.optString(it).takeIf(String::isNotEmpty)?.let(::File)?.delete() }
                journalDatabase.remove(captureId, destinationId)
            }
            return OrgExportResult(OrgState.EXPORTED, destinationId)
        }
        return OrgExportResult(OrgState.NEEDS_REVIEW, destinationId, "transcript-version-changed")
    }

    private fun journalHasStartedWriting(captureId: String, destinationId: String): Boolean =
        journalDatabase.get(captureId, destinationId)?.optString("phase") in setOf("WRITING", "NEEDS_REVIEW")

    private class OrgReviewException(message: String) : IOException(message)

    private fun journalTargetsCurrentInbox(journal: JSONObject): Boolean {
        if (journal.optString("targetName") != ORG_INBOX_NAME) return false
        val target = journal.optString("targetUri").takeIf(String::isNotEmpty)?.let(Uri::parse) ?: return true
        return queryDocumentName(target) !in setOf("inbox.org", "inbox.org.txt")
    }

    private fun hasConfirmedLegacyPostimage(destination: NotesDestination, journal: JSONObject, record: CaptureRecord): Boolean {
        if (journal.getString("captureId") != record.captureId || journal.getString("destinationId") != destination.id) return false
        if (journal.getLong("transcriptVersion") != record.transcriptVersion) return false
        if (journal.getString("phase") !in setOf("PREPARED", "WRITING", "NEEDS_REVIEW")) return false
        val target = journal.getString("targetUri").takeIf(String::isNotEmpty)?.let(Uri::parse) ?: return false
        if (target.scheme != "content" || target.authority != destination.treeUri.authority) return false
        if (DocumentsContract.getTreeDocumentId(target) != DocumentsContract.getTreeDocumentId(destination.treeUri)) return false
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) return false
        val root = DocumentsContract.buildDocumentUriUsingTree(destination.treeUri, DocumentsContract.getTreeDocumentId(destination.treeUri))
        if (!DocumentsContract.isChildDocument(resolver, root, target)) return false
        if (queryDocumentName(target) !in setOf("inbox.org", "inbox.org.txt")) return false

        val directory = File(File(appContext.filesDir, "capture-org"), journalKey(record.captureId, destination.id))
        val canonicalDirectory = File(File(appContext.filesDir.canonicalFile, "capture-org"), directory.name)
        if (directory.canonicalFile != canonicalDirectory) return false
        fun ownedStage(field: String, name: String): File? {
            val staged = File(journal.getString(field))
            val expected = File(directory, name)
            if (staged.absoluteFile != expected.absoluteFile || staged.canonicalFile != File(canonicalDirectory, name)) return null
            return staged.takeIf(File::isFile)
        }
        val preimage = ownedStage("preimage", "preimage.bin") ?: return false
        val postimage = ownedStage("postimage", "postimage.bin") ?: return false
        val entry = ownedStage("entry", "entry.bin") ?: return false
        val preLength = journal.getLong("preLength")
        val postLength = journal.getLong("postLength")
        if (preLength < 0 || preimage.length() != preLength || postimage.length() != postLength) return false
        if (journal.getBoolean("targetWasAbsent") && preLength != 0L) return false
        if (hashFile(preimage) != journal.getString("preHash") || hashFile(postimage) != journal.getString("postHash")) return false
        val expectedEntry = OrgExport.formatEntry(record.finalText ?: return false, record.finishedLocalDate ?: return false)
        if (entry.length() != expectedEntry.size.toLong() || !entry.readBytes().contentEquals(expectedEntry)) return false
        if (preLength > Long.MAX_VALUE - expectedEntry.size || postLength != preLength + expectedEntry.size) return false
        val digest = MessageDigest.getInstance("SHA-256")
        val hashOutput = object : OutputStream() {
            override fun write(value: Int) = digest.update(value.toByte())
            override fun write(bytes: ByteArray, offset: Int, length: Int) = digest.update(bytes, offset, length)
        }
        FileInputStream(preimage).use { OrgExport.writePrepended(expectedEntry, it, hashOutput) }
        if (digest.digest().hex() != journal.getString("postHash")) return false
        return hashDocument(target) == journal.getString("postHash")
    }

    private fun findInbox(treeUri: Uri): Uri? {
        val rootId = DocumentsContract.getTreeDocumentId(treeUri)
        val root = DocumentsContract.buildDocumentUriUsingTree(treeUri, rootId)
        val childUri = DocumentsContract.buildChildDocumentsUriUsingTree(root, rootId)
        val matches = ArrayList<Triple<Uri, String, String>>()
        resolver.query(childUri, arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE), null, null, null)
            ?.use { cursor ->
                val idIndex = cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DOCUMENT_ID)
                val nameIndex = cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DISPLAY_NAME)
                val mimeIndex = cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_MIME_TYPE)
                while (cursor.moveToNext()) {
                    val name = cursor.getString(nameIndex) ?: ""
                    if (isInboxDocumentName(name) || name == ORG_INBOX_TEXT_EXTENSION_NAME) {
                        matches += Triple(DocumentsContract.buildDocumentUriUsingTree(root, cursor.getString(idIndex)), name, cursor.getString(mimeIndex))
                    }
                }
            } ?: throw IOException("Cannot enumerate notes folder")
        val canonical = matches.filter { isInboxDocumentName(it.second) }
        val candidates = canonical.ifEmpty { matches }
        if (candidates.size > 1) throw OrgReviewException("multiple-inbox-documents-require-review")
        val selected = candidates.singleOrNull() ?: return null
        if (selected.third == DocumentsContract.Document.MIME_TYPE_DIR) throw OrgReviewException("ai-inbox.org-is-a-folder-not-a-file")
        return selected.first
    }

    private fun createJournaledInbox(treeUri: Uri, journal: JSONObject): Uri {
        val rootId = DocumentsContract.getTreeDocumentId(treeUri)
        val root = DocumentsContract.buildDocumentUriUsingTree(treeUri, rootId)
        val created = DocumentsContract.createDocument(resolver, root, ORG_DOCUMENT_MIME_TYPE, ORG_INBOX_NAME)
            ?: throw IOException("Provider refused to create ai-inbox.org")
        journal.put("targetUri", created.toString())
        journalDatabase.put(journal.getString("captureId"), journal.getString("destinationId"), journal)
        return ensureExactInboxName(treeUri, created, journal)
    }

    private fun ensureExactInboxName(treeUri: Uri, target: Uri, journal: JSONObject): Uri {
        val selected = findInbox(treeUri)
        if (selected != null && selected != target) throw OrgReviewException("journaled-inbox-is-not-the-unique-canonical-target")
        val name = queryDocumentName(target)
        if (isInboxDocumentName(name)) return target
        if (name != ORG_INBOX_TEXT_EXTENSION_NAME) throw OrgReviewException(EXACT_INBOX_NAME_ERROR)
        val actualHash = hashDocument(target)
        if (actualHash != journal.getString("preHash") && actualHash != journal.getString("postHash")) {
            throw OrgReviewException("inbox-changed-after-export-preparation")
        }
        val renamed = try {
            DocumentsContract.renameDocument(resolver, target, ORG_INBOX_NAME)
        } catch (failure: Exception) {
            throw OrgReviewException(EXACT_INBOX_NAME_ERROR)
        } ?: throw OrgReviewException(EXACT_INBOX_NAME_ERROR)
        journal.put("targetUri", renamed.toString())
        journalDatabase.put(journal.getString("captureId"), journal.getString("destinationId"), journal)
        if (!isInboxDocumentName(queryDocumentName(renamed))) throw OrgReviewException(EXACT_INBOX_NAME_ERROR)
        if (findInbox(treeUri) != renamed) throw OrgReviewException("journaled-inbox-is-not-the-unique-canonical-target")
        return renamed
    }

    private fun recoverTextExtensionReview(treeUri: Uri, journal: JSONObject, record: CaptureRecord): Boolean {
        if (!journal.optBoolean("targetWasAbsent", false)) return false
        if (record.captureState != CaptureState.SAVED || record.recognitionState != RecognitionState.FINAL) return false
        val target = journal.optString("targetUri").takeIf(String::isNotEmpty)?.let(Uri::parse) ?: return false
        val name = queryDocumentName(target)
        if (name != ORG_INBOX_TEXT_EXTENSION_NAME && !isInboxDocumentName(name)) return false
        if (findInbox(treeUri) != target) return false

        val preimage = journal.optString("preimage").takeIf(String::isNotEmpty)?.let(::File) ?: return false
        val postimage = journal.optString("postimage").takeIf(String::isNotEmpty)?.let(::File) ?: return false
        val entry = journal.optString("entry").takeIf(String::isNotEmpty)?.let(::File) ?: return false
        if (!preimage.isFile || !postimage.isFile || !entry.isFile) return false
        if (preimage.length() != 0L || preimage.length() != journal.optLong("preLength", -1L)) return false
        if (postimage.length() != journal.optLong("postLength", -1L)) return false
        if (hashFile(preimage) != journal.optString("preHash") || hashFile(postimage) != journal.optString("postHash")) return false
        val expectedEntry = OrgExport.formatEntry(
            record.finalText ?: return false,
            record.finishedLocalDate ?: return false,
        )
        if (!entry.readBytes().contentEquals(expectedEntry) || !postimage.readBytes().contentEquals(expectedEntry)) return false
        return hashDocument(target) == journal.optString("preHash")
    }

    private fun queryDocumentName(uri: Uri): String? = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
        if (cursor.moveToFirst()) cursor.getString(cursor.getColumnIndexOrThrow(OpenableColumns.DISPLAY_NAME)) else null
    } ?: throw IOException("Cannot inspect provider document")

    private fun recoverOrCreateJournaledInbox(
        treeUri: Uri,
        journal: JSONObject,
    ): Uri? {
        if (!journal.optBoolean("targetWasAbsent", false)) return null
        val existing = findInbox(treeUri)
        if (existing != null) return null
        return createJournaledInbox(treeUri, journal)
    }

    private fun copyDocument(uri: Uri, target: File) {
        val input = resolver.openInputStream(uri) ?: throw IOException("Cannot read ai-inbox.org")
        input.use { source -> FileOutputStream(target).use { output -> source.copyTo(output, COPY_BUFFER_BYTES); output.fd.sync() } }
    }

    private fun createPostimage(preimage: File, entry: ByteArray, postimage: File) {
        FileInputStream(preimage).use { input ->
            FileOutputStream(postimage).use { output ->
                OrgExport.writePrepended(entry, input, output)
                output.flush()
                output.fd.sync()
            }
        }
    }

    private fun hashDocument(uri: Uri): String {
        val input = resolver.openInputStream(uri) ?: throw IOException("Provider cannot reopen inbox document")
        val digest = MessageDigest.getInstance("SHA-256")
        input.use { stream ->
            val buffer = ByteArray(COPY_BUFFER_BYTES)
            while (true) {
                val count = stream.read(buffer)
                if (count < 0) break
                digest.update(buffer, 0, count)
            }
        }
        return digest.digest().hex()
    }

    private fun hashFile(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        FileInputStream(file).use { input ->
            val buffer = ByteArray(COPY_BUFFER_BYTES)
            while (true) {
                val count = input.read(buffer)
                if (count < 0) break
                digest.update(buffer, 0, count)
            }
        }
        return digest.digest().hex()
    }

    private fun markReview(captureId: String, destinationId: String, reason: String): OrgExportResult {
        repository.update(captureId) { it.copy(orgState = OrgState.NEEDS_REVIEW, exportDestinationId = destinationId, lastErrorStage = "org", lastErrorCode = reason) }
        return OrgExportResult(OrgState.NEEDS_REVIEW, destinationId, reason)
    }

    private fun retryable(captureId: String, destinationId: String, reason: String): OrgExportResult {
        val updated = repository.update(captureId) {
            if (it.retryCount >= MAX_AUTOMATIC_RETRIES) {
                it.copy(orgState = OrgState.NEEDS_REVIEW, exportDestinationId = destinationId, lastErrorStage = "org", lastErrorCode = reason)
            } else {
                it.copy(orgState = OrgState.RETRYABLE_ERROR, exportDestinationId = destinationId, lastErrorStage = "org", lastErrorCode = reason, retryCount = it.retryCount + 1)
            }
        }
        val state = updated?.orgState ?: OrgState.RETRYABLE_ERROR
        return OrgExportResult(state, destinationId, reason)
    }

    private fun journalKey(captureId: String, destinationId: String) = "journal.${MessageDigest.getInstance("SHA-256").digest("$captureId:$destinationId".toByteArray(Charsets.UTF_8)).hex()}"

    companion object {
        private const val COPY_BUFFER_BYTES = 64 * 1024
        private const val MAX_AUTOMATIC_RETRIES = 3
        private const val EXACT_INBOX_NAME_ERROR = "provider-cannot-name-inbox-exactly-rename-to-ai-inbox.org-or-choose-another-folder"
        private val processExportLock = Any()
        private fun ByteArray.hex() = joinToString("") { "%02x".format(it) }
    }
}

internal const val ORG_DOCUMENT_MIME_TYPE = "text/org"
internal const val ORG_INBOX_NAME = "ai-inbox.org"
private const val ORG_INBOX_TEXT_EXTENSION_NAME = "$ORG_INBOX_NAME.txt"
internal fun isInboxDocumentName(name: String?): Boolean = name == ORG_INBOX_NAME

/** Journals are rows keyed by the immutable capture/destination pair, not transient UI preferences. */
private class OrgJournalDatabase(context: Context) : SQLiteOpenHelper(context, "capture_org_journal.db", null, 1) {
    override fun onCreate(db: SQLiteDatabase) {
        db.execSQL("CREATE TABLE org_journal (capture_id TEXT NOT NULL, destination_id TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY(capture_id, destination_id))")
    }

    override fun onUpgrade(db: SQLiteDatabase, oldVersion: Int, newVersion: Int) {
        throw IllegalStateException("No migration path from Org journal schema $oldVersion to $newVersion")
    }

    @Synchronized
    fun get(captureId: String, destinationId: String): JSONObject? = readableDatabase.rawQuery(
        "SELECT body FROM org_journal WHERE capture_id = ? AND destination_id = ?",
        arrayOf(captureId, destinationId),
    ).use { cursor -> if (cursor.moveToFirst()) JSONObject(cursor.getString(0)) else null }

    @Synchronized
    fun hasPendingDestination(destinationId: String, targetMatches: (JSONObject) -> Boolean): Boolean = readableDatabase.rawQuery(
        "SELECT body FROM org_journal WHERE destination_id = ?",
        arrayOf(destinationId),
    ).use { cursor ->
        while (cursor.moveToNext()) {
            if (targetMatches(JSONObject(cursor.getString(0)))) return@use true
        }
        false
    }

    @Synchronized
    fun put(captureId: String, destinationId: String, journal: JSONObject) {
        val values = ContentValues().apply {
            put("capture_id", captureId)
            put("destination_id", destinationId)
            put("body", journal.toString())
        }
        check(writableDatabase.insertWithOnConflict("org_journal", null, values, SQLiteDatabase.CONFLICT_REPLACE) != -1L) {
            "Cannot persist Org recovery journal"
        }
    }

    @Synchronized
    fun remove(captureId: String, destinationId: String) {
        writableDatabase.delete("org_journal", "capture_id = ? AND destination_id = ?", arrayOf(captureId, destinationId))
    }
}
