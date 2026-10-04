package dev.gitblame.voxgolem.capture

import android.net.Uri
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PhoneCaptureCoordinatorTest {
    private val context: android.content.Context = RuntimeEnvironment.getApplication()
    private val captureId = "123e4567-e89b-42d3-a456-426614174010"
    private val sessionId = "123e4567-e89b-42d3-a456-426614174011"
    private lateinit var repository: CaptureRepository
    private lateinit var speech: FakeSpeech
    private lateinit var exports: MutableList<String>
    private lateinit var coordinator: PhoneCaptureCoordinator

    @Before
    fun setUp() {
        context.deleteDatabase("captures.db")
        context.getDatabasePath("captures.db").parentFile?.resolve("captures.db-wal")?.delete()
        context.getDatabasePath("captures.db").parentFile?.resolve("captures.db-shm")?.delete()
        repository = CaptureRepository(context)
        speech = FakeSpeech()
        exports = CopyOnWriteArrayList()
        coordinator = PhoneCaptureCoordinator(
            context,
            speech,
            { _, _, _ -> },
            { Uri.parse("content://synthetic-notes/recording.wav") },
            { id -> exports += id; OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 0,
            runtimeReady = { true },
        )
    }

    @After
    fun tearDown() {
        coordinator.shutdownForTests()
    }

    @Test
    fun foregroundRuntimeRequiresTheExplicitVisibleSetupAction() {
        assertEquals("setup-required", coordinator.start().state)
        assertFalse(PhoneCaptureRuntimeService.isForegroundReady())
    }

    @Test
    fun retryReplaysOnlyValidatedWavDataChunkAndPersistsFullFinalHypothesis() {
        val pcm = byteArrayOf(0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x08)
        val wav = writeWav(pcm, includeAncillaryChunk = true)
        repository.save(savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED))

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertTrue("replay did not finish: ${repository.get(captureId)}", speech.finished)
        assertArrayEquals(pcm, speech.received.toByteArray())
        assertEquals(RecognitionState.RUNNING, repository.get(captureId)?.recognitionState)

        speech.events!!.onHypothesis("synthetic replay transcript", true)
        assertTrue(coordinator.awaitIdle())
        val result = repository.get(captureId)!!
        assertEquals(RecognitionState.FINAL, result.recognitionState)
        assertEquals("synthetic replay transcript", result.finalText)
        assertEquals(1L, result.transcriptVersion)
        assertEquals(listOf(captureId), exports)
    }

    @Test
    fun unsupportedExternalAudioLeavesTheSavedRecordingAvailableForAnotherRetry() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val record = savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED)
        repository.save(record)
        speech.supportsExternalAudio = false

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())

        val result = repository.get(captureId)!!
        assertEquals(RecognitionState.UNAVAILABLE, result.recognitionState)
        assertEquals("external-audio-provider-unavailable", result.lastErrorCode)
        assertTrue(File(result.localAudioPath!!).isFile)
        assertFalse(speech.started)
    }

    @Test
    fun archiveFinalizesProvisionalLiveRecordAndPreservesItsFinalTranscript() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        repository.save(
            CaptureRecord(
                captureId = captureId,
                createdAtUtc = STAMP,
                startedAtUtc = STAMP,
                captureState = CaptureState.RECORDING,
                recognitionState = RecognitionState.FINAL,
                recognitionSessionId = sessionId,
                previewText = "final synthetic note",
                finalText = "final synthetic note",
                lastRevision = 3,
                transcriptVersion = 1,
                engineId = "synthetic-engine",
                observedFrames = WavPcmReader(wav, CaptureFormat()).use { it.frameCount },
            ),
        )
        val archived = savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED)
            .copy(archiveState = ArchiveState.STAGED, transferState = TransferState.PHONE_STAGED)

        coordinator.onArchiveReceived(archived, "synthetic-node")
        assertTrue(coordinator.awaitIdle())

        val merged = repository.get(captureId)!!
        assertEquals(CaptureState.SAVED, merged.captureState)
        assertEquals(archived.sha256, merged.sha256)
        assertEquals("final synthetic note", merged.finalText)
        assertEquals("final synthetic note", merged.previewText)
        assertEquals(sessionId, merged.recognitionSessionId)
        assertEquals(RecognitionState.FINAL, merged.recognitionState)
        assertEquals("synthetic-engine", merged.engineId)
        assertEquals(ArchiveState.VERIFIED, merged.archiveState)
        assertEquals(listOf(captureId), exports)
    }

    @Test
    fun finalArrivingAfterVerifiedArchiveAlsoTriggersOrgExport() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        repository.save(savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED)
            .copy(archiveState = ArchiveState.VERIFIED, transferState = TransferState.PHONE_ARCHIVED,
                archivedAudioUri = "content://synthetic-notes/recording.wav"))

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertTrue(exports.isEmpty())
        speech.events!!.onHypothesis("arrived after archive", true)
        assertTrue(coordinator.awaitIdle())

        assertEquals(listOf(captureId), exports)
        assertEquals("arrived after archive", repository.get(captureId)?.finalText)
    }

    @Test
    fun finalizedHashConflictIsRejectedWithoutChangingDurableRow() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val stored = savedRecord(wav, captureState = CaptureState.SAVED, recognitionState = RecognitionState.QUEUED)
            .copy(sha256 = "a".repeat(64), finalizedByteLength = wav.length(), archiveState = ArchiveState.STAGED)
        repository.save(stored)
        val conflict = stored.copy(sha256 = "b".repeat(64), localAudioPath = wav.absolutePath)

        assertThrows(IllegalStateException::class.java) { coordinator.onArchiveReceived(conflict, "synthetic-node") }
        assertEquals(stored, repository.get(captureId))
    }

    @Test
    fun truncatedLiveFinalIsOnlyAPreviewUntilTheFullArchiveIsRecognized() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8))
        repository.save(CaptureRecord(
            captureId = captureId, createdAtUtc = STAMP, captureState = CaptureState.RECORDING,
            recognitionSessionId = sessionId, recognitionState = RecognitionState.FINAL,
            observedFrames = 1, finalText = "synthetic truncated prefix",
        ))
        coordinator.onArchiveReceived(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED), "synthetic-node")
        repeat(3) { assertTrue(coordinator.awaitIdle()) }
        val recovering = repository.get(captureId)!!
        assertEquals(null, recovering.finalText)
        assertEquals("synthetic truncated prefix", recovering.previewText)
        assertTrue(exports.isEmpty())
        assertArrayEquals(byteArrayOf(1, 2, 3, 4, 5, 6, 7, 8), speech.received.toByteArray())
        speech.events!!.onHypothesis("synthetic full recording transcript", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals("synthetic full recording transcript", repository.get(captureId)?.finalText)
        assertEquals(listOf(captureId), exports)
    }

    @Test
    fun interruptedArchiveRemainsInterruptedAndNeverAutomaticallyExports() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val interrupted = savedRecord(wav, CaptureState.INTERRUPTED, RecognitionState.FINAL).copy(
            finalText = "synthetic interrupted preview",
            archiveState = ArchiveState.STAGED,
            transferState = TransferState.PHONE_STAGED,
        )

        coordinator.onArchiveReceived(interrupted, "synthetic-node")
        assertTrue(coordinator.awaitIdle())

        assertEquals(CaptureState.INTERRUPTED, repository.get(captureId)?.captureState)
        assertEquals(ArchiveState.VERIFIED, repository.get(captureId)?.archiveState)
        assertTrue(exports.isEmpty())
    }

    @Test
    fun queuedArchiveAutomaticallyReplaysWhenThereIsNoActiveLiveAttempt() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        val queued = savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED)
            .copy(archiveState = ArchiveState.STAGED, transferState = TransferState.PHONE_STAGED)
        coordinator.onArchiveReceived(queued, "synthetic-node")
        repeat(3) { assertTrue(coordinator.awaitIdle()) }
        assertTrue(speech.started)
        assertTrue(speech.finished)
    }

    @Test
    fun interruptedCaptureRequiresExplicitFinalizationBeforeRecognitionAndExport() {
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        repository.save(savedRecord(wav, CaptureState.INTERRUPTED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertFalse(speech.started)

        coordinator.finalizeCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertEquals(CaptureState.SAVED, repository.get(captureId)?.captureState)
        assertTrue(speech.started)
    }

    @Test
    fun supplyingReplayFramesRefreshesTheInactivityTimeoutUntilEndOfAudio() {
        coordinator.shutdownForTests()
        coordinator = PhoneCaptureCoordinator(
            context, speech, { _, _, _ -> },
            { Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 100,
            recognitionTimeoutSeconds = 1,
            runtimeReady = { true },
        )
        val pcm = ByteArray(3_200 * 15) { (it % 127).toByte() }
        val wav = writeWav(pcm)
        repository.save(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED)
            .copy(durableFrames = pcm.size.toLong() / 2, observedFrames = pcm.size.toLong() / 2))

        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertArrayEquals(pcm, speech.received.toByteArray())
        speech.events!!.onHypothesis("synthetic long replay completed", true)
        assertTrue(coordinator.awaitIdle())
        assertEquals(RecognitionState.FINAL, repository.get(captureId)?.recognitionState)
    }

    @Test
    fun savedRecognitionWaitsForForegroundReadinessThenResumesThroughPendingWork() {
        coordinator.shutdownForTests()
        var ready = false
        coordinator = PhoneCaptureCoordinator(
            context, speech, { _, _, _ -> },
            { Uri.parse("content://synthetic-notes/recording.wav") },
            { OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 0,
            runtimeReady = { ready },
        )
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        repository.save(savedRecord(wav, CaptureState.SAVED, RecognitionState.QUEUED))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertFalse(speech.started)
        assertEquals(RecognitionState.QUEUED, repository.get(captureId)?.recognitionState)

        ready = true
        coordinator.retryPendingCaptures()
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertTrue(speech.finished)
    }

    @Test
    fun pendingArchivalAndExportResumeAfterTheDestinationBecomesAvailable() {
        coordinator.shutdownForTests()
        var folderAvailable = false
        coordinator = PhoneCaptureCoordinator(
            context, speech, { _, _, _ -> },
            { if (!folderAvailable) error("notes-folder-not-selected") else Uri.parse("content://synthetic-notes/recording.wav") },
            { id -> exports += id; OrgExportResult(OrgState.EXPORTED, "synthetic-destination") },
            replayPacingMillis = 0,
            runtimeReady = { true },
        )
        val wav = writeWav(byteArrayOf(1, 2, 3, 4))
        repository.save(savedRecord(wav, CaptureState.SAVED, RecognitionState.FINAL)
            .copy(finalText = "synthetic queued task", archiveState = ArchiveState.STAGED, archivedAudioUri = null))
        coordinator.retryCapture(captureId)
        assertTrue(coordinator.awaitIdle())
        assertEquals(ArchiveState.WAITING_FOR_FOLDER, repository.get(captureId)?.archiveState)
        assertTrue(exports.isEmpty())

        folderAvailable = true
        coordinator.retryPendingCaptures()
        repeat(2) { assertTrue(coordinator.awaitIdle()) }
        assertEquals(ArchiveState.VERIFIED, repository.get(captureId)?.archiveState)
        assertEquals(listOf(captureId), exports)
    }

    private fun savedRecord(file: File, captureState: CaptureState, recognitionState: RecognitionState): CaptureRecord {
        val frameCount = WavPcmReader(file, CaptureFormat()).use { it.frameCount }
        return CaptureRecord(
        captureId = captureId,
        createdAtUtc = STAMP,
        startedAtUtc = STAMP,
        finishedAtUtc = "2026-01-02T03:04:06Z",
        finishedLocalDate = "2026-01-02",
        timezoneId = "Etc/UTC",
        captureState = captureState,
        observedFrames = frameCount,
        durableFrames = frameCount,
        format = CaptureFormat(),
        localAudioPath = file.absolutePath,
        finalizedByteLength = file.length(),
        sha256 = "c".repeat(64),
        recognitionState = recognitionState,
        transcriptVersion = 0,
        archiveState = ArchiveState.VERIFIED,
        transferState = TransferState.PHONE_ARCHIVED,
        archivedAudioUri = "content://synthetic-notes/recording.wav",
        )
    }

    private fun writeWav(pcm: ByteArray, includeAncillaryChunk: Boolean = false): File {
        val format = CaptureFormat()
        val ancillary = if (includeAncillaryChunk) "JUNK".toByteArray(Charsets.US_ASCII) + byteArrayOf(2, 0, 0, 0, 0x41, 0x42) else byteArrayOf()
        val bytes = ByteArrayOutputStream().apply {
            write("RIFF".toByteArray(Charsets.US_ASCII)); writeLe32(36 + ancillary.size + pcm.size)
            write("WAVEfmt ".toByteArray(Charsets.US_ASCII)); writeLe32(16); writeLe16(1)
            writeLe16(format.channels); writeLe32(format.sampleRate); writeLe32(format.sampleRate * format.channels * 2)
            writeLe16(format.channels * 2); writeLe16(format.bitsPerSample)
            write(ancillary)
            write("data".toByteArray(Charsets.US_ASCII)); writeLe32(pcm.size); write(pcm)
        }.toByteArray()
        return File(context.cacheDir, "$captureId.wav").apply { FileOutputStream(this).use { it.write(bytes) } }
    }

    private fun ByteArrayOutputStream.writeLe16(value: Int) = write(ByteBuffer.allocate(2).order(ByteOrder.LITTLE_ENDIAN).putShort(value.toShort()).array())
    private fun ByteArrayOutputStream.writeLe32(value: Int) = write(ByteBuffer.allocate(4).order(ByteOrder.LITTLE_ENDIAN).putInt(value).array())

    private class FakeSpeech : SpeechRecognizerAdapter {
        var supportsExternalAudio = true
        var started = false
        var finished = false
        var events: SpeechRecognizerAdapter.Events? = null
        val received = ByteArrayOutputStream()
        override fun capabilities() = SpeechRecognizerAdapter.Capabilities(
            setOf(CaptureFormat()), setOf("en-US"), true, true, true, supportsExternalAudio, supportsExternalAudio,
        )
        override fun start(sessionId: String, format: CaptureFormat, language: String?, events: SpeechRecognizerAdapter.Events) {
            started = true
            this.events = events
        }
        override fun append(sessionId: String, pcm: ByteArray) { received.write(pcm) }
        override fun finish(sessionId: String) { finished = true }
        override fun cancel(sessionId: String) = Unit
    }

    companion object {
        private const val STAMP = "2026-01-02T03:04:05Z"
    }
}
