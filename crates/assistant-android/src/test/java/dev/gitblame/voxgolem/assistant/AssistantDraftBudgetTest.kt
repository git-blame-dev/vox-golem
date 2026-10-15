package dev.gitblame.voxgolem.assistant

import org.junit.Assert.*
import org.junit.Test

class AssistantDraftBudgetTest {
    @Test fun validatesBytesWithoutTrimmingOrTruncatingAndPermitsEmptyDrafts() {
        assertTrue(AssistantDraftBudget.accepts(""))
        for (text in listOf("x".repeat(AssistantDraftBudget.MAX_BYTES),
            "é".repeat(AssistantDraftBudget.MAX_BYTES / 2), "🙂".repeat(AssistantDraftBudget.MAX_BYTES / 4))) {
            assertTrue(AssistantDraftBudget.accepts(text))
            assertFalse(AssistantDraftBudget.accepts(text + "x"))
        }
        assertFalse(AssistantDraftBudget.accepts(" ".repeat(AssistantDraftBudget.MAX_BYTES + 1)))
    }
}
