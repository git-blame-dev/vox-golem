package dev.gitblame.voxgolem.android.wear.capture

import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.RecognitionState
import dev.gitblame.voxgolem.capture.TransferState
import dev.gitblame.voxgolem.capture.CaptureFormat
import dev.gitblame.voxgolem.capture.PcmRecordingWriter
import dev.gitblame.voxgolem.capture.CaptureRepository as SharedRepository
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CaptureFeedbackTest {
    private val context = RuntimeEnvironment.getApplication()
    private val shared = SharedRepository(context)
    private val watch = CaptureRepository(context)

    private fun savedAudio(): CaptureRecord {
        val id = UUID.randomUUID().toString()
        val pcm = shared.createPcmSpool(id)
        val wav = watch.audio(id)
        val manifest = PcmRecordingWriter(pcm, CaptureFormat()).use { writer ->
            writer.append(byteArrayOf(1, 2, 3, 4))
            writer.finish(wav)
        }
        return shared.save(CaptureRecord(
            captureId = id, createdAtUtc = "2026-10-04T12:30:00Z", captureState = CaptureState.SAVED,
            localAudioPath = wav.absolutePath, finalizedByteLength = manifest.wavByteLength, sha256 = manifest.sha256,
            transferState = TransferState.PHONE_STAGED,
        ))
    }

    @Test fun emptyLiveHypothesesKeepTheLastPersistedDraftAcrossRepositoryRecreation() {
        val record = savedAudio()
        val sessionId = UUID.randomUUID().toString()
        shared.update(record.captureId) { it.copy(recognitionSessionId = sessionId) }
        watch.updateTranscript(record.captureId, sessionId, 1, "synthetic visible draft", false)

        // The transport deliberately treats even live FINAL messages as previews.
        CaptureRepository(context).updateTranscript(record.captureId, sessionId, 2, "", false)
        CaptureRepository(context).updateTranscript(record.captureId, sessionId, 3, "  ", false)

        val restored = CaptureRepository(context).get(record.captureId)!!
        assertEquals("synthetic visible draft", restored.previewText)
        assertEquals(3L, restored.lastRevision)
        assertNull(restored.finalText)
        assertEquals(RecognitionState.RUNNING, restored.recognitionState)
        assertFalse(watch.claimCompletionFeedback(record.captureId))
        assertTrue(watch.pendingTransfers().any { it.captureId == record.captureId })
    }

    @Test fun emptyFinalKeepsDraftButCannotClaimCompletionOrBlockLaterNonemptyText() {
        val record = savedAudio()
        val sessionId = UUID.randomUUID().toString()
        shared.update(record.captureId) { it.copy(recognitionSessionId = sessionId) }
        watch.updateTranscript(record.captureId, sessionId, 1, "synthetic visible draft", false)
        watch.updateTranscript(record.captureId, sessionId, 2, "", true)

        val restored = CaptureRepository(context).get(record.captureId)!!
        assertEquals("synthetic visible draft", restored.previewText)
        assertEquals(RecognitionState.NO_SPEECH, restored.recognitionState)
        assertNull(restored.finalText)
        assertFalse(watch.claimCompletionFeedback(record.captureId))

        watch.updateTranscript(record.captureId, sessionId, 3, "synthetic revised draft", false)
        assertEquals("synthetic revised draft", watch.get(record.captureId)!!.previewText)
        assertNull(watch.get(record.captureId)!!.finalText)
    }

    @Test fun lifecycleSaveWithAnEmptyEqualRevisionPreviewCannotEraseDraft() {
        val record = savedAudio()
        val sessionId = UUID.randomUUID().toString()
        shared.update(record.captureId) { it.copy(recognitionSessionId = sessionId) }
        watch.updateTranscript(record.captureId, sessionId, 1, "synthetic visible draft", false)
        val snapshot = watch.get(record.captureId)!!

        watch.save(snapshot.copy(previewText = ""))

        assertEquals("synthetic visible draft", CaptureRepository(context).get(record.captureId)!!.previewText)
        assertNull(watch.get(record.captureId)!!.finalText)
        assertFalse(watch.claimCompletionFeedback(record.captureId))
    }

    @Test fun localAudioAndLivePreviewCannotClaimSuccess() {
        val record = savedAudio()
        shared.update(record.captureId) { it.copy(previewText = "synthetic provisional text") }
        assertFalse(watch.claimCompletionFeedback(record.captureId))
        assertFalse(watch.acceptPhoneCompletion(record.captureId, "b".repeat(64), "wrong recording", 1))
        assertFalse(watch.acceptPhoneCompletion(record.captureId, record.sha256!!, "", 1))
        assertNull(shared.get(record.captureId)!!.finalText)
    }

    @Test fun hashBoundFinalSurvivesRestartAndClaimsOnlyOneSuccessEvenAfterReceiptReplay() {
        val record = savedAudio()
        assertTrue(watch.reportCompletionProblem(record.captureId, "phone-unavailable"))
        assertFalse(CaptureRepository(context).reportCompletionProblem(record.captureId, "phone-unavailable"))
        assertTrue(watch.acceptPhoneCompletion(record.captureId, record.sha256!!, "synthetic full note", 2))
        assertEquals("synthetic full note", CaptureRepository(context).get(record.captureId)!!.finalText)
        assertNull(shared.get(record.captureId)!!.lastErrorCode)
        assertTrue(CaptureRepository(context).claimCompletionFeedback(record.captureId))
        assertFalse(watch.claimCompletionFeedback(record.captureId))
        assertFalse(watch.acceptPhoneCompletion(record.captureId, record.sha256!!, "stale receipt", 1))
        assertFalse(watch.reportCompletionProblem(record.captureId, "stale-error", RecognitionState.FAILED))
        watch.save(record)
        assertTrue(CaptureRepository(context).get(record.captureId)!!.completionNotified)
        assertEquals("synthetic full note", shared.get(record.captureId)!!.finalText)
    }

    @Test fun archivedAudioStaysPendingUntilFinalCompletionIsAcknowledged() {
        val record = savedAudio()
        watch.updateTransfer(record.captureId, TransferState.PHONE_ARCHIVED)
        assertTrue(watch.pendingTransfers().any { it.captureId == record.captureId })
        watch.acceptPhoneCompletion(record.captureId, record.sha256!!, "synthetic final note", 1)
        assertTrue(watch.claimCompletionFeedback(record.captureId))
        assertFalse(watch.pendingTransfers().any { it.captureId == record.captureId })
    }

    @Test fun missingLocalAudioNeverProducesSuccessFeedback() {
        val record = savedAudio()
        watch.acceptPhoneCompletion(record.captureId, record.sha256!!, "synthetic final note", 1)
        watch.audio(record.captureId).delete()
        assertFalse(watch.claimCompletionFeedback(record.captureId))
    }

    @Test fun assetDeliveredTranscriptAboveMessageLimitCanCompleteWithoutTruncation() {
        val record = savedAudio()
        val text = "synthetic large note ".repeat(2_000)
        assertTrue(text.toByteArray().size > dev.gitblame.voxgolem.capture.CaptureProtocol.MAX_MESSAGE_BYTES)
        assertTrue(watch.acceptPhoneCompletion(record.captureId, record.sha256!!, text, 1))
        assertEquals(text, CaptureRepository(context).get(record.captureId)!!.finalText)
        assertTrue(watch.claimCompletionFeedback(record.captureId))
    }

    @Test fun olderCaptureFailuresRemainVisibleWithoutLateAttentionAfterANewerNote() {
        val old = savedAudio()
        val recent = shared.save(CaptureRecord(
            captureId = UUID.randomUUID().toString(), createdAtUtc = "2026-10-04T12:31:00Z",
            captureState = CaptureState.SAVED, recognitionState = RecognitionState.FINAL,
            finalText = "synthetic newer completed note", completionNotified = true,
        ))
        assertFalse(watch.reportCompletionProblem(old.captureId, "speech-recognizer-timeout", RecognitionState.FAILED))
        assertEquals("speech-recognizer-timeout", watch.get(old.captureId)!!.lastErrorCode)
        assertEquals(RecognitionState.FAILED, watch.get(old.captureId)!!.recognitionState)
        assertNull(watch.get(recent.captureId)!!.lastErrorCode)
        assertTrue(watch.get(recent.captureId)!!.completionNotified)
    }

    @Test fun staleLifecycleSnapshotCannotEraseConcurrentFinalReceiptArchiveOrRevision() {
        val record = savedAudio()
        val session = UUID.randomUUID().toString()
        shared.update(record.captureId) { it.copy(recognitionSessionId = session) }
        val stale = watch.get(record.captureId)!!
        watch.updateTranscript(record.captureId, session, 5, "Synthetic current preview", false)
        watch.updateTransfer(record.captureId, TransferState.PHONE_ARCHIVED)
        shared.update(record.captureId) { it.copy(archivedAudioUri = "content://synthetic.archive/audio") }
        assertTrue(watch.acceptPhoneCompletion(record.captureId, record.sha256!!, "Synthetic receipt final", 2))
        assertTrue(watch.claimCompletionFeedback(record.captureId))
        watch.save(stale.copy(captureState = CaptureState.SAVED, previewText = "Synthetic stale draft", lastRevision = 1))
        val persisted = CaptureRepository(context).get(record.captureId)!!
        assertEquals("Synthetic receipt final", persisted.finalText)
        assertEquals("Synthetic receipt final", persisted.previewText)
        assertEquals(RecognitionState.FINAL, persisted.recognitionState)
        assertEquals(5L, persisted.lastRevision)
        assertEquals(2L, persisted.transcriptVersion)
        assertEquals(session, persisted.recognitionSessionId)
        assertEquals(TransferState.PHONE_ARCHIVED, persisted.transferState)
        assertEquals("content://synthetic.archive/audio", persisted.archivedAudioUri)
        assertTrue(persisted.completionNotified)
        assertFalse(watch.claimCompletionFeedback(record.captureId))
    }
}
