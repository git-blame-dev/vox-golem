package dev.gitblame.voxgolem.assistant.phone

import android.content.Context
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import dev.gitblame.voxgolem.assistant.runtime.AssistantEngine
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.runtime.committedVoiceInputContext
import dev.gitblame.voxgolem.assistant.runtime.ProviderAuthReason
import dev.gitblame.voxgolem.assistant.replication.AssistantReplication
import dev.gitblame.voxgolem.assistant.replication.PeerPairingState
import org.json.JSONObject
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

/** Owns only the active transport sink. SQLite/session/provider ownership stays in AssistantRuntime. */
class EnginePhoneBackend internal constructor(private val context: Context, private val engine: AssistantEngine) : AssistantPhoneBackend {
    constructor(context: Context) : this(context, AssistantRuntime.get(context))
    private val delivery = AtomicReference<PhoneRequestDelivery?>(null)
    private val preferences = PhonePreferences(context)
    private val preferencesPending = AtomicBoolean(false)
    private val admission = Any()
    private var initializingConversation = false
    private val startupWaiters = mutableListOf<(Result<Any?>) -> Unit>()
    private var deferEmptyShortcutConversation = false
    private val requestTokens = LinkedHashMap<String, String>()
    @Volatile var speechStatus: JSONObject? = null
    @Volatile var onPreferencesChanged: ((Boolean) -> Unit)? = null
    fun nativePreferences(): JSONObject = preferencesFor(engine.snapshot())
    private fun preferencesFor(state: AssistantState) = preferences.read(state.model)
        .put("web_enabled", state.webEnabled).put("speech_network_fallback", state.speechNetworkFallback)
        .put("speech_language", state.speechLanguage).put("speech_tts_provider", state.speechTtsProvider).put("speech_tts_voice", state.speechTtsVoice)
    fun publishSpeechOptions(options: dev.gitblame.voxgolem.assistant.speech.SpeechOptions) { engine.publishSpeechOptions(options) }
    fun busy(): Boolean = engine.snapshot().busy || delivery.get() != null || preferencesPending.get()
    fun saveSpeechDraft(text: String) { engine.setDraft(text) }
    fun speechConversation(): String? = engine.snapshot().conversation
    fun speechHead(): String? = engine.snapshot().selectedHead
    fun speechAdmissionSnapshot(): SpeechAdmissionSnapshot = engine.snapshot().let {
        SpeechAdmissionSnapshot(SpeechInputOwner(it.conversation, it.selectedHead), it.draft)
    }
    fun observeSpeechContext(observer: () -> Unit): AutoCloseable = engine.subscribe { observer() }
    fun saveOwnedSpeechDraft(text: String, conversation: String?) { engine.setSpeechDraft(text, conversation) }
    fun saveOwnedSpeechDraft(text: String, conversation: String?, head: String?) {
        engine.setSpeechDraft(text, conversation, expectedHead = head, requireHeadOwner = true)
    }
    fun stopAll() { delivery.get()?.cancel(); engine.stop() }

