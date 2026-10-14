package dev.gitblame.voxgolem.capture

import android.content.Context
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.DocumentsProvider
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowContentResolver
import java.io.File
import java.io.FilterOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.security.MessageDigest

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [25])
class SafAudioArchiveTest {
    private val context = RuntimeEnvironment.getApplication()
    private val id = "123e4567-e89b-42d3-a456-426614174093"
    private val tree = DocumentsContract.buildTreeDocumentUri("synthetic.audio.documents", "root")
    private val destination = NotesDestination("synthetic-audio-destination", tree)
    private lateinit var provider: HierarchicalAudioProvider
    private lateinit var wav: File
    private lateinit var hash: String

    @Before fun setUp() {
        context.getSharedPreferences("capture_notes", Context.MODE_PRIVATE).edit().clear().commit()
        provider = HierarchicalAudioProvider(context.cacheDir)
        provider.attachInfo(context, android.content.pm.ProviderInfo().apply {
            authority = "synthetic.audio.documents"; exported = true; grantUriPermissions = true
            readPermission = "android.permission.MANAGE_DOCUMENTS"; writePermission = "android.permission.MANAGE_DOCUMENTS"
        })
        ShadowContentResolver.registerProviderInternal("synthetic.audio.documents", provider)
        val bytes = ByteBuffer.allocate(48).order(ByteOrder.LITTLE_ENDIAN)
            .put("RIFF".toByteArray()).putInt(40).put("WAVEfmt ".toByteArray()).putInt(16)
            .putShort(1).putShort(1).putInt(16_000).putInt(32_000).putShort(2).putShort(16)
            .put("data".toByteArray()).putInt(4).putShort(1).putShort(2).array()
        wav = File(context.cacheDir, "synthetic-saf-source.wav").apply { writeBytes(bytes) }
        hash = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    }
    @After fun tearDown() { ShadowContentResolver.reset() }
    private fun archive(store: SafNotesStore = SafNotesStore(context)) = store.archiveAudio(destination, id, wav, hash, 48)

