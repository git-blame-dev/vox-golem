package dev.gitblame.voxgolem.capture

import android.content.ContentResolver
import android.content.ContentValues
import android.content.Context
import android.database.sqlite.SQLiteDatabase
import android.database.sqlite.SQLiteOpenHelper
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import org.json.JSONObject
import java.io.BufferedInputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.security.MessageDigest

data class NotesDestination(val id: String, val treeUri: Uri)

interface NotesDestinationStore {
    fun selected(): NotesDestination?
    fun select(treeUri: Uri): NotesDestination
}

/** SAF-backed folder preference and verified recording archive. */
class SafNotesStore(private val context: Context) : NotesDestinationStore {
    private val resolver: ContentResolver = context.contentResolver
    private val preferences = context.applicationContext.getSharedPreferences("capture_notes", Context.MODE_PRIVATE)
    private val lock = processArchiveLock

    override fun selected(): NotesDestination? {
        val encoded = preferences.getString(KEY_TREE_URI, null) ?: return null
        val uri = runCatching { Uri.parse(encoded) }.getOrNull() ?: return null
        return NotesDestination(destinationId(uri), uri)
    }

    override fun select(treeUri: Uri): NotesDestination {
        require(treeUri.scheme == "content") { "Notes destination must be a SAF content URI" }
        val flags = android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION or android.content.Intent.FLAG_GRANT_WRITE_URI_PERMISSION
        resolver.takePersistableUriPermission(treeUri, flags)
        val root = DocumentsContract.buildDocumentUriUsingTree(treeUri, DocumentsContract.getTreeDocumentId(treeUri))
        val mime = queryMime(root) ?: throw IOException("Cannot read selected notes folder")
        require(mime == DocumentsContract.Document.MIME_TYPE_DIR) { "Selected URI is not a folder" }
        preferences.edit().putString(KEY_TREE_URI, treeUri.toString()).commit().also {
            if (!it) throw IOException("Cannot persist notes folder selection")
        }
        return NotesDestination(destinationId(treeUri), treeUri)
    }

    fun archiveAudio(
        destination: NotesDestination,
        captureId: String,
        sourceWav: File,
        expectedSha256: String,
        expectedByteLength: Long,
    ): Uri = synchronized(lock) {
        require(CaptureProtocol.isValidUuid(captureId)) { "Invalid capture UUID" }
        require(sourceWav.isFile && sourceWav.length() == expectedByteLength) { "Staged WAV length does not match manifest" }
        require(sha256(sourceWav) == expectedSha256.lowercase()) { "Staged WAV hash does not match manifest" }
        val recordings = findOrCreateDirectory(destination.treeUri, "recordings")
        val name = "${captureId.lowercase()}.wav"
        val existing = children(recordings).filter { it.name == name }
        if (existing.size > 1) throw IOException("Multiple recordings named $name require manual review")
        val journalKey = "audio.${digest("${destination.id}:${captureId.lowercase()}")}"
        val journalText = preferences.getString(journalKey, null)
        val journal = journalText?.let { runCatching { JSONObject(it) }.getOrNull() }
        val target = if (existing.isNotEmpty()) {
            val child = existing.single()
            if (journal?.optString("uri") != child.uri.toString()) {
                if (contentMatches(child.uri, expectedSha256, expectedByteLength)) return@synchronized child.uri
                throw IOException("A different document already uses $name")
            }
            child.uri
        } else {
            if (journal != null) throw IOException("Previously journaled recording disappeared; folder requires review")
            val created = DocumentsContract.createDocument(resolver, recordings, "audio/wav", name)
                ?: throw IOException("Provider refused to create $name")
            val stored = JSONObject().put("uri", created.toString()).put("hash", expectedSha256.lowercase()).put("length", expectedByteLength)
            if (!preferences.edit().putString(journalKey, stored.toString()).commit()) throw IOException("Cannot journal new recording document")
            if (documentName(created) != name) throw IOException("Provider changed the requested recording name; journal retained for review")
            created
        }
        val knownJournal = journal ?: JSONObject().put("uri", target.toString()).put("hash", expectedSha256.lowercase()).put("length", expectedByteLength)
        if (knownJournal.optString("hash") != expectedSha256.lowercase() || knownJournal.optLong("length", -1) != expectedByteLength) {
            throw IOException("Journaled recording belongs to a different archive attempt")
        }
        resolver.openOutputStream(target, "wt")?.use { output ->
            FileInputStream(sourceWav).use { input -> input.copyTo(output, COPY_BUFFER_BYTES) }
        } ?: throw IOException("Provider cannot open recording for replacement")
        if (!contentMatches(target, expectedSha256, expectedByteLength)) throw IOException("Archived recording failed read-back verification")
        return@synchronized target
    }

    private data class Child(val uri: Uri, val name: String, val mime: String)

