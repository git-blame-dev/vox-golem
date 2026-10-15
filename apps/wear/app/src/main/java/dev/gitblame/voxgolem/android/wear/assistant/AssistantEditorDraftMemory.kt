package dev.gitblame.voxgolem.android.wear.assistant

import android.os.Bundle
import androidx.lifecycle.ViewModel
import dev.gitblame.voxgolem.assistant.runtime.AssistantState

/** The save ticket, not an Activity instance, owns its native acknowledgement across rotation. */
internal class AssistantEditorDraftMemory : ViewModel() {
    class Ticket(val text: String, val baseline: String, val conversation: String?, val head: String?, val restored: Boolean = false)
    private var initialized = false
    private var pending: Ticket? = null
    private var recoverySuppressed = false

    @Synchronized fun restore(saved: Bundle?, current: AssistantState) {
        if (initialized) return // A retained owner outranks the older onSaveInstanceState copy.
        initialized = true
        val text = saved?.getString(TEXT) ?: return
        val baseline = saved.getString(BASELINE) ?: return
        val ticket = Ticket(text, baseline, saved.getString(CONVERSATION), saved.getString(HEAD), restored = true)
        // Process restoration is not permission to re-submit. Already durable text needs no recovery.
        if (!sameContext(ticket, current) || current.draft != text) {
            pending = ticket
            recoverySuppressed = saved.getBoolean(SUPPRESSED) || !sameContext(ticket, current) || current.draft != baseline
        }
    }

    @Synchronized fun begin(text: String, current: AssistantState): Ticket =
        Ticket(text, current.draft, current.conversation, current.selectedHead).also {
            initialized = true; pending = it; recoverySuppressed = false
        }

    @Synchronized fun settle(ticket: Ticket, settled: AssistantState) {
        if (pending === ticket && sameContext(ticket, settled) && settled.draft == ticket.text) pending = null
    }

    @Synchronized fun prefill(current: AssistantState): String? {
        val ticket = pending ?: return null
        if (ticket.restored && (!sameContext(ticket, current) || current.draft != ticket.baseline)) recoverySuppressed = true
        // Keep ambiguous recovery bytes, but never later resurrect them after observing native progress.
        return ticket.text.takeIf { sameContext(ticket, current) && (!ticket.restored || !recoverySuppressed) }
    }

    @Synchronized fun save(out: Bundle) {
        listOf(TEXT, BASELINE, CONVERSATION, HEAD, SUPPRESSED).forEach(out::remove)
        pending?.let {
            out.putString(TEXT, it.text); out.putString(BASELINE, it.baseline)
            out.putString(CONVERSATION, it.conversation); out.putString(HEAD, it.head)
            out.putBoolean(SUPPRESSED, recoverySuppressed)
        }
    }

    private fun sameContext(ticket: Ticket, state: AssistantState) =
        ticket.conversation == state.conversation && ticket.head == state.selectedHead

    private companion object {
        const val TEXT = "assistant.editor.unacknowledged"
        const val BASELINE = "assistant.editor.baseline"
        const val CONVERSATION = "assistant.editor.conversation"
        const val HEAD = "assistant.editor.head"
        const val SUPPRESSED = "assistant.editor.recovery-suppressed"
    }
}