    @Test fun actualArchiveCreatesRecordingUnderRecordingsAndReopenedStoreDoesNotDuplicateIt() {
        val uri = archive()
        val audioId = DocumentsContract.getDocumentId(uri)
        assertArrayEquals(wav.readBytes(), provider.bytes(audioId))
        assertEquals("recordings", provider.parentName(audioId))
        assertTrue(provider.isChildDocument("root", provider.parentId(audioId)))
        assertTrue("Tree membership follows the complete root/recordings/WAV hierarchy", provider.isChildDocument("root", audioId))
        assertEquals(uri, archive(SafNotesStore(context)))
        assertEquals(1, provider.audioCount())
        assertEquals(1, provider.directoryCount())
        assertFalse(provider.isChildDocument("synthetic-unknown-parent", audioId))
        assertFalse(provider.isChildDocument("root", "synthetic-unknown-document"))
        assertFalse(provider.isChildDocument(audioId, provider.parentId(audioId)))
        val outside = provider.seedUnrelatedAudio(byteArrayOf(11, 22, 33))
        assertFalse(provider.isChildDocument("root", outside))
        val outsideUri = DocumentsContract.buildDocumentUriUsingTree(tree, outside)
        assertThrows(SecurityException::class.java) {
            context.contentResolver.query(outsideUri, arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID), null, null, null)?.close()
        }
        val unknownUri = DocumentsContract.buildDocumentUriUsingTree(tree, "synthetic-unknown-document")
        assertThrows(SecurityException::class.java) {
            context.contentResolver.query(unknownUri, arrayOf(DocumentsContract.Document.COLUMN_DOCUMENT_ID), null, null, null)?.close()
        }
    }

    @Test fun partialCopyRetainsJournalAndReopenedStoreRepairsOnlyItsExactDocument() {
        var failed = false
        val store = SafNotesStore(context, openArchiveOutput = { uri ->
            val actual = context.contentResolver.openOutputStream(uri, "wt")!!
            object : FilterOutputStream(actual) {
                override fun write(bytes: ByteArray, offset: Int, length: Int) {
                    if (!failed) { failed = true; out.write(bytes, offset, 12); throw IOException("Synthetic partial SAF write") }
                    out.write(bytes, offset, length)
                }
            }
        })
        assertThrows(IOException::class.java) { archive(store) }
        val target = provider.onlyAudio()
        assertEquals(12, provider.bytes(target).size)
        assertTrue(journalExists())
        val repaired = archive(SafNotesStore(context))
        assertEquals(target, DocumentsContract.getDocumentId(repaired))
        assertArrayEquals(wav.readBytes(), provider.bytes(target))
        assertEquals(1, provider.audioCount())
    }

    @Test fun corruptReadbackFailsThenFreshStoreRetriesTheJournaledTarget() {
        provider.corruptNextRead = true
        assertThrows(IOException::class.java) { archive() }
        assertTrue(journalExists())
        assertFalse(wav.readBytes().contentEquals(provider.bytes(provider.onlyAudio())))
        val recovered = archive(SafNotesStore(context))
        assertArrayEquals(wav.readBytes(), provider.bytes(DocumentsContract.getDocumentId(recovered)))
        assertEquals(1, provider.audioCount())
    }

    @Test fun unjournaledMatchingExistingAudioIsVerifiedWithoutWriteButConflictIsPreserved() {
        val matching = provider.seedAudio("$id.wav", wav.readBytes())
        val uri = archive()
        assertEquals(matching, DocumentsContract.getDocumentId(uri))
        assertEquals(0, provider.writes)
        val different = byteArrayOf(11, 22, 33)
        provider.replace(matching, different)
        assertThrows(IOException::class.java) { archive(SafNotesStore(context)) }
        assertArrayEquals(different, provider.bytes(matching))
        assertEquals(0, provider.writes)
        assertFalse(journalExists())
    }

    @Test fun journalCommitFailureNeverWritesOrClaimsTheUnjournaledCreatedDocument() {
        assertThrows(IOException::class.java) { archive(SafNotesStore(context, commitArchiveJournal = { _, _ -> false })) }
        assertFalse(journalExists())
        assertEquals(0, provider.writes)
        assertEquals(1, provider.audioCount())
        assertEquals(0, provider.bytes(provider.onlyAudio()).size)
        assertThrows(IOException::class.java) { archive(SafNotesStore(context)) }
        assertEquals(1, provider.audioCount())
        assertEquals(0, provider.writes)
    }

    @Test fun revokedWriteOrReadPermissionsNeverProduceVerifiedSuccessAndRetryKeepsIdentity() {
        provider.denyWrite = true
        assertThrows(SecurityException::class.java) { archive() }
        val target = provider.onlyAudio()
        assertTrue(journalExists())
        provider.denyWrite = false; provider.denyRead = true
        assertThrows(SecurityException::class.java) { archive(SafNotesStore(context)) }
        provider.denyRead = false
        assertEquals(target, DocumentsContract.getDocumentId(archive(SafNotesStore(context))))
        assertArrayEquals(wav.readBytes(), provider.bytes(target))
        assertEquals(1, provider.audioCount())
    }

    private fun journalExists() = context.getSharedPreferences("capture_notes", Context.MODE_PRIVATE).all.keys.any { it.startsWith("audio.") }

    /** Real SAF query/create/open entry points with actual hierarchy and isolated synthetic bytes. */
    private class HierarchicalAudioProvider(private val root: File) : DocumentsProvider() {
        private data class Document(val id: String, val parent: String?, val name: String, val mime: String, val file: File?)
        private val documents = linkedMapOf("root" to Document("root", null, "Synthetic root", DocumentsContract.Document.MIME_TYPE_DIR, null))
        var denyWrite = false
        var denyRead = false
        var corruptNextRead = false
        var writes = 0
        override fun onCreate() = true
        override fun queryRoots(projection: Array<out String>): Cursor = MatrixCursor(projection)
        override fun queryDocument(documentId: String, projection: Array<out String>): Cursor = rows(projection, listOfNotNull(documents[documentId]))
        override fun queryChildDocuments(parentDocumentId: String, projection: Array<out String>, sortOrder: String?): Cursor =
            rows(projection, documents.values.filter { it.parent == parentDocumentId })
        override fun isChildDocument(parentDocumentId: String, documentId: String): Boolean {
            if (documents[parentDocumentId]?.mime != DocumentsContract.Document.MIME_TYPE_DIR) return false
            var current = documents[documentId] ?: return false
            val visited = mutableSetOf(current.id)
            while (true) {
                val parentId = current.parent ?: return false
                val parent = documents[parentId] ?: return false
                if (!visited.add(parentId)) return false
                if (parentId == parentDocumentId) return true
                current = parent
            }
        }
        private fun rows(projection: Array<out String>, values: List<Document>): Cursor = MatrixCursor(projection).apply {
            values.forEach { document -> addRow(projection.map {
                when (it) {
                    DocumentsContract.Document.COLUMN_DOCUMENT_ID -> document.id
                    DocumentsContract.Document.COLUMN_DISPLAY_NAME -> document.name
                    DocumentsContract.Document.COLUMN_MIME_TYPE -> document.mime
                    else -> null
                }
            }.toTypedArray()) }
        }
        override fun createDocument(parentDocumentId: String, mimeType: String, displayName: String): String {
            check(documents[parentDocumentId]?.mime == DocumentsContract.Document.MIME_TYPE_DIR)
            val id = "synthetic-document-${documents.size}"
            val file = if (mimeType == DocumentsContract.Document.MIME_TYPE_DIR) null
                else File(root, "synthetic-saf-${java.util.UUID.randomUUID()}").apply { createNewFile() }
            documents[id] = Document(id, parentDocumentId, displayName, mimeType, file)
            return id
        }
        override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
            val file = requireNotNull(documents.getValue(documentId).file)
            if (mode.contains("w")) {
                if (denyWrite) throw SecurityException("Synthetic write permission revoked")
                writes++
            } else {
                if (denyRead) throw SecurityException("Synthetic read permission revoked")
                if (corruptNextRead) { corruptNextRead = false; file.writeBytes(byteArrayOf(7, 8, 9)) }
            }
            return ParcelFileDescriptor.open(file, if (mode.contains("w"))
                ParcelFileDescriptor.MODE_WRITE_ONLY or ParcelFileDescriptor.MODE_TRUNCATE else ParcelFileDescriptor.MODE_READ_ONLY)
        }
        fun seedAudio(name: String, bytes: ByteArray): String {
            val folder = createDocument("root", DocumentsContract.Document.MIME_TYPE_DIR, "recordings")
            return createDocument(folder, "audio/wav", name).also { replace(it, bytes) }
        }
        fun seedUnrelatedAudio(bytes: ByteArray): String {
            val otherRoot = "synthetic-other-root"
            documents[otherRoot] = Document(otherRoot, null, "Synthetic other root", DocumentsContract.Document.MIME_TYPE_DIR, null)
            return createDocument(otherRoot, "audio/wav", "synthetic-unrelated.wav").also { replace(it, bytes) }
        }
        fun replace(id: String, bytes: ByteArray) { documents.getValue(id).file!!.writeBytes(bytes) }
        fun bytes(id: String) = documents.getValue(id).file!!.readBytes()
        fun parentName(id: String) = documents.getValue(documents.getValue(id).parent!!).name
        fun parentId(id: String): String = requireNotNull(documents.getValue(id).parent)
        fun onlyAudio() = documents.values.single { it.mime == "audio/wav" }.id
        fun audioCount() = documents.values.count { it.mime == "audio/wav" }
        fun directoryCount() = documents.values.count { it.id != "root" && it.mime == DocumentsContract.Document.MIME_TYPE_DIR }
    }
}