    override fun command(name: String, arguments: JSONObject, reply: (Result<Any?>) -> Unit) {
        if (name == "cancel_prompt") {
            val requestId = arguments.optString("requestId")
            if (requestId.isEmpty() || requestId.toByteArray(Charsets.UTF_8).size > 128) {
                reply(Result.failure(AssistantPhoneFailure("invalid_input", "Request ID is empty or too long"))); return
            }
            val accepted = engine.snapshot().let { it.busy && it.clientRequestToken == requestId }
            cancelDelivery(requestId)
            reply(Result.success(accepted))
            return
        }
        fun settled(state: AssistantState) {
            try {
                requireSettled(state)
                AssistantReplication.reconcileNow(context)
                publishSnapshot(reply)
            } catch (error: RuntimeException) { reply(Result.failure(error)) }
        }
        try {
            when (name) {
                "get_app_snapshot" -> engine.selectedContext { raw, state ->
                    if (state.conversation == null && NewVoiceShortcutLauncher.actions.hasAction()) deferEmptyShortcutConversation = true
                    if (state.conversation == null && !state.busy && !deferEmptyShortcutConversation) {
                        ensureConversation(state, reply)
                    } else replySnapshot(reply, state, raw)
                }
                "new_conversation" -> {
                    val draft = arguments.optString("draft", "")
                    if (draft.isNotEmpty()) engine.resetWithDraft(draft, ::settled)
                    else engine.reset(::settled)
                }
                "select_conversation" -> engine.selectConversation(PhoneIdentity.native(arguments.getString("conversationId")), ::settled)
                "select_conversation_head" -> engine.selectHead(PhoneIdentity.native(arguments.getString("headId")), ::settled)
                "save_assistant_draft" -> engine.setDraft(arguments.getString("text")) { state -> reply(runCatching { requireSettled(state); null }) }
                "set_preferences" -> updatePreferences(arguments.getJSONObject("preferences"), reply)
                "set_search_mode" -> engine.selectSearchMode(arguments.getString("mode")) { state ->
                    reply(runCatching { requireSettled(state); JSONObject().put("mode", state.searchMode) })
                }
                "list_assistant_conversations" -> engine.refresh { state -> reply(runCatching {
                    val view = JSONObject(state.viewJson)
                    if (view.getString("revision") != arguments.getString("expectedRevision")) throw AssistantPhoneFailure("busy", "Conversation inventory changed; refresh and retry")
                    val offset = arguments.getInt("offset")
                    require(offset in 0..16384)
                    val records = view.getJSONArray("conversations")
                    val visible = (0 until records.length()).map { records.getJSONObject(it) }.filter { !it.getBoolean("deleted") && !it.isNull("title") }
                    val page = org.json.JSONArray(visible.drop(offset).take(128).map { JSONObject().put("id", PhoneSnapshotEncoder.wireId(it.getJSONObject("id"))).put("title", it.getString("title")) })
                    JSONObject().put("revision", view.getString("revision")).put("conversations", page).put("next_offset", (offset + page.length()).coerceAtMost(visible.size))
                }) }
                "list_assistant_heads" -> engine.refresh { state -> reply(runCatching {
                    val view = JSONObject(state.viewJson)
                    if (view.getString("revision") != arguments.getString("expectedRevision") || state.conversation?.let { PhoneSnapshotEncoder.wireId(JSONObject(it)) } != arguments.getString("conversationId")) throw AssistantPhoneFailure("busy", "Continuation inventory changed; refresh and retry")
                    val offset = arguments.getInt("offset")
                    require(offset in 0..16384)
                    val page = state.branchChoices.drop(offset).take(128).map { PhoneSnapshotEncoder.wireId(JSONObject(it)) }
                    JSONObject().put("revision", view.getString("revision")).put("heads", org.json.JSONArray(page)).put("next_offset", (offset + page.size).coerceAtMost(state.branchChoices.size))
                }) }
                "assistant_pairing_status" -> AssistantReplication.pairingState(context) { state -> reply(Result.success(pairingValue(state))) }
                else -> reply(Result.failure(AssistantPhoneFailure("invalid_input", "Unknown Assistant command")))
            }
        } catch (error: RuntimeException) { reply(Result.failure(error)) }
    }

    private fun pairingValue(state: PeerPairingState): JSONObject {
        return JSONObject().put("reason", state.reason.name.lowercase())
            .put("associationId", state.associationId ?: JSONObject.NULL).put("installationId", state.peerInstallationId ?: JSONObject.NULL)
            .put("replica", state.peerReplica ?: JSONObject.NULL).put("credentialsAllowed", state.credentialsAllowed)
    }

