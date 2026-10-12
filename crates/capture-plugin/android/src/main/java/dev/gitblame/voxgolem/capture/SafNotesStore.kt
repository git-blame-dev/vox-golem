package dev.gitblame.voxgolem.capture

import android.content.ContentResolver
import android.content.Context
import android.net.Uri
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import org.json.JSONObject
import java.io.BufferedInputStream
import java.io.File
import java.io.FileInputStream
import java.io.IOException
import java.security.MessageDigest

data class NotesDestination(val id: String, val treeUri: Uri)

interface NotesDestinationStore {
    fun selected(): NotesDestination?
    fun select(treeUri: Uri): NotesDestination
}

/** SAF-backed folder preference and verified recording archive. */
class SafNotesStore(context: Context) : NotesDestinationStore {
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
