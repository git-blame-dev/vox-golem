package dev.gitblame.voxgolem.capture

import android.Manifest
import android.net.Uri
import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows
import org.robolectric.annotation.Config
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CountDownLatch
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.TimeUnit

/** Correction contracts are separate from transport/replay scheduling scenarios. */
@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class PhoneCaptureCorrectionTest {
    private val context = RuntimeEnvironment.getApplication()
    private val id = "123e4567-e89b-42d3-a456-426614174091"
    private val sessionId = "123e4567-e89b-42d3-a456-426614174092"
    private val stamp = "2026-01-01T00:00:00Z"
    private lateinit var repository: CaptureRepository
    private lateinit var coordinator: PhoneCaptureCoordinator
    private val leases = RecognitionResourceLease()
    private val speech = CorrectionSpeech()
    private val exported = CopyOnWriteArrayList<CaptureRecord>()
    private var runtimeCheck: () -> Boolean = { true }

    @Before fun setUp() {
        context.deleteDatabase("captures.db")
        repository = CaptureRepository(context)
        Shadows.shadowOf(context).grantPermissions(Manifest.permission.RECORD_AUDIO)
        coordinator = PhoneCaptureCoordinator(context, speech, { _, _, _ -> },
            { Uri.parse("content://synthetic-notes/corrected.wav") },
            { capture ->
                exported += repository.get(capture)!!
                OrgExportResult(OrgState.EXPORTED, "synthetic-destination")
            }, replayPacingMillis = 0, runtimeReady = { runtimeCheck() }, recognitionLeases = leases)
    }
    @After fun tearDown() { speech.failCleanup = false; coordinator.shutdownForTests() }

    @Test fun correctionRetiresLiveOwnerAndLatePartialFinalFailureCannotReplaceIt() {
        val inputOwner: () -> Unit = {}
        assertTrue(coordinator.registerLiveInput(id, sessionId, "synthetic-node", inputOwner))
        assertTrue(coordinator.beginLive(id, sessionId, "synthetic-node", CaptureFormat(), stamp, inputOwner))
        coordinator.appendLive(id, sessionId, "synthetic-node", inputOwner, byteArrayOf(1, 0, 2, 0))
        coordinator.finishLive(id, sessionId, "synthetic-node", inputOwner, 2)
        coordinator.onArchiveReceived(replayRecord(), "synthetic-node")
        assertTrue(coordinator.awaitIdle())
        assertEquals(CaptureState.SAVED, repository.get(id)!!.captureState)
        val old = speech.events!!
        val corrected = coordinator.correctTranscript(id, "Synthetic manual authority", expectedVersion = 0)
        old.onHypothesis("Synthetic stale partial", false)
        old.onHypothesis("Synthetic stale final", true)
        old.onFailure("speech-recognizer-7")
        assertTrue(coordinator.awaitIdle())
        assertEquals(corrected.finalText, repository.get(id)!!.finalText)
        assertEquals(1L, repository.get(id)!!.transcriptVersion)
        assertNull(repository.get(id)!!.recognitionSessionId)
        assertFalse(repository.get(id)!!.canTranscribeLive)
        assertEquals(LiveTranscriptionState.COMPLETED, repository.get(id)!!.liveTranscriptionState)
        assertFalse(old.isCurrent())
        assertEquals(listOf(sessionId), speech.cancelled)
    }

    @Test fun replayCorrectionFencesAnAlreadyQueuedFinalAndExportsOnlyTheCorrection() {
        saveReplay()
        coordinator.retryCapture(id)
        assertTrue(coordinator.awaitIdle())
        val old = speech.events!!
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        coordinator.beforeHypothesisPersistence = { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)) }
        try {
            old.onHypothesis("Synthetic queued ASR final", true)
            assertTrue(entered.await(2, TimeUnit.SECONDS))
            coordinator.correctTranscript(id, "Synthetic queued correction", expectedVersion = 0)
        } finally { release.countDown(); coordinator.beforeHypothesisPersistence = {} }
        assertTrue(coordinator.awaitIdle())
        val current = repository.get(id)!!
        assertEquals("Synthetic queued correction", current.finalText)
        assertEquals(1L, current.transcriptVersion)
        assertNull(current.recognitionSessionId)
        assertTrue("An actual export must occur", exported.isNotEmpty())
        assertTrue(exported.all { it.finalText == current.finalText && it.transcriptVersion == current.transcriptVersion })
    }

    @Test fun failedCorrectionCleanupRetainsExactLeaseUntilExplicitRecovery() {
        saveReplay()
        coordinator.retryCapture(id)
        assertTrue(coordinator.awaitIdle())
        val lease = leases.current()!!
        val owner = speech.events!!
        speech.failCleanup = true
        coordinator.correctTranscript(id, "Synthetic corrected while cleanup failed", expectedVersion = 0)
        assertTrue(coordinator.awaitIdle())
        assertSame(lease, leases.current())
        assertNull(leases.tryAcquire(RecognitionResourceLease.Kind.CHAT))
        owner.onHypothesis("Synthetic revoked callback", false)
        assertTrue(coordinator.awaitIdle())
        assertEquals("Synthetic corrected while cleanup failed", repository.get(id)!!.finalText)
        speech.failCleanup = false
        assertTrue(coordinator.retryFailedRetirements())
        assertNull(leases.current())
    }

    @Test fun blankCorrectionPreservesExportedHistoryAndNeverStartsRecognitionAgain() {
        saveReplay()
        repository.update(id) { it.copy(orgState = OrgState.EXPORTED, exportTranscriptVersion = 4,
            recognitionState = RecognitionState.FINAL, finalText = "Synthetic prior export", transcriptVersion = 4) }
        coordinator.correctTranscript(id, "", expectedVersion = 4)
        assertTrue(coordinator.awaitIdle())
        val corrected = repository.get(id)!!
        assertEquals(RecognitionState.NO_SPEECH, corrected.recognitionState)
        assertEquals(OrgState.EXPORTED, corrected.orgState)
        assertEquals(4L, corrected.exportTranscriptVersion)
        assertEquals(5L, corrected.transcriptVersion)
        assertTrue(exported.isEmpty())
        assertNull(speech.events)
    }

    @Test fun versionConflictDoesNotRetireTheCurrentRecognizerOrExportAndAsrCanStillFinish() {
        saveReplay()
        coordinator.retryCapture(id)
        assertTrue(coordinator.awaitIdle())
        val running = repository.get(id)!!
        val lease = leases.current()!!
        val events = speech.events!!
        assertThrows(CaptureTranscriptConflictException::class.java) {
            coordinator.correctTranscript(id, "Synthetic conflicting edit", expectedVersion = 1)
        }
        assertTrue(coordinator.awaitIdle())
        assertEquals(running, repository.get(id))
        assertSame(lease, leases.current())
        assertTrue(events.isCurrent())
        assertTrue(speech.cancelled.isEmpty())
        assertTrue(exported.isEmpty())
        events.onHypothesis("Synthetic authoritative ASR final", true)
        assertTrue(coordinator.awaitIdle())
        val final = repository.get(id)!!
        assertEquals(1L, final.transcriptVersion)
        assertEquals("Synthetic authoritative ASR final", final.finalText)
        val exportCount = exported.size
        assertTrue(exportCount > 0)
        assertThrows(CaptureTranscriptConflictException::class.java) {
            coordinator.correctTranscript(id, "Synthetic stale edit after ASR", expectedVersion = 0)
        }
        assertTrue(coordinator.awaitIdle())
        assertEquals(final, repository.get(id))
        assertEquals(exportCount, exported.size)
    }

    @Test fun failedCapabilitiesFromAnOldReplaySelectionCannotRevokeAManualCorrection() = earlyReplayFailure("capabilities")
    @Test fun unavailableRuntimeFromAnOldReplaySelectionCannotRevokeAManualCorrection() = earlyReplayFailure("runtime")
    @Test fun missingWavFromAnOldReplaySelectionCannotRevokeAManualCorrection() = earlyReplayFailure("missing-wav")

    private fun earlyReplayFailure(boundary: String) {
        saveReplay()
        val originalPath = repository.get(id)!!.localAudioPath
        val entered = CountDownLatch(1)
        val release = CountDownLatch(1)
        val block = { entered.countDown(); check(release.await(5, TimeUnit.SECONDS)) }
        when (boundary) {
            "capabilities" -> { speech.externalAudio = false; speech.beforeCapabilities = block }
            "runtime" -> runtimeCheck = { block(); false }
            "missing-wav" -> {
                val missing = File(context.cacheDir, "synthetic-missing-${java.util.UUID.randomUUID()}.wav")
                assertFalse(missing.exists())
                repository.update(id) { it.copy(localAudioPath = missing.absolutePath) }
                coordinator.beforeReplayValidation = block
            }
        }
        try {
            coordinator.retryCapture(id)
            assertTrue("Selected replay must enter $boundary before the edit", entered.await(2, TimeUnit.SECONDS))
            coordinator.correctTranscript(id, "Synthetic correction during $boundary", expectedVersion = 0)
        } finally { release.countDown() }
        assertTrue(coordinator.awaitIdle())
        val corrected = repository.get(id)!!
        assertEquals(RecognitionState.FINAL, corrected.recognitionState)
        assertEquals(1L, corrected.transcriptVersion)
        assertEquals("Synthetic correction during $boundary", corrected.finalText)
        assertNull(corrected.recognitionSessionId)
        assertNull(corrected.lastErrorCode)
        assertNull(speech.events)
        speech.externalAudio = true; speech.beforeCapabilities = {}; runtimeCheck = { true }
        coordinator.beforeReplayValidation = {}
        repository.update(id) { it.copy(localAudioPath = originalPath) }
        coordinator.retryCapture(id)
        assertTrue(coordinator.awaitIdle())
        assertNull("A later retry must not admit ASR over the correction", speech.events)
        assertEquals(1L, repository.get(id)!!.transcriptVersion)
        assertEquals(corrected.finalText, repository.get(id)!!.finalText)
        assertTrue(exported.all { it.finalText == corrected.finalText })
    }

    private fun saveReplay() {
        assertNotNull(repository.admitPhoneCapture(replayRecord(), "synthetic-node"))
    }

    private fun replayRecord(): CaptureRecord {
        // Independent canonical RIFF fixture; no production writer or parser establishes its bytes.
        val bytes = ByteBuffer.allocate(48).order(ByteOrder.LITTLE_ENDIAN)
            .put("RIFF".toByteArray()).putInt(40).put("WAVEfmt ".toByteArray()).putInt(16)
            .putShort(1).putShort(1).putInt(16_000).putInt(32_000).putShort(2).putShort(16)
            .put("data".toByteArray()).putInt(4).putShort(1).putShort(2).array()
        val wav = File(context.cacheDir, "synthetic-correction.wav").apply { writeBytes(bytes) }
        return CaptureRecord(id, createdAtUtc = stamp, captureState = CaptureState.SAVED,
            localAudioPath = wav.absolutePath, durableFrames = 2, finalizedByteLength = 48,
            sha256 = "a".repeat(64), recognitionState = RecognitionState.QUEUED,
            archiveState = ArchiveState.VERIFIED, transferState = TransferState.PHONE_ARCHIVED,
            archivedAudioUri = "content://synthetic-notes/corrected.wav")
    }

    private class CorrectionSpeech : SpeechRecognizerAdapter {
        var events: SpeechRecognizerAdapter.Events? = null
        var failCleanup = false
        var externalAudio = true
        var beforeCapabilities: () -> Unit = {}
        val cancelled = CopyOnWriteArrayList<String>()
        override fun capabilities(): SpeechRecognizerAdapter.Capabilities {
            beforeCapabilities()
            return SpeechRecognizerAdapter.Capabilities(setOf(CaptureFormat()), emptySet(), true, true, true, externalAudio, externalAudio)
        }
        override fun start(sessionId: String, format: CaptureFormat, language: String?, events: SpeechRecognizerAdapter.Events) { this.events = events }
        override fun append(sessionId: String, pcm: ByteArray) = Unit
        override fun finish(sessionId: String) = Unit
        override fun cancel(sessionId: String) { if (failCleanup) error("Synthetic retirement failed"); cancelled += sessionId }
    }
}
