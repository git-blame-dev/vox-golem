package dev.gitblame.voxgolem.assistant.inputtransport

import dev.gitblame.voxgolem.assistant.recognition.RecognitionResourceLease
import java.util.concurrent.CompletableFuture

/** Call all methods/callbacks on the same main dispatcher. Call tick at least every 250 ms. */
class PhoneInputController(
    private val factory: SuppliedInputFactory,
    private val foreground: CallerForegroundSignal,
    private val peers: SourcePeerBinding,
    private val lease: RecognitionResourceLease = RecognitionResourceLease.process,
    private val reply: (String, InputFrame) -> Unit,
    private val now: () -> Long = { System.nanoTime() / 1000000 },
    private val onOutcome: (InputOutcome) -> Unit = {},
) {
    private class Active(val node: String, val start: InputFrame, val token: RecognitionResourceLease.Token,
        val startedAt: Long) {
        var sdk: SuppliedInputSession? = null
        var inputSequence = 0L
        var outputSequence = 0L
        var lastAudio = startedAt
        var stoppingAt: Long? = null
        var stopRequestedAt: Long? = null
        var ready = false
        var draft = ""
        val result = CompletableFuture<InputOutcome>()
    }
    private var active: Active? = null
    // A finite replay fence for this process; START duplicates cannot resurrect stopped audio.
    private val recent = ArrayDeque<String>()

    fun receive(node: String, frame: InputFrame): Boolean {
        if (runCatching { AssistantInputWire.validate(frame) }.isFailure) return false
        if (!peers.accepts(node, frame.source, frame.target)) return false
        if (frame.kind == InputKind.START) return begin(node, frame)
        val a = active ?: return if (frame.kind == InputKind.CANCEL) {
            remember(frame.session); true
        } else false
        if (!matches(a, node, frame) || frame.sequence <= a.inputSequence) return false
        if (frame.kind !in setOf(InputKind.STOP, InputKind.CANCEL)) return false
        a.inputSequence = frame.sequence
        if (frame.kind == InputKind.CANCEL) terminal(a, null, "cancelled")
        else if (a.stopRequestedAt == null) a.stopRequestedAt = now()
        return true
    }
    private fun begin(node: String, frame: InputFrame): Boolean {
        if (frame.sequence != 0L || active != null || frame.session in recent) return false
        if (!foreground.isForeground() || !factory.available(frame.language)) return false
        val token = lease.tryAcquire(RecognitionResourceLease.Kind.CHAT) ?: return false
        val a = Active(node, frame, token, now()); active = a
        remember(frame.session)
        try {
            a.sdk = factory.open(frame.language)
            a.sdk!!.checkSupport { supported ->
                if (active !== a) return@checkSupport
                if (!supported || !eligible(a)) { terminal(a, null, "supplied_audio_unavailable"); return@checkSupport }
                try {
                    a.sdk!!.start(object : SuppliedInputEvents {
                        override fun partial(text: String) {
                            if (active !== a) return
                            if (!eligible(a)) { terminal(a, null, "foreground_or_peer_lost"); return }
                            if (!bounded(text)) { terminal(a, null, "transcript_limit"); return }
                            a.draft = text; emit(a, InputKind.PARTIAL)
                        }
                        override fun final(text: String) {
                            if (active !== a) return
                            if (!eligible(a)) { terminal(a, null, "foreground_or_peer_lost"); return }
                            if (a.stoppingAt == null) terminal(a, null, "provider_ended_before_stop")
                            else if (text.isBlank()) terminal(a, null, "recognition_empty_final")
                            else if (!bounded(text)) terminal(a, null, "transcript_limit")
                            else terminal(a, text, null)
                        }
                        override fun error(code: String) { if (active === a) terminal(a, null, safeCode(code)) }
                    })
                    if (active === a) { a.ready = true; emit(a, InputKind.READY) }
                } catch (_: Exception) { if (active === a) terminal(a, null, "sdk_start_failed") }
            }
        } catch (_: Exception) { terminal(a, null, "sdk_open_failed") }
        return true
    }
    fun acceptsAudio(node: String, session: String): Boolean = active?.let {
        it.node == node && it.start.session == session && it.ready && it.stoppingAt == null && eligible(it)
    } ?: false
    fun audio(node: String, session: String, pcm: ByteArray): Boolean {
        val a = active ?: return false
        if (!acceptsAudio(node, session)) return false
        val accepted = runCatching { a.sdk!!.append(pcm) }.getOrDefault(false)
        if (accepted) a.lastAudio = now() else terminal(a, null, "audio_backpressure")
        return accepted
    }
    /** Source Channel EOF, not a Message STOP, is the ordered end of PCM. */
    fun sourceEnded(node: String, session: String) {
        val a = active ?: return
        if (a.node == node && a.start.session == session && eligible(a)) stop(session)
    }
    fun stop(session: String): CompletableFuture<InputOutcome> {
        val a = active?.takeIf { it.start.session == session }
            ?: return CompletableFuture.completedFuture(InputOutcome(session, "", false, "session_unavailable"))
        if (a.stoppingAt == null) {
            a.stoppingAt = now()
            try { a.sdk?.finish() } catch (_: Exception) { terminal(a, null, "sdk_finish_failed") }
        }
        return a.result
    }
    fun tick() {
        val a = active ?: return
        val time = now()
        val error = when {
            !eligible(a) -> "foreground_or_peer_lost"
            a.stoppingAt?.let { time - it >= AssistantInputWire.FINAL_MILLIS } == true -> "final_timeout"
            a.stopRequestedAt?.let { a.stoppingAt == null && time - it >= AssistantInputWire.FINAL_MILLIS } == true -> "source_end_timeout"
            time - a.startedAt >= AssistantInputWire.MAX_SESSION_MILLIS -> "session_timeout"
            time - a.lastAudio >= AssistantInputWire.IDLE_MILLIS && a.stoppingAt == null -> "audio_timeout"
            else -> null
        }
        if (error != null) terminal(a, null, error)
    }
    fun interrupt() { active?.let { terminal(it, null, "interrupted") } }
    fun transportFailure(node: String, session: String) {
        active?.takeIf { it.node == node && it.start.session == session }?.let { terminal(it, null, "transport_failed") }
    }
    private fun eligible(a: Active) = foreground.isForeground() && peers.accepts(a.node, a.start.source, a.start.target)
    private fun remember(session: String) {
        if (session !in recent) recent.addLast(session)
        if (recent.size > 64) recent.removeFirst()
    }
    private fun matches(a: Active, node: String, f: InputFrame) = a.node == node && a.start.session == f.session &&
        a.start.source == f.source && a.start.target == f.target && a.start.language == f.language
    private fun bounded(text: String) = text.toByteArray(Charsets.UTF_8).size <= AssistantInputWire.MAX_TEXT_BYTES
    private fun safeCode(code: String) = code.takeIf { it.length <= 96 && it.matches(Regex("[a-z0-9_-]+")) } ?: "sdk_error"
    private fun emit(a: Active, kind: InputKind, error: String = "") {
        val frame = a.start.copy(kind = kind, source = a.start.target, target = a.start.source,
            sequence = ++a.outputSequence, text = if (kind == InputKind.READY) "" else a.draft, error = error)
        try { reply(a.node, frame) } catch (_: Exception) { if (active === a) terminal(a, null, "transport_failed") }
    }
    private fun terminal(a: Active, final: String?, error: String?) {
        if (active !== a) return
        active = null // Fence synchronous/late SDK events before teardown.
        var failure = error
        val retired = runCatching { a.sdk?.close() }.isSuccess
        if (retired) a.token.close() else {
            failure = "sdk_retirement_failed"
            a.token.onRetirementRetry {
                runCatching { a.sdk?.close(); a.token.close(); true }.getOrDefault(false)
            }
        }
        if (final != null) a.draft = final
        val authoritative = final != null && failure == null
        emit(a, if (authoritative) InputKind.FINAL else InputKind.ERROR, failure ?: "")
        val outcome = InputOutcome(a.start.session, a.draft, authoritative, failure)
        a.result.complete(outcome); runCatching { onOutcome(outcome) }
    }
}
