package dev.gitblame.voxgolem.assistant.phone

import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class VoiceEntryTimingTest {
    @Test fun acceptsOnlyTheExplicitFrontendVocabulary() {
        val stages = listOf(
            Stage.ENTRY_FRONTEND_READY, Stage.ENTRY_FRONTEND_CLAIMED,
            Stage.ENTRY_FRONTEND_INVALIDATED, Stage.ENTRY_FRONTEND_BUSY,
            Stage.ENTRY_FRONTEND_UNAVAILABLE, Stage.ENTRY_DRAFT_REQUESTED,
            Stage.ENTRY_DRAFT_ADMITTED, Stage.ENTRY_DRAFT_REJECTED,
            Stage.ENTRY_CHANNEL_UNAVAILABLE, Stage.ENTRY_DISPATCHED,
            Stage.ENTRY_RESOLVED, Stage.ENTRY_NATIVE_REJECTED, Stage.ENTRY_EVENT_INVALID,
        )
        stages.forEach { stage ->
            val wire = stage.name.lowercase(java.util.Locale.ROOT)
            assertEquals(stage, voiceEntryFrontendStage(wire))
            assertNull(voiceEntryFrontendStage(stage.name))
            assertNull(voiceEntryFrontendStage(" $wire"))
            assertNull(voiceEntryFrontendStage("$wire\n"))
        }
        Stage.values().filterNot(stages::contains).forEach {
            assertNull(voiceEntryFrontendStage(it.name.lowercase(java.util.Locale.ROOT)))
        }
        listOf("", "frontend_ready", "first_response_dom", "entry_unknown",
            "entry_resolved synthetic-payload", "synthetic-session-id").forEach {
            assertNull(voiceEntryFrontendStage(it))
        }
    }
}