    private fun ensureConversation(before: AssistantState, reply: (Result<Any?>) -> Unit) {
        synchronized(admission) {
            if (startupWaiters.size >= 16) { reply(Result.failure(providerFailure("busy"))); return }
            startupWaiters += reply
            if (initializingConversation) return
            initializingConversation = true
        }
        fun finish(state: AssistantState) {
            if (state.error == null) AssistantReplication.reconcileNow(context)
            val waiters = synchronized(admission) { initializingConversation = false; startupWaiters.toList().also { startupWaiters.clear() } }
            for (waiting in waiters) {
                if (state.error != null) waiting(Result.failure(providerFailure(state.error)))
                else publishSnapshot(waiting)
            }
        }
        engine.newConversation { created ->
            if (created.error == null && before.draft.isNotEmpty()) engine.setDraft(before.draft, ::finish)
            else finish(created)
        }
    }

    private fun publishSnapshot(reply: (Result<Any?>) -> Unit) {
        engine.selectedContext { raw, state -> replySnapshot(reply, state, raw) }
    }

    private fun replySnapshot(reply: (Result<Any?>) -> Unit, state: AssistantState, raw: String?) {
        reply(runCatching {
            val unresolved = state.branchChoices.isNotEmpty() && state.selectedHead == null
            if (raw == null && state.conversation != null && !unresolved) {
                throw AssistantPhoneFailure("internal", "Canonical conversation context is unavailable")
            }
            encode(state, raw?.let(::JSONObject))
        })
    }

    private fun encode(state: AssistantState, selectedContext: JSONObject?): JSONObject {
        val credential = state.credential
        val ready = credential?.reason == ProviderAuthReason.READY || credential?.reason == ProviderAuthReason.FORBIDDEN
        val reason = when (credential?.reason) {
            ProviderAuthReason.READY -> "Ready"
            ProviderAuthReason.EXPIRED -> providerFailure("auth_expired").boundedMessage
            ProviderAuthReason.UNAUTHORIZED -> providerFailure("unauthorized").boundedMessage
            ProviderAuthReason.FORBIDDEN -> providerFailure("forbidden").boundedMessage
            else -> providerFailure("auth_unavailable").boundedMessage
        }
        val routing = mutableMapOf<String, String>()
        selectedContext?.getJSONArray("turns")?.let { turns ->
            for (index in 0 until turns.length()) {
                val id = turns.getJSONObject(index).getJSONObject("operation").getJSONObject("body").getJSONObject("request")
                val key = PhoneSnapshotEncoder.wireId(id)
                val token = synchronized(admission) { requestTokens[key] }
                    ?: engine.clientTokenForDurableRequest(id.getString("replica"), id.getString("sequence"))
                token?.let { routing[key] = it }
            }
        }
        val encoded = PhoneSnapshotEncoder.encode(state, selectedContext, preferencesFor(state), routing,
            providerAvailable = ready, providerReason = reason)
        val speech = speechStatus
        if (speech?.optBoolean("ttsAvailable") == true) {
            val capabilities = encoded.getJSONArray("capabilities")
            for (index in 0 until capabilities.length()) if (capabilities.getJSONObject(index).getString("id") == "tts") {
                val network = speech.optBoolean("ttsNetworkRequired", false)
                capabilities.getJSONObject(index).put("state", "available")
                    .put("reason", if (network) "Native network voice fallback" else "Installed local Android voice")
                    .put("actual_provider", if (network) "android_network_tts" else "android_local_tts")
            }
        }
        return encoded
    }

    private fun requireSettled(state: AssistantState) {
        state.error?.let { throw providerFailure(it) }
    }

