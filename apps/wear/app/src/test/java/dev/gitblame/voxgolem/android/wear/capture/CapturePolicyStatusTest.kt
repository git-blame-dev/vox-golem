package dev.gitblame.voxgolem.android.wear.capture

import dev.gitblame.voxgolem.assistant.runtime.AssistantPolicy
import dev.gitblame.voxgolem.assistant.runtime.NativeAssistantPolicyStorage
import dev.gitblame.voxgolem.capture.CapturePurpose
import dev.gitblame.voxgolem.capture.CaptureRecord
import dev.gitblame.voxgolem.capture.CaptureState
import dev.gitblame.voxgolem.capture.LiveTranscriptionState
import dev.gitblame.voxgolem.capture.OrgState
import dev.gitblame.voxgolem.capture.TransferState
import dev.gitblame.voxgolem.capture.CaptureRepository as SharedRepository
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.Locale

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class CapturePolicyStatusTest {
    private val context = RuntimeEnvironment.getApplication()
    private val shared = SharedRepository(context)
    private val watch = CaptureRepository(context)
    private val id = "123e4567-e89b-42d3-a456-426614174090"
    private val session = "123e4567-e89b-42d3-a456-426614174091"
    private fun active() = shared.save(CaptureRecord(id, createdAtUtc = "2026-01-02T03:04:05Z",
        captureState = CaptureState.RECORDING, recognitionSessionId = session))

    @Test fun newNoteCapturesChatLanguageAndPrivacyOnceWithoutChangingLegacyNotes() {
        val legacy = active()
        val preferences = NativeAssistantPolicyStorage(context)
        preferences.write(AssistantPolicy(speechLanguage = "en-AU", speechNetworkFallback = false))
        val local = watch.create(16_000, session)
        preferences.write(AssistantPolicy(speechLanguage = "de-DE", speechNetworkFallback = true))
        val allowed = watch.create(16_000, session)
        assertEquals(CapturePurpose.NOTE, local.purpose)
        assertEquals("en-AU", local.requestedLanguage)
        assertFalse(local.speechNetworkFallback)
        assertEquals("de-DE", allowed.requestedLanguage)
        assertTrue(allowed.speechNetworkFallback)
        assertEquals(local, watch.get(local.captureId))
        assertEquals(legacy, watch.get(id))
        assertFalse(legacy.speechNetworkFallback)
        assertNull(legacy.requestedLanguage)
    }

    @Test fun automaticLanguageUsesWatchLocaleAndNotAPhoneLocaleAtRecognitionTime() {
        val previous = Locale.getDefault()
        try {
            Locale.setDefault(Locale.forLanguageTag("en-AU"))
            NativeAssistantPolicyStorage(context).write(AssistantPolicy(speechLanguage = ""))
            assertEquals("en-AU", watch.create(16_000, session).requestedLanguage)
        } finally { Locale.setDefault(previous) }
    }

    @Test fun statusRevisionsAndSessionIdentityFenceLateEventsAndLifecycleSavesPreserveStatus() {
        val original = active()
        assertTrue(watch.updateLiveStatus(id, session, 1, LiveTranscriptionState.PENDING, false, "phone-capture-not-enabled"))
        assertTrue(watch.updateLiveStatus(id, session, 4, LiveTranscriptionState.TRANSCRIBING, true))
        assertFalse(watch.updateLiveStatus(id, session, 2, LiveTranscriptionState.UNAVAILABLE, false))
        assertFalse(watch.updateLiveStatus(id, "123e4567-e89b-42d3-a456-426614174092", 5, LiveTranscriptionState.UNAVAILABLE, false))
        assertFalse(watch.updateLiveStatus(id, session, 5, LiveTranscriptionState.PROVIDER_READY, false))
        watch.save(original.copy(observedFrames = 10))
        val current = watch.get(id)!!
        assertEquals(LiveTranscriptionState.TRANSCRIBING, current.liveTranscriptionState)
        assertEquals(4L, current.liveStatusRevision)
        assertTrue(current.canTranscribeLive)
        assertNull(current.liveErrorCode)
        shared.update(id) { it.copy(captureState = CaptureState.SAVED, sha256 = "a".repeat(64), transferState = TransferState.PHONE_STAGED) }
        assertTrue(watch.acceptPhoneCompletion(id, "a".repeat(64), "Synthetic final note", 1))
        assertFalse(watch.updateLiveStatus(id, session, 6, LiveTranscriptionState.UNAVAILABLE, false))
        assertEquals(LiveTranscriptionState.COMPLETED, watch.get(id)?.liveTranscriptionState)
    }

    @Test fun localTransportFailureRetainsLatePreviewWithoutClaimingTheAbortedStreamIsLive() {
        active()
        watch.reportLiveTransportFailure(id, session, "live-queue-overflow")
        assertFalse(watch.updateLiveStatus(id, session, 1, LiveTranscriptionState.TRANSCRIBING, true))
        watch.updateTranscript(id, session, 1, "Synthetic retained preview", false)
        val current = watch.get(id)!!
        assertEquals("Synthetic retained preview", current.previewText)
        assertEquals(LiveTranscriptionState.UNAVAILABLE, current.liveTranscriptionState)
        assertFalse(current.canTranscribeLive)
        assertEquals("live-queue-overflow", current.liveErrorCode)
        assertTrue(watch.updateLiveStatus(id, session, 2, LiveTranscriptionState.PENDING, false, "phone-capture-not-enabled"))
        assertEquals(LiveTranscriptionState.UNAVAILABLE, watch.get(id)?.liveTranscriptionState)
        assertEquals("phone-capture-not-enabled", watch.get(id)?.liveErrorCode)
        assertFalse(watch.updateLiveStatus(id, session, 3, LiveTranscriptionState.PROVIDER_READY, false))
        watch.reportLiveTransportFailure(id, "123e4567-e89b-42d3-a456-426614174092", "live-stream-failed")
        assertEquals("phone-capture-not-enabled", watch.get(id)?.liveErrorCode)
    }

    @Test fun chatClipsCannotBecomeNoteActiveHistoryTransfersOrRecoveryWork() {
        val note = active()
        val chatId = "123e4567-e89b-42d3-a456-426614174092"
        val chat = shared.save(CaptureRecord(chatId, createdAtUtc = "2026-01-03T03:04:05Z", purpose = CapturePurpose.ASK,
            orgState = OrgState.NONE, captureState = CaptureState.RECORDING))
        assertEquals(note, watch.active())
        assertFalse(watch.records().any { it.captureId == chatId })
        watch.recoverForAdmission()
        assertEquals(chat, shared.get(chatId))
        shared.update(chatId) { it.copy(captureState = CaptureState.SAVED, localAudioPath = "/synthetic/audio.wav") }
        assertFalse(watch.pendingTransfers().any { it.captureId == chatId })
    }
}
