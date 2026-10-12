package dev.gitblame.voxgolem.android.wear.assistant

import android.os.Handler
import android.os.SystemClock
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.speech.LoopPhase
import dev.gitblame.voxgolem.assistant.speech.SpeechPhase

/** Main-thread window policy. Uptime is also the Handler's scheduling clock. */
internal class AssistantScreenAwake(private val handler: Handler, private val keepScreenOn: (Boolean) -> Unit) {
    internal data class Token(val foreground: Long = 0, val window: Long = 0)
    @Volatile var token = Token()
        private set
    private var foreground = false
    private var deadline = 0L
    private var beganAt = 0L
    private var chat = AssistantState()
    private var voice = AssistantVoiceState()
    private val expire = Runnable {
        if (foreground && deadline != 0L && SystemClock.uptimeMillis() >= deadline) releaseWindow()
    }

    fun resume(chat: AssistantState, voice: AssistantVoiceState) {
        token = Token(token.foreground + 1, token.window + 1)
        foreground = true
        beganAt = SystemClock.uptimeMillis()
        this.chat = chat
        this.voice = voice
        renew(beganAt)
    }

    fun suspend() {
        token = Token(token.foreground + 1, token.window + 1)
        foreground = false
        releaseWindow()
    }

    fun interaction() {
        if (foreground) renew(SystemClock.uptimeMillis())
    }

    fun chatProgress(value: AssistantState, observed: Token, occurredAt: Long) {
        if (!foreground || observed.foreground != token.foreground || occurredAt < beganAt) return
        // Retired-window observations still advance equality baselines, never the awake deadline.
        val previous = chat
        chat = value
        val meaningful = value.draft != previous.draft || value.busy != previous.busy ||
            (value.busy && (value.clientRequestToken != previous.clientRequestToken ||
                value.provisional != previous.provisional || value.toolStatus != previous.toolStatus))
        if (meaningful && observed == token) progress(occurredAt)
    }

    fun voiceProgress(value: AssistantVoiceState, observed: Token, occurredAt: Long) {
        if (!foreground || observed.foreground != token.foreground || occurredAt < beganAt) return
        val previous = voice
        voice = value
        val dictationProgress = (value.dictation.phase != previous.dictation.phase ||
            value.dictation.draft != previous.dictation.draft || value.dictation.utterance != previous.dictation.utterance) &&
            (value.dictation.phase != DictationPhase.IDLE || previous.dictation.phase != DictationPhase.IDLE)
        val speechProgress = value.speech.phase != previous.speech.phase &&
            (value.speech.phase == SpeechPhase.SPEAKING || previous.speech.phase == SpeechPhase.SPEAKING)
        val activeLoop = setOf(LoopPhase.LISTENING, LoopPhase.GENERATING, LoopPhase.SPEAKING, LoopPhase.BETWEEN_TURNS)
        val loopProgress = (value.loop.phase != previous.loop.phase || value.loop.draft != previous.loop.draft) &&
            (value.loop.phase in activeLoop || previous.loop.phase in activeLoop)
        if ((dictationProgress || speechProgress || loopProgress) && observed == token) progress(occurredAt)
    }

    private fun progress(occurredAt: Long) {
        // Expiry invalidates queued progress; only a newly observed change can open a fresh window.
        val now = SystemClock.uptimeMillis()
        if (deadline != 0L && now >= deadline) { releaseWindow(); return }
        if (occurredAt > now || occurredAt + IDLE_MILLIS <= now) return
        renew(occurredAt)
    }

    private fun renew(at: Long) {
        deadline = maxOf(deadline, at + IDLE_MILLIS)
        handler.removeCallbacks(expire)
        keepScreenOn(true)
        handler.postAtTime(expire, deadline)
    }

    private fun releaseWindow() {
        token = token.copy(window = token.window + 1)
        deadline = 0L
        handler.removeCallbacks(expire)
        keepScreenOn(false)
    }

    companion object { const val IDLE_MILLIS = 120_000L }
}
