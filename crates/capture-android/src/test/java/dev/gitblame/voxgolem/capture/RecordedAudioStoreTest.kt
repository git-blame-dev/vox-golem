package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test
import org.junit.After
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.File
import java.text.SimpleDateFormat
import java.util.Locale
import java.util.TimeZone

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 35], manifest = Config.NONE)
class RecordedAudioStoreTest {
    private val context = RuntimeEnvironment.getApplication()
    private val repositories = mutableListOf<CaptureRepository>()
    private fun repository() = CaptureRepository(context).also { repositories += it }
    private fun store(source: String, clock: () -> Long = System::currentTimeMillis,
        timeZone: () -> TimeZone = TimeZone::getDefault) = RecordedAudioStore(context, source, clock, timeZone).also {
        val field = RecordedAudioStore::class.java.getDeclaredField("repository").apply { isAccessible = true }
        repositories += field.get(it) as CaptureRepository
    }
    @After fun closeDatabases() {
        val field = CaptureRepository::class.java.getDeclaredField("helper").apply { isAccessible = true }
        repositories.forEach { (field.get(it) as android.database.sqlite.SQLiteOpenHelper).close() }
    }
    @Test fun allAssistantPurposesSaveOneLocalWavWithTheCapturedPolicyAndNoExportScheduling() {
        val store = store("watch")
        for (purpose in listOf(CapturePurpose.ASK, CapturePurpose.TALK, CapturePurpose.DRAFT)) {
            val recording = store.open(purpose, "en-AU", true)
            recording.started(); recording.append(byteArrayOf(1, 0, 2, 0, 3, 0, 4, 0)); recording.checkpoint()
            val artifact = recording.finish("finished")
            assertSame(artifact, recording.finish("finished"))
            recording.recognition("Synthetic authoritative words")
            val record = repository().get(artifact.captureId)!!
            assertEquals(purpose, record.purpose); assertEquals("en-AU", record.requestedLanguage)
            assertTrue(record.speechNetworkFallback); assertEquals(CaptureState.SAVED, record.captureState)
            assertEquals(TransferState.LOCAL_ONLY, record.transferState)
            assertEquals(ArchiveState.NOT_STAGED, record.archiveState); assertNull(record.exportDestinationId)
            assertEquals(OrgState.NONE, record.orgState)
            assertEquals(RecognitionState.FINAL, record.recognitionState)
            assertEquals(4L, record.durableFrames); assertEquals(artifact.manifest.sha256, record.sha256)
            WavPcmReader(File(artifact.path), CaptureFormat()).use {
                assertEquals(4L, it.frameCount); assertArrayEquals(byteArrayOf(1, 0, 2, 0, 3, 0, 4, 0), it.read())
            }
        }
    }
    @Test fun failedFinalizationRetainsThePcmSpoolAndInterruptedMetadata() {
        val recording = store("phone").open(CapturePurpose.ASK, "en-US", false)
        recording.started(); recording.append(byteArrayOf(1, 0)); recording.checkpoint()
        val record = repository().get(recording.captureId)!!
        val spool = File(record.localAudioPath!!)
        assertTrue(File(spool.parentFile, "audio.wav").mkdir())
        assertThrows(Exception::class.java) { recording.finish("finished") }
        assertTrue(spool.isFile); assertEquals(2L, spool.length())
        assertEquals(CaptureState.INTERRUPTED, repository().get(recording.captureId)!!.captureState)
    }
    @Test fun rawRecoveryCoversEveryVoicePurposeAndLeavesNoteRowsAndAudioUntouched() {
        val repository = repository()
        val purposes = listOf(CapturePurpose.NOTE, CapturePurpose.ASK, CapturePurpose.TALK, CapturePurpose.DRAFT)
        val records = purposes.mapIndexed { index, purpose ->
            val id = "123e4567-e89b-42d3-a456-42661417400$index"
            val spool = repository.createPcmSpool(id)
            PcmRecordingWriter(spool, CaptureFormat()).use { it.append(byteArrayOf(1, 0, 2, 0)); it.checkpoint() }
            repository.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z", purpose = purpose,
                captureState = CaptureState.RECORDING, localAudioPath = spool.absolutePath,
                orgState = if (purpose == CapturePurpose.NOTE) OrgState.WAITING_FOR_TRANSCRIPT else OrgState.NONE))
        }
        val recovered = store("watch").recoverInterrupted()
        assertEquals(setOf(CapturePurpose.ASK, CapturePurpose.TALK, CapturePurpose.DRAFT), recovered.map { it.purpose }.toSet())
        assertTrue(recovered.all { it.captureState == CaptureState.INTERRUPTED && it.durableFrames == 2L })
        val note = records.first()
        val noteSpool = File(note.localAudioPath!!)
        assertEquals(note, repository.get(note.captureId))
        assertArrayEquals(byteArrayOf(1, 0, 2, 0), noteSpool.readBytes())
        assertFalse(File(noteSpool.parentFile, "audio.wav").exists())
        assertThrows(IllegalArgumentException::class.java) { store("watch").open(CapturePurpose.NOTE, "en-AU", false) }
        assertEquals(note, repository.get(note.captureId))
    }
    @Test fun utcAndLocalFinishMetadataShareOneClockSampleAndRespectTheDaylightSavingDateBoundary() {
        val utcParser = SimpleDateFormat("yyyy-MM-dd'T'HH:mm:ss.SSS'Z'", Locale.ROOT).apply {
            timeZone = TimeZone.getTimeZone("UTC")
        }
        val createdAt = "2026-03-08T00:00:00.000Z"
        for ((finishedAt, localDate) in listOf(
            "2026-03-08T04:30:00.123Z" to "2026-03-07",
            "2026-03-09T04:30:00.456Z" to "2026-03-09",
        )) {
            var now = utcParser.parse(createdAt)!!.time
            var clockReads = 0
            var zoneReads = 0
            val store = store("watch", { clockReads++; now }, {
                zoneReads++; TimeZone.getTimeZone("America/New_York")
            })
            val recording = store.open(CapturePurpose.DRAFT, "en-AU", false)
            recording.started(); recording.append(byteArrayOf(1, 0))
            now = utcParser.parse(finishedAt)!!.time
            val artifact = recording.finish("finished")
            assertSame(artifact, recording.finish("finished"))
            val record = repository().get(artifact.captureId)!!
            assertEquals(createdAt, record.createdAtUtc); assertEquals(createdAt, record.startedAtUtc)
            assertEquals(finishedAt, record.finishedAtUtc); assertEquals(localDate, record.finishedLocalDate)
            assertEquals("America/New_York", record.timezoneId)
            assertEquals(2, clockReads); assertEquals(1, zoneReads)
        }
    }
}
