package dev.gitblame.voxgolem.assistant.runtime

import dev.gitblame.voxgolem.assistant.speech.EnginePort
import dev.gitblame.voxgolem.assistant.speech.GenerationResult
import dev.gitblame.voxgolem.assistant.speech.VoiceInputContext
import org.json.JSONObject
import java.util.UUID
import java.util.concurrent.atomic.AtomicReference
import dev.gitblame.voxgolem.assistant.recognition.RecordedAudioResult
import dev.gitblame.voxgolem.assistant.AssistantTiming

/** Reply ownership comes from the committed turn, never a subsequently reconciled selection. */
fun committedVoiceInputContext(record: JSONObject): VoiceInputContext {
    val operation = record.getJSONObject("operation")
    val body = operation.getJSONObject("body")
    require(body.getString("kind") == "turn_completed")
    return VoiceInputContext(body.getJSONObject("conversation").toString(), operation.getJSONObject("id").toString())
}

/** Durable completion and explicitly subscribed provisional speech share one request-token owner. */
class AssistantEnginePort(private val engine: AssistantEngine) : EnginePort {
    private val token = AtomicReference<String?>(null)
    private var inputConversation: String? = null
    private var inputHead: String? = null
    private var inputOwned = false
    private var inputAudio: RecordedAudioResult? = null
    private var inputTiming: AssistantTiming? = null
    override fun beginInput() = beginInput(null)
    override fun beginInput(timingParent: AssistantTiming?) {
        inputTiming = timingParent
        val state = engine.snapshot()
        inputConversation = state.conversation; inputHead = state.selectedHead; inputOwned = true
    }
    override fun prewarm() { engine.prewarm() }
    override fun setDraft(text: String) {
        if (inputOwned) engine.setSpeechDraft(text, inputConversation, expectedHead = inputHead, requireHeadOwner = true) else engine.setDraft(text)
    }
    override fun setRecordedAudio(audio: RecordedAudioResult?) { inputAudio = audio }
    override fun observeStreamText(listener: (String) -> Unit): AutoCloseable =
        engine.observeProvisionalStream { requestToken, provisional ->
            val activeToken = token.get()
            if (activeToken != null && activeToken == requestToken && provisional.isNotEmpty()) {
                listener(provisional)
            }
        }
    override fun sendFinal(text: String, onTerminal: (GenerationResult) -> Unit) {
        val audio = inputAudio
        val request = "voice:${UUID.randomUUID()}"
        if (!token.compareAndSet(null, request)) { onTerminal(GenerationResult.Failed("generation_busy")); return }
        val listener = AssistantResultListener { result, _ ->
            token.compareAndSet(request, null)
            val final = try {
                if (result.code == "saved_local_state_failed") GenerationResult.Failed(result.code)
                else if (result.status == "complete" && result.recordJson != null) {
                    val record = JSONObject(result.recordJson)
                    val answer = record.getJSONObject("operation").getJSONObject("body").getJSONObject("answer")
                    GenerationResult.Complete(answer.getString(if (answer.getString("kind") == "refusal") "reason" else "text"),
                        committedVoiceInputContext(record), audio)
                } else GenerationResult.Failed(result.code ?: "generation_failed")
            } catch (_: RuntimeException) { GenerationResult.Failed("invalid_saved_record") }
            onTerminal(final)
        }
        if (inputOwned) engine.submitSpeech(request, text, inputConversation, listener, expectedHead = inputHead, timingParent = inputTiming)
        else engine.submit(request, text, resultListener = listener)
    }
    override fun stop() { inputOwned = false; inputAudio = null; inputTiming = null; token.getAndSet(null)?.let(engine::cancel) }
}