    private fun updatePreferences(input: JSONObject, reply: (Result<Any?>) -> Unit) {
        val value = PhonePreferences.normalize(input)
        PhonePreferences.validate(value)
        if (value.getBoolean("tts_enabled") && !nativePreferences().getBoolean("tts_enabled") &&
            (!AssistantRuntime.captureResourceAdmissionReady() || speechStatus?.optBoolean("ttsAvailable") != true)) {
            throw AssistantPhoneFailure("unavailable", "An installed native TTS voice or native resource admission is unavailable")
        }
        synchronized(admission) {
            if (!preferencesPending.compareAndSet(false, true)) { reply(Result.failure(providerFailure("busy"))); return }
        }
        val selectedModel = value.getString("instant")
        AssistantRuntime.prewarm(selectedModel)
        engine.updatePreferences(selectedModel, dev.gitblame.voxgolem.assistant.runtime.AssistantPolicy(
            value.getBoolean("web_enabled"), value.getBoolean("speech_network_fallback"), value.getString("speech_language"),
            value.getString("speech_tts_provider"), value.getString("speech_tts_voice")), { preferences.write(value) }) { selected ->
            preferencesPending.set(false)
            if (selected.error != null) reply(Result.failure(providerFailure(selected.error)))
            else {
                // A view listener failure must not misclassify already persisted settings.
                runCatching { onPreferencesChanged?.invoke(value.getBoolean("tts_enabled")) }
                reply(Result.success(preferencesFor(selected)))
            }
        }
    }

    override fun submit(clientRequestId: String, prompt: String, update: (JSONObject) -> Unit, terminal: (Result<Any?>) -> Unit) {
        submit(clientRequestId, prompt, update, terminal, null)
    }

    override fun submit(clientRequestId: String, prompt: String, update: (JSONObject) -> Unit, terminal: (Result<Any?>) -> Unit, searchMode: String?) {
        submitForOwner(clientRequestId, prompt, update, terminal, searchMode, null)
    }
    fun submitOwnedSpeech(clientRequestId: String, prompt: String, update: (JSONObject) -> Unit, terminal: (Result<Any?>) -> Unit,
        searchMode: String?, owner: SpeechInputOwner) {
        submitForOwner(clientRequestId, prompt, update, terminal, searchMode, owner)
    }
    private fun submitForOwner(clientRequestId: String, prompt: String, update: (JSONObject) -> Unit, terminal: (Result<Any?>) -> Unit,
        searchMode: String?, owner: SpeechInputOwner?) {
        val current = PhoneRequestDelivery(clientRequestId, update, { engine.rejectDelivery(clientRequestId) })
        val admitted = synchronized(admission) { !preferencesPending.get() && !initializingConversation && delivery.compareAndSet(null, current) }
        if (!admitted) {
            terminal(Result.failure(AssistantPhoneFailure("busy", "A request is already active"))); return
        }
        val listener = dev.gitblame.voxgolem.assistant.runtime.AssistantResultListener { result, _ ->
            delivery.compareAndSet(current, null)
            try {
                when (result.status) {
                    "complete" -> {
                        val record = JSONObject(result.recordJson ?: error("Missing saved record"))
                        val requestId = result.providerRequestId ?: error("Missing durable request ID")
                        synchronized(admission) {
                            while (requestTokens.size >= 256) requestTokens.remove(requestTokens.keys.first())
                            requestTokens[requestId] = result.clientRequestToken
                        }
                        val turn = PhoneSnapshotEncoder.turn(record, mapOf(requestId to result.clientRequestToken))
                        val refused = turn.getJSONObject("answer").getJSONObject("content").getString("kind") == "refusal"
                        val outcome = JSONObject().put("kind", if (refused) "refused" else "completed").put("turn", turn)
                        if (result.firstTextMs != null && result.completedMs != null) {
                            outcome.put("first_text_ms", result.firstTextMs)
                            outcome.put("completed_ms", result.completedMs)
                        }
                        result.speechText?.let { outcome.put("speechText", it) }
                        if (owner != null) committedVoiceInputContext(record).let { committed ->
                            outcome.put("voiceContext", JSONObject().put("conversation", committed.conversation)
                                .put("selectedHead", committed.selectedHead))
                        }
                        val reconciliationFailed = runCatching { AssistantReplication.reconcileNow(context) }.isFailure
                        if (result.code == "saved_local_state_failed" || reconciliationFailed) {
                            outcome.put("warning", providerFailure("saved_local_state_failed").boundedMessage)
                        }
                        terminal(Result.success(outcome))
                    }
                    "cancelled" -> terminal(Result.success(JSONObject().put("kind", "cancelled")))
                    else -> {
                        terminal(Result.failure(providerFailure(result.code)))
                    }
                }
            } catch (error: RuntimeException) { terminal(Result.failure(error)) }
        }
        if (owner == null) engine.submit(clientRequestId, prompt, current.gate, listener, searchMode)
        else engine.submitSpeech(clientRequestId, prompt, owner.conversation, listener,
            expectedHead = owner.selectedHead, delivery = current.gate, searchMode = searchMode)
    }

