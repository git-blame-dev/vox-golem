package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import org.junit.Assert.*
import org.junit.Test

class AssistantSearchPresentationTest {
    @Test fun toolActivityIsSeparateFromAnswerTextAndNeverShownAfterBusyRetires() {
        for (status in listOf("Searching", "Reading", "Fallback", "Partial")) {
            val state = AssistantState(busy = true, toolStatus = status, provisional = "Synthetic answer")
            assertEquals(status, assistantToolActivity(state))
            assertNull(assistantToolActivity(state.copy(busy = false)))
        }
        assertNull(assistantToolActivity(AssistantState(busy = true)))
    }
}
