package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.assistant.runtime.AssistantModelPresentation
import org.junit.Assert.assertEquals
import org.junit.Test

class AssistantModelHeaderTest {
    private fun model(effort: String, effortLabel: String) =
        AssistantModelPresentation("id", "Full", "family", "Family", "6.1 Sol", effort, effortLabel)

    @Test fun fullEffortLabelIsAppendedAfterAMiddleDot() {
        val expected = mapOf(
            "low" to "Low",
            "medium" to "Medium",
            "high" to "High",
            "xhigh" to "X-High",
            "max" to "Max",
        )
        for ((effort, label) in expected) {
            assertEquals("6.1 Sol \u00B7 $label", headerLabel(model(effort, label)))
        }
    }

    @Test fun noEffortVariantOrModelLeavesOnlyTheFamilyLabel() {
        assertEquals("6.1 Sol", headerLabel(model("none", "No Reasoning")))
        assertEquals("Model", headerLabel(null))
    }
}