    override fun acknowledge(clientRequestId: String, providerRequestId: String, revision: String, accepted: Boolean): Boolean =
        delivery.get()?.takeIf { it.clientRequestId == clientRequestId }?.acknowledge(providerRequestId, revision, accepted) ?: false

    override fun cancelDelivery(clientRequestId: String) {
        delivery.get()?.takeIf { it.clientRequestId == clientRequestId }?.cancel()
        engine.cancel(clientRequestId)
    }

    override fun foreground(visible: Boolean) {
        if (visible) { engine.resume(); AssistantReplication.reconcileNow(context) } else {
            delivery.get()?.cancel()
            engine.background()
        }
    }

    companion object {
        fun providerFailure(code: String?): AssistantPhoneFailure = when (code) {
            "busy" -> AssistantPhoneFailure("busy", "A request is already active")
            "invalid_request", "empty_prompt", "prompt_too_large", "unsupported_search_mode" -> AssistantPhoneFailure("invalid_input", "Prompt, request ID or search mode is invalid")
            "duplicate_request" -> AssistantPhoneFailure("invalid_input", "This request identity has already been used")
            "auth_expired" -> AssistantPhoneFailure("unavailable", "Provider access has expired. Update the app-private provider file externally, then retry explicitly.")
            "auth_unavailable" -> AssistantPhoneFailure("unavailable", "Provider access is missing or invalid. Supply the app-private provider file externally, then retry explicitly.")
            "unauthorized" -> AssistantPhoneFailure("unavailable", "Provider rejected access. Replace the app-private provider file externally, then retry explicitly.")
            "forbidden" -> AssistantPhoneFailure("unavailable", "Provider denied access to this model or account")
            "rate_limited" -> AssistantPhoneFailure("unavailable", "Provider rate limit reached")
            "timeout" -> AssistantPhoneFailure("unavailable", "Provider request timed out")
            "too_large" -> AssistantPhoneFailure("unavailable", "Provider response was too large")
            "not_saved" -> AssistantPhoneFailure("internal", "Response was not saved. The draft is retained; retry explicitly.")
            "draft_not_saved", "storage_error" -> AssistantPhoneFailure("internal", "Draft storage failed. Keep the visible text; do not assume it survives restart.")
            "saved_local_state_failed" -> AssistantPhoneFailure("internal", "Response was saved, but local conversation state could not be reconciled. Stop and select a continuation before sending.")
            "delivery_failed" -> AssistantPhoneFailure("unavailable", "Response delivery failed before save. The draft is retained.")
            "choose_continuation" -> AssistantPhoneFailure("unavailable", "Choose a continuation before sending")
             "unsupported_model" -> AssistantPhoneFailure("unavailable", "Selected model is unavailable on Android")
             "unsupported_speech_language" -> AssistantPhoneFailure("unavailable", "Selected speech language is not supported by the available recognizer; use Automatic or a reported supported language")
             "unsupported_tts_provider" -> AssistantPhoneFailure("unavailable", "Selected TTS provider is not installed or available")
             "unsupported_tts_voice" -> AssistantPhoneFailure("unavailable", "Selected TTS voice is not available for this provider, language or network policy")
             "invalid_speech_selection" -> AssistantPhoneFailure("invalid_input", "Invalid speech selection")
            "background", "backgrounded" -> AssistantPhoneFailure("unavailable", "The app must be in the foreground to send a request")
            else -> AssistantPhoneFailure("unavailable", "Assistant request failed. The draft is retained; retry explicitly.")
        }
    }
}
