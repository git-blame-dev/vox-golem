package dev.gitblame.voxgolem.android.wear.assistant

import android.app.Activity
import android.os.Bundle
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows
import org.robolectric.annotation.Config

/** Simulated delayed native settlement; actual Activity recreation/ViewModel retention is exercised. */
class DelayedEditorAssistantActivity : SyntheticAssistantActivity() {
    override fun saveEditorDraft(text: String, onSettled: (AssistantState) -> Unit) { acknowledgements += text to onSettled }
    companion object { val acknowledgements = mutableListOf<Pair<String, (AssistantState) -> Unit>>() }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AssistantEditorDraftRetentionTest {
    @Test fun rotationKeepsAcknowledgementOwnerAndReopeningAfterSendUsesCurrentNativeDraft() = withClient { client ->
        val controller = Robolectric.buildActivity(DelayedEditorAssistantActivity::class.java).setup()
        try {
            val original = controller.get()
            confirm(original, "Synthetic pending editor text")
            assertEquals("Synthetic old draft", client.state.draft)
            controller.recreate()
            assertNotSame(original, controller.get())
            client.publish(client.state.copy(draft = "Synthetic pending editor text"))
            DelayedEditorAssistantActivity.acknowledgements.single().second(client.state)
            client.send()
            client.publish(client.state.copy(draft = "Synthetic native draft after send"))
            assertEquals("Synthetic native draft after send", prefill(controller.get()))
            assertEquals(1, client.sends)
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun oldActivityAcknowledgementCannotClearANewerRecoveryTicketAfterRotation() = withClient { client ->
        val controller = Robolectric.buildActivity(DelayedEditorAssistantActivity::class.java).setup()
        try {
            confirm(controller.get(), "Synthetic first pending text")
            controller.recreate()
            confirm(controller.get(), "Synthetic newer pending text")
            assertEquals(2, DelayedEditorAssistantActivity.acknowledgements.size)
            client.publish(client.state.copy(draft = "Synthetic first pending text"))
            DelayedEditorAssistantActivity.acknowledgements[0].second(client.state)
            assertEquals("Synthetic newer pending text", prefill(controller.get()))
            client.publish(client.state.copy(draft = "Synthetic newer pending text"))
            DelayedEditorAssistantActivity.acknowledgements[1].second(client.state)
            client.publish(client.state.copy(draft = "Synthetic later native draft"))
            assertEquals("Synthetic later native draft", prefill(controller.get()))
        } finally { controller.pause().stop().destroy() }
    }

    @Test fun freshBundleRestorationDoesNotResubmitOrPrefillAmbiguousTextOverANewerNativeDraft() = withClient { client ->
        val original = Robolectric.buildActivity(DelayedEditorAssistantActivity::class.java).setup()
        val saved = Bundle()
        try {
            confirm(original.get(), "Synthetic ambiguous recovery text")
            original.saveInstanceState(saved)
        } finally { original.pause().stop().destroy() }
        DelayedEditorAssistantActivity.acknowledgements.clear() // A new process has no old callbacks.
        client.publish(client.state.copy(draft = "Synthetic newer durable native text"))
        val restored = Robolectric.buildActivity(DelayedEditorAssistantActivity::class.java).create(saved).start().resume().visible()
        try {
            assertTrue("Restoration must never issue setDraft", DelayedEditorAssistantActivity.acknowledgements.isEmpty())
            assertEquals("Synthetic newer durable native text", prefill(restored.get()))
            assertEquals("Synthetic newer durable native text", client.state.draft)
            assertTrue(DelayedEditorAssistantActivity.acknowledgements.isEmpty())
        } finally { restored.pause().stop().destroy() }
    }

    @Test fun restoredCopiesAreContextFencedAndAlreadyDurableTextDoesNotReappearAfterSend() {
        val state = AssistantState(draft = "Synthetic baseline", conversation = "synthetic-conversation", selectedHead = "synthetic-head")
        val owner = AssistantEditorDraftMemory()
        owner.restore(null, state)
        val ticket = owner.begin("Synthetic saved text", state)
        val saved = Bundle().also(owner::save)
        val recovery = AssistantEditorDraftMemory()
        recovery.restore(saved, state)
        assertEquals(ticket.text, recovery.prefill(state))
        assertNull(recovery.prefill(state.copy(conversation = "synthetic-other-conversation")))
        assertNull(recovery.prefill(state.copy(selectedHead = "synthetic-other-head")))
        assertNull(recovery.prefill(state.copy(draft = "Synthetic newer native text")))
        assertNull("Observed native progress must not later resurrect ambiguous process recovery", recovery.prefill(state))
        val held = Bundle().also(recovery::save)
        val restoredHeld = AssistantEditorDraftMemory()
        restoredHeld.restore(held, state)
        assertNull(restoredHeld.prefill(state))
        val durable = AssistantEditorDraftMemory()
        durable.restore(saved, state.copy(draft = ticket.text))
        assertNull(durable.prefill(state.copy(draft = "")))
        // Retained memory ignores the stale Bundle when acknowledgement won before reconstruction.
        owner.settle(ticket, state.copy(draft = ticket.text))
        owner.restore(saved, state)
        assertNull(owner.prefill(state))
        owner.save(saved)
        val afterDeath = AssistantEditorDraftMemory()
        afterDeath.restore(saved, state)
        assertNull(afterDeath.prefill(state))
    }

    private fun confirm(activity: DelayedEditorAssistantActivity, text: String) {
        activity.editDraft()
        assertNotNull(Shadows.shadowOf(activity).nextStartedActivityForResult)
        activity.receiveText(Activity.RESULT_OK, AssistantTextInput.result(text))
    }

    private fun prefill(activity: DelayedEditorAssistantActivity): String? {
        activity.editDraft()
        val value = Shadows.shadowOf(activity).nextStartedActivityForResult.intent.getStringExtra(AssistantTextInput.DRAFT)
        activity.receiveText(Activity.RESULT_CANCELED, null)
        return value
    }

    private fun withClient(test: (SyntheticAssistantClient) -> Unit) {
        val previous = SyntheticAssistantActivity.retainedClient
        val client = SyntheticAssistantClient()
        SyntheticAssistantActivity.retainedClient = client
        DelayedEditorAssistantActivity.acknowledgements.clear()
        try { test(client) }
        finally { SyntheticAssistantActivity.retainedClient = previous; DelayedEditorAssistantActivity.acknowledgements.clear() }
    }
}
