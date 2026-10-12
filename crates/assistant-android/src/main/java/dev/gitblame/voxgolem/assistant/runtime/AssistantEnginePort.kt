package dev.gitblame.voxgolem.assistant.runtime

import dev.gitblame.voxgolem.assistant.speech.EnginePort
import dev.gitblame.voxgolem.assistant.speech.GenerationResult
import dev.gitblame.voxgolem.assistant.speech.VoiceInputContext
import org.json.JSONObject
import java.util.UUID
import java.util.concurrent.atomic.AtomicReference
import dev.gitblame.voxgolem.assistant.recognition.RecordedAudioResult

/** Reply ownership comes from the committed turn, never a subsequently reconciled selection. */
fun committedVoiceInputContext(record: JSONObject): VoiceInputContext {
    val operation = record.getJSONObject("operation")
    val body = operation.getJSONObject("body")
    require(body.getString("kind") == "turn_completed")
    return VoiceInputContext(body.getJSONObject("conversation").toString(), operation.getJSONObject("id").toString())
}

/** Speech consumes only a durable terminal result; streaming text is never a playback source. */
class AssistantEnginePort(private val engine: AssistantEngine) : EnginePort {
    private val token = AtomicReference<String?>(null)
    private var inputConversation: String? = null
    private var inputHead: String? = null
    private var inputOwned = false
    private var inputAudio: RecordedAudioResult? = null
    override fun beginInput() { val state = engine.snapshot(); inputConversation = state.conversation; inputHead = state.selectedHead; inputOwned = true }
    override fun setDraft(text: String) {
        if (inputOwned) engine.setSpeechDraft(text, inputConversation, expectedHead = inputHead, requireHeadOwner = true) else engine.setDraft(text)
    }
    override fun setRecordedAudio(audio: RecordedAudioResult?) { inputAudio = audio }
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
                    GenerationResult.Complete(result.speechText ?: answer.getString(if (answer.getString("kind") == "refusal") "reason" else "text"),
                        committedVoiceInputContext(record), audio)
                } else GenerationResult.Failed(result.code ?: "generation_failed")
            } catch (_: RuntimeException) { GenerationResult.Failed("invalid_saved_record") }
            onTerminal(final)
        }
        if (inputOwned) engine.submitSpeech(request, text, inputConversation, listener, expectedHead = inputHead)
        else engine.submit(request, text, resultListener = listener)
    }
    override fun stop() { inputOwned = false; inputAudio = null; token.getAndSet(null)?.let(engine::cancel) }
}