    private fun findOrCreateDirectory(treeUri: Uri, name: String): Uri {
        val treeId = DocumentsContract.getTreeDocumentId(treeUri)
        val root = DocumentsContract.buildDocumentUriUsingTree(treeUri, treeId)
        val matches = children(root).filter { it.name == name && it.mime == DocumentsContract.Document.MIME_TYPE_DIR }
        if (matches.size > 1) throw IOException("Multiple $name folders require manual review")
        if (matches.size == 1) return matches.single().uri
        if (children(root).any { it.name == name }) throw IOException("A non-folder document is named $name")
        val created = DocumentsContract.createDocument(resolver, root, DocumentsContract.Document.MIME_TYPE_DIR, name)
            ?: throw IOException("Provider refused to create $name folder")
        if (documentName(created) != name || queryMime(created) != DocumentsContract.Document.MIME_TYPE_DIR) {
            throw IOException("Provider did not create the requested $name folder")
        }
        return created
    }

    private fun children(parent: Uri): List<Child> {
        val id = DocumentsContract.getDocumentId(parent)
        val childUri = DocumentsContract.buildChildDocumentsUriUsingTree(parent, id)
        val result = ArrayList<Child>()
        resolver.query(childUri, arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE), null, null, null)
            ?.use { cursor ->
                val idColumn = cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DOCUMENT_ID)
                val nameColumn = cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DISPLAY_NAME)
                val mimeColumn = cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_MIME_TYPE)
                while (cursor.moveToNext()) {
                    val childId = cursor.getString(idColumn) ?: throw IOException("Provider returned a nameless document")
                    result += Child(
                        DocumentsContract.buildDocumentUriUsingTree(parent, childId),
                        cursor.getString(nameColumn) ?: "",
                        cursor.getString(mimeColumn) ?: "",
                    )
                }
            } ?: throw IOException("Cannot enumerate selected notes folder")
        return result
    }

    private fun documentName(uri: Uri): String? {
        resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) return cursor.getString(cursor.getColumnIndexOrThrow(OpenableColumns.DISPLAY_NAME))
        } ?: throw IOException("Cannot inspect provider document")
        return null
    }

    private fun queryMime(uri: Uri): String? {
        resolver.query(uri, arrayOf(DocumentsContract.Document.COLUMN_MIME_TYPE), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) return cursor.getString(cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_MIME_TYPE))
        } ?: throw IOException("Cannot inspect provider document")
        return null
    }

    private fun contentMatches(uri: Uri, expectedHash: String, expectedLength: Long): Boolean {
        val input = resolver.openInputStream(uri) ?: throw IOException("Provider cannot read archived recording")
        val digest = MessageDigest.getInstance("SHA-256")
        var length = 0L
        input.use { stream ->
            val buffer = ByteArray(COPY_BUFFER_BYTES)
            while (true) {
                val count = stream.read(buffer)
                if (count < 0) break
                length += count
                digest.update(buffer, 0, count)
            }
        }
        return length == expectedLength && digest.digest().hex() == expectedHash.lowercase()
    }

    companion object {
        private const val KEY_TREE_URI = "selected_tree_uri"
        private const val COPY_BUFFER_BYTES = 64 * 1024
        private val processArchiveLock = Any()

        private fun destinationId(uri: Uri): String = digest(uri.toString())
        private fun digest(value: String): String = MessageDigest.getInstance("SHA-256").digest(value.toByteArray(Charsets.UTF_8)).hex()
        private fun sha256(file: File): String {
            val digest = MessageDigest.getInstance("SHA-256")
            BufferedInputStream(FileInputStream(file)).use { input ->
                val buffer = ByteArray(COPY_BUFFER_BYTES)
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    digest.update(buffer, 0, count)
                }
            }
            return digest.digest().hex()
        }
        private fun ByteArray.hex() = joinToString("") { "%02x".format(it) }
    }
}

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
        if (record.exportDestinationId != null && record.exportDestinationId != destination.id) {
            return@synchronized markReview(captureId, record.exportDestinationId, "export-destination-changed-during-pending-write")
        }
        if (record.exportDestinationId == null) {
            repository.update(captureId) { it.copy(orgState = OrgState.WRITING, exportDestinationId = destination.id) }
        }
        val journalKey = journalKey(captureId, destination.id)
        val journal = journalDatabase.get(captureId, destination.id)
        try {
            if (journal != null) {
                if (journal.optString("phase") == "NEEDS_REVIEW") {
                    return@synchronized markReview(captureId, destination.id, "inbox-creation-requires-review")
                }
                if (journal.getLong("transcriptVersion") != record.transcriptVersion) {
                    return@synchronized markReview(captureId, destination.id, "transcript-changed-during-export")
                }
                val target = journal.optString("targetUri").takeIf(String::isNotEmpty)?.let(Uri::parse)
                    ?: recoverOrCreateJournaledInbox(destination.treeUri, journal)
                    ?: return@synchronized markReview(captureId, destination.id, "created-inbox-identity-is-ambiguous")
                val actualHash = hashDocument(target)
                return@synchronized when (actualHash) {
                    journal.getString("postHash") -> commitExport(captureId, destination.id, record.transcriptVersion)
                    journal.getString("preHash") -> writePrepared(captureId, record, destination.id, target, journal)
                    else -> markReview(captureId, destination.id, "inbox-changed-after-export-preparation")
                }
            }

            val target = findInbox(destination.treeUri)
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
            val targetUri = target ?: createJournaledInbox(destination.treeUri, prepared)
            if (hashDocument(targetUri) != prepared.getString("preHash")) {
                journalDatabase.remove(captureId, destination.id)
                return@synchronized retryable(captureId, destination.id, "inbox-changed-before-write")
            }
            writePrepared(captureId, record, destination.id, targetUri, prepared)
        } catch (failure: SecurityException) {
            if (journalHasStartedWriting(captureId, destination.id)) markReview(captureId, destination.id, "saf-permission-revoked-during-write")
            else retryable(captureId, destination.id, "saf-permission-revoked")
        } catch (failure: OrgReviewException) {
            markReview(captureId, destination.id, failure.message ?: "inbox-creation-requires-review")
        } catch (failure: Exception) {
            if (journalHasStartedWriting(captureId, destination.id)) markReview(captureId, destination.id, "provider-failure-during-write")
            else retryable(captureId, destination.id, failure.javaClass.simpleName.lowercase())
        }
    }

    private fun writePrepared(
        captureId: String,
        record: CaptureRecord,
        destinationId: String,
        target: Uri,
        journal: JSONObject,
    ): OrgExportResult {
        val latest = repository.get(captureId)
            ?: return OrgExportResult(OrgState.RETRYABLE_ERROR, destinationId, "capture-disappeared")
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

    private fun findInbox(treeUri: Uri): Uri? {
        val rootId = DocumentsContract.getTreeDocumentId(treeUri)
        val root = DocumentsContract.buildDocumentUriUsingTree(treeUri, rootId)
        val childUri = DocumentsContract.buildChildDocumentsUriUsingTree(root, rootId)
        val matches = ArrayList<Pair<Uri, String>>()
        resolver.query(childUri, arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID, DocumentsContract.Document.COLUMN_DISPLAY_NAME, DocumentsContract.Document.COLUMN_MIME_TYPE), null, null, null)
            ?.use { cursor ->
                val idIndex = cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DOCUMENT_ID)
                val nameIndex = cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_DISPLAY_NAME)
                val mimeIndex = cursor.getColumnIndexOrThrow(DocumentsContract.Document.COLUMN_MIME_TYPE)
                while (cursor.moveToNext()) {
                    val name = cursor.getString(nameIndex) ?: ""
                    if (name == "inbox.org") {
                        if (cursor.getString(mimeIndex) == DocumentsContract.Document.MIME_TYPE_DIR) throw IOException("inbox.org is a folder, not a file")
                        matches += DocumentsContract.buildDocumentUriUsingTree(root, cursor.getString(idIndex)) to name
                    }
                }
            } ?: throw IOException("Cannot enumerate notes folder")
        if (matches.size > 1) throw IOException("Multiple inbox.org documents require review")
        if (matches.size == 1) return matches.single().first
        return null
    }

    private fun createJournaledInbox(treeUri: Uri, journal: JSONObject): Uri {
        val rootId = DocumentsContract.getTreeDocumentId(treeUri)
        val root = DocumentsContract.buildDocumentUriUsingTree(treeUri, rootId)
        val created = DocumentsContract.createDocument(resolver, root, "text/plain", "inbox.org")
            ?: throw IOException("Provider refused to create inbox.org")
        val actualName = resolver.query(created, arrayOf(android.provider.OpenableColumns.DISPLAY_NAME), null, null, null)?.use { cursor ->
            if (cursor.moveToFirst()) cursor.getString(cursor.getColumnIndexOrThrow(android.provider.OpenableColumns.DISPLAY_NAME)) else null
        } ?: throw IOException("Cannot verify created inbox.org")
        journal.put("targetUri", created.toString())
        journal.put("phase", if (actualName == "inbox.org") "PREPARED" else "NEEDS_REVIEW")
        journalDatabase.put(journal.getString("captureId"), journal.getString("destinationId"), journal)
        if (actualName != "inbox.org") throw OrgReviewException("Provider renamed inbox.org to $actualName")
        return created
    }

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
        val input = resolver.openInputStream(uri) ?: throw IOException("Cannot read inbox.org")
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
        private val processExportLock = Any()
        private fun ByteArray.hex() = joinToString("") { "%02x".format(it) }
    }
}

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
