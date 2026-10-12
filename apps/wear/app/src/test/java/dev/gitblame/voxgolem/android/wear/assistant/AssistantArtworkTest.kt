package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.recognition.DictationState
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.assertEquals
import org.junit.Test

class AssistantArtworkTest {
    private val idle = AssistantState(foreground = true, selectedHead = "synthetic-old-head")

    @Test fun listeningArtworkRequiresSdkReadinessAndNeverStartsAtAdmissionOrFromARetiredReadyFlag() {
        for (phase in DictationPhase.values()) for (ready in listOf(false, true)) {
            val voice = AssistantVoiceState(dictation = DictationState(phase = phase, readyForSpeech = ready))
            assertEquals(phase == DictationPhase.LISTENING && ready, assistantArtworkListening(voice))
        }
        val artwork = AssistantArtworkTransition()
        artwork.update(idle, true)
        val admitted = AssistantVoiceState(dictation = DictationState(phase = DictationPhase.LISTENING))
        assertFalse(assistantArtworkListening(admitted))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle, true, assistantArtworkListening(admitted)))
        val sdkReady = admitted.copy(dictation = admitted.dictation.copy(readyForSpeech = true))
        assertTrue(assistantArtworkListening(sdkReady))
        assertEquals(AssistantArtworkPhase.LISTENING, artwork.update(idle, true, assistantArtworkListening(sdkReady)))
        assertEquals("Readiness can arrive before the first Compose frame", AssistantArtworkPhase.LISTENING,
            AssistantArtworkTransition().update(idle, true, assistantArtworkListening(sdkReady)))
        assertEquals("Initial in-flight generation still does not replay old feedback", AssistantArtworkPhase.IDLE,
            AssistantArtworkTransition().update(idle.copy(busy = true), true, assistantArtworkListening(sdkReady)))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle.copy(foreground = false), true, assistantArtworkListening(sdkReady)))
    }

    @Test fun actualListeningUsesTheNotePhaseAndBusyRetainsGenerationPriority() {
        val artwork = AssistantArtworkTransition()
        artwork.update(idle, true)
        assertEquals(AssistantArtworkPhase.LISTENING, artwork.update(idle, true, listening = true))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle, true, listening = false))
        assertEquals(AssistantArtworkPhase.GENERATING, artwork.update(idle.copy(busy = true), true, listening = true))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle, false, listening = true))
    }

    @Test fun newListeningCanAnimateAfterAnEarlierRequestFailureButItsEndNeverReturnsAsSuccess() {
        val artwork = AssistantArtworkTransition()
        val failed = idle.copy(error = "transport", interrupted = true)
        artwork.update(failed, true)
        assertEquals(AssistantArtworkPhase.LISTENING, artwork.update(failed, true, listening = true))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(failed, true, listening = false))
    }

    @Test fun currentFollowupListeningSupersedesDurableReturnFeedback() {
        val artwork = AssistantArtworkTransition()
        artwork.update(idle, true)
        artwork.update(idle.copy(busy = true), true)
        val completed = idle.copy(selectedHead = "synthetic-new-head")
        assertEquals(AssistantArtworkPhase.LISTENING, artwork.update(completed, true, listening = true))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(completed, true, listening = false))
    }

    @Test fun onlyObservedGenerationWithANewDurableHeadAnimatesTheReturn() {
        val artwork = AssistantArtworkTransition()
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle, true))
        assertEquals(AssistantArtworkPhase.GENERATING, artwork.update(idle.copy(busy = true), true))
        assertEquals(AssistantArtworkPhase.GENERATING, artwork.update(idle.copy(busy = true, provisional = "Synthetic stream"), true))
        assertEquals(AssistantArtworkPhase.RETURNING, artwork.update(idle.copy(selectedHead = "synthetic-new-head"), true))
        artwork.returned()
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle.copy(selectedHead = "synthetic-new-head"), true))
    }

    @Test fun failureStopBackgroundAndPageExitNeverLookLikeAnAnswer() {
        for (terminal in listOf(idle.copy(error = "transport"), idle.copy(interrupted = true),
            idle.copy(foreground = false), idle)) {
            val artwork = AssistantArtworkTransition()
            artwork.update(idle, true)
            artwork.update(idle.copy(busy = true), true)
            assertEquals(AssistantArtworkPhase.IDLE, artwork.update(terminal, true))
        }
        val artwork = AssistantArtworkTransition()
        artwork.update(idle, true)
        artwork.update(idle.copy(busy = true), true)
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle.copy(busy = true), false))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle.copy(busy = true), true))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle.copy(selectedHead = "synthetic-late-head"), true))
    }

    @Test fun recreationAndForegroundingDoNotRestartAnOldAnimationButANewRequestCan() {
        val artwork = AssistantArtworkTransition()
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle.copy(busy = true), true))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle, true))
        assertEquals(AssistantArtworkPhase.GENERATING, artwork.update(idle.copy(busy = true), true))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle.copy(busy = true, foreground = false), true))
        assertEquals(AssistantArtworkPhase.IDLE, artwork.update(idle.copy(busy = true), true))
    }
}
