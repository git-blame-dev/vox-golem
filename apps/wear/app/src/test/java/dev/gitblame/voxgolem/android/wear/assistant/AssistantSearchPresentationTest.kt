package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.recognition.DictationState
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import org.junit.Assert.*
import org.junit.Test

class AssistantSearchPresentationTest {
    @Test fun compactModeControlCyclesAllThreeModesAndProjectsNativeSelection() {
        assertEquals("Quick", assistantSearchModeLabel(AssistantState()))
        assertEquals("Verify", assistantSearchModeLabel(AssistantState(searchMode = "verify")))
        assertEquals("Research", assistantSearchModeLabel(AssistantState(searchMode = "research")))
        assertEquals("verify", assistantNextSearchMode("quick"))
        assertEquals("research", assistantNextSearchMode("verify"))
        assertEquals("quick", assistantNextSearchMode("research"))
    }

    @Test fun modeSelectionRequiresForegroundIdleSupportedModelAndNoActiveVoice() {
        val ready = AssistantState(foreground = true)
        assertTrue(assistantCanSelectSearchMode(ready, AssistantVoiceState()))
        for (model in listOf("custom-sol-high", "custom-sol-none", "custom-luna-none", "custom-luna-low")) {
            assertTrue(assistantCanSelectSearchMode(ready.copy(model = model), AssistantVoiceState()))
        }
        for (state in listOf(ready.copy(foreground = false), ready.copy(busy = true), ready.copy(model = "synthetic-unsupported"))) {
            assertFalse(assistantCanSelectSearchMode(state, AssistantVoiceState()))
        }
        assertFalse(assistantCanSelectSearchMode(ready, AssistantVoiceState(dictation = DictationState(phase = DictationPhase.LISTENING))))
    }

    @Test fun toolActivityIsSeparateFromAnswerTextAndNeverShownAfterBusyRetires() {
        for (status in listOf("Searching", "Reading", "Fallback", "Partial")) {
            val state = AssistantState(busy = true, toolStatus = status, provisional = "Synthetic answer")
            assertEquals(status, assistantToolActivity(state))
            assertNull(assistantToolActivity(state.copy(busy = false)))
        }
        assertNull(assistantToolActivity(AssistantState(busy = true)))
    }
}
