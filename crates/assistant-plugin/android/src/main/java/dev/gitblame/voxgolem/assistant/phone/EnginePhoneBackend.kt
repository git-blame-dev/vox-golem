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
class EnginePhoneBackend internal constructor(
    private val context: Context,
    private val engine: AssistantEngine,
    private val reconcile: () -> Unit = { AssistantReplication.reconcileNow(context) },
    private val commandAdmission: (String) -> Unit = {},
    private val enginePrewarm: () -> Unit = { engine.prewarm() },
    private val prewarm: (String) -> Unit = AssistantRuntime::prewarm,
) : AssistantPhoneBackend {
    constructor(context: Context) : this(context, AssistantRuntime.get(context))
    private val delivery = AtomicReference<PhoneRequestDelivery?>(null)
    private val ingressRetired = AtomicBoolean(false)
    internal fun isIngressRetired() = ingressRetired.get()
    /** Seal scheduling first; the owning session still verifies provider/actor retirement separately. */
    @Synchronized internal fun retireIngress() {
        if (!ingressRetired.compareAndSet(false, true)) return
        delivery.get()?.let { current ->
            current.cancel()
            engine.cancel(current.clientRequestId)
        }
        val pending = synchronized(admission) {
            initializingConversation = false
            startupWaiters.toList().also { startupWaiters.clear() }
        }
        pending.forEach { it(Result.failure(retiredFailure())) }
    }
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
        .put("fast_mode", state.fastMode)
        .put("speech_language", state.speechLanguage).put("speech_tts_provider", state.speechTtsProvider).put("speech_tts_voice", state.speechTtsVoice)
    @Synchronized fun publishSpeechOptions(options: dev.gitblame.voxgolem.assistant.speech.SpeechOptions) {
        if (!ingressRetired.get()) engine.publishSpeechOptions(options)
    }
    fun busy(): Boolean = engine.snapshot().busy || delivery.get() != null || preferencesPending.get()
    @Synchronized fun saveSpeechDraft(text: String) { if (!ingressRetired.get()) engine.setDraft(text) }
    fun speechConversation(): String? = engine.snapshot().conversation
    fun speechHead(): String? = engine.snapshot().selectedHead
    fun speechAdmissionSnapshot(): SpeechAdmissionSnapshot = engine.snapshot().let {
        SpeechAdmissionSnapshot(SpeechInputOwner(it.conversation, it.selectedHead), it.draft)
    }
    @Synchronized fun observeSpeechContext(observer: () -> Unit): AutoCloseable {
        if (ingressRetired.get()) return AutoCloseable {}
        return engine.subscribe { synchronized(this) { if (!ingressRetired.get()) observer() } }
    }
    @Synchronized fun saveOwnedSpeechDraft(text: String, conversation: String?) {
        if (!ingressRetired.get()) engine.setSpeechDraft(text, conversation)
    }
    @Synchronized fun saveOwnedSpeechDraft(text: String, conversation: String?, head: String?) {
        if (!ingressRetired.get()) engine.setSpeechDraft(text, conversation, expectedHead = head, requireHeadOwner = true)
    }
    fun stopSpeechRequest(requestId: String) { cancelDelivery(requestId) }
    @Synchronized fun prewarm() { if (!ingressRetired.get()) runCatching { enginePrewarm() } }
    @Synchronized fun observeSpeechStream(listener: (String?, String) -> Unit): AutoCloseable {
        if (ingressRetired.get()) return AutoCloseable {}
        return engine.observeProvisionalStream { id, text -> synchronized(this) { if (!ingressRetired.get()) listener(id, text) } }
    }

    @Synchronized override fun command(name: String, arguments: JSONObject, reply: (Result<Any?>) -> Unit) {
        if (ingressRetired.get()) { reply(Result.failure(retiredFailure())); return }
        try { commandAdmission(name) } catch (error: RuntimeException) { reply(Result.failure(error)); return }
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
            synchronized(this) {
                if (ingressRetired.get()) reply(Result.failure(retiredFailure()))
                else try {
                    requireSettled(state)
                    reconcile()
                    publishSnapshot(reply)
                } catch (error: RuntimeException) { reply(Result.failure(error)) }
            }
        }
        try {
            when (name) {
                "get_app_snapshot" -> engine.selectedContext { raw, state ->
                    synchronized(this) {
                        if (ingressRetired.get()) reply(Result.failure(retiredFailure()))
                        else {
                            if (state.conversation == null && NewVoiceShortcutLauncher.actions.hasAction()) deferEmptyShortcutConversation = true
                            if (state.conversation == null && !state.busy && !deferEmptyShortcutConversation) {
                                ensureConversation(state, reply)
                            } else replySnapshot(reply, state, raw)
                        }
                    }
                }
                "new_conversation" -> {
                    val draft = newConversationDraft(arguments)
                    if (draft.isNotEmpty()) engine.resetWithDraft(draft, ::settled)
                    else engine.reset(::settled)
                }
                "select_conversation" -> engine.selectConversation(PhoneIdentity.native(arguments.getString("conversationId")), ::settled)
                "select_conversation_head" -> engine.selectHead(PhoneIdentity.native(arguments.getString("headId")), ::settled)
                "save_assistant_draft" -> {
                    val text = arguments.getString("text")
                    val wasBlank = engine.snapshot().draft.isBlank()
                    engine.setDraft(text) { state ->
                        synchronized(this) {
                            if (ingressRetired.get()) reply(Result.failure(retiredFailure()))
                            else {
                                if (wasBlank && text.isNotBlank() && state.error == null) runCatching { enginePrewarm() }
                                reply(runCatching { requireSettled(state); null })
                            }
                        }
                    }
                }
                "set_preferences" -> updatePreferences(arguments.getJSONObject("preferences"), reply)
                "list_assistant_conversations" -> {
                    val expectedRevision = arguments.getString("expectedRevision")
                    val offset = arguments.getInt("offset")
                    require(offset in 0..16384)
                    engine.conversationPage(expectedRevision, offset) { raw, state, errorCode ->
                        reply(runCatching {
                            if (raw == null) {
                                if (errorCode == "revision_conflict") throw AssistantPhoneFailure("busy", "Conversation inventory changed; refresh and retry")
                                errorCode?.let { throw providerFailure(it) }
                                requireSettled(state)
                                throw AssistantPhoneFailure("internal", "Conversation page is unavailable")
                            }
                            val page = JSONObject(raw)
                            val records = page.getJSONArray("conversations")
                            val projected = org.json.JSONArray()
                            for (index in 0 until records.length()) {
                                val item = records.getJSONObject(index)
                                projected.put(JSONObject().put("id", PhoneSnapshotEncoder.wireId(item.getJSONObject("id"))).put("title", item.getString("title")))
                            }
                            JSONObject().put("revision", page.getString("revision")).put("conversations", projected).put("next_offset", page.getInt("next_offset"))
                        })
                    }
                }
                "list_assistant_heads" -> {
                    val expectedRevision = arguments.getString("expectedRevision")
                    val conversation = PhoneIdentity.native(arguments.getString("conversationId"))
                    val offset = arguments.getInt("offset")
                    require(offset in 0..16384)
                    engine.headPage(expectedRevision, conversation, null, offset) { raw, state, errorCode ->
                        reply(runCatching {
                            if (raw == null) {
                                if (errorCode == "revision_conflict") throw AssistantPhoneFailure("busy", "Continuation inventory changed; refresh and retry")
                                errorCode?.let { throw providerFailure(it) }
                                requireSettled(state)
                                throw AssistantPhoneFailure("internal", "Continuation page is unavailable")
                            }
                            val page = JSONObject(raw)
                            val records = page.getJSONArray("heads")
                            val projected = org.json.JSONArray()
                            for (index in 0 until records.length()) {
                                projected.put(PhoneSnapshotEncoder.wireId(records.getJSONObject(index)))
                            }
                            JSONObject().put("revision", page.getString("revision")).put("heads", projected).put("next_offset", page.getInt("next_offset"))
                        })
                    }
                }
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

    @Synchronized private fun ensureConversation(before: AssistantState, reply: (Result<Any?>) -> Unit) {
        if (ingressRetired.get()) { reply(Result.failure(retiredFailure())); return }
        synchronized(admission) {
            if (startupWaiters.size >= 16) { reply(Result.failure(providerFailure("busy"))); return }
            startupWaiters += reply
            if (initializingConversation) return
            initializingConversation = true
        }
        fun finish(state: AssistantState) {
            synchronized(this) {
                if (state.error == null && !ingressRetired.get()) reconcile()
                val waiters = synchronized(admission) { initializingConversation = false; startupWaiters.toList().also { startupWaiters.clear() } }
                for (waiting in waiters) {
                    if (ingressRetired.get()) waiting(Result.failure(retiredFailure()))
                    else if (state.error != null) waiting(Result.failure(providerFailure(state.error)))
                    else publishSnapshot(waiting)
                }
            }
        }
        engine.newConversation { created ->
            synchronized(this) {
                if (!ingressRetired.get() && created.error == null && before.draft.isNotEmpty()) engine.setDraft(before.draft, ::finish)
                else finish(created)
            }
        }
    }

    @Synchronized private fun publishSnapshot(reply: (Result<Any?>) -> Unit) {
        if (ingressRetired.get()) { reply(Result.failure(retiredFailure())); return }
        engine.selectedContext { raw, state -> replySnapshot(reply, state, raw) }
    }

    @Synchronized private fun replySnapshot(reply: (Result<Any?>) -> Unit, state: AssistantState, raw: String?) {
        if (ingressRetired.get()) { reply(Result.failure(retiredFailure())); return }
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
            providerAvailable = ready, providerReason = reason, optionalProviders = AssistantRuntime.optionalProviderReadiness())
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
        var previousModel: String? = null
        engine.updatePreferences(selectedModel, dev.gitblame.voxgolem.assistant.runtime.AssistantPolicy(
            value.getBoolean("web_enabled"), value.getBoolean("speech_network_fallback"), value.getString("speech_language"),
            value.getString("speech_tts_provider"), value.getString("speech_tts_voice"), value.getBoolean("fast_mode")), {
            previousModel = engine.snapshot().model
            preferences.write(value)
        }) { selected ->
            synchronized(this) {
                preferencesPending.set(false)
                if (ingressRetired.get()) reply(Result.failure(retiredFailure()))
                else if (selected.error != null) reply(Result.failure(providerFailure(selected.error)))
                else {
                    if (previousModel != null && selected.model == selectedModel && selected.model != previousModel) {
                        runCatching { prewarm(selected.model) }
                    }
                    // A view listener failure must not misclassify already persisted settings.
                    runCatching { onPreferencesChanged?.invoke(value.getBoolean("tts_enabled")) }
                    reply(Result.success(preferencesFor(selected)))
                }
            }
        }
    }

    override fun submit(clientRequestId: String, prompt: String, update: (JSONObject) -> Unit, terminal: (Result<Any?>) -> Unit) {
        submitForOwner(clientRequestId, prompt, update, terminal, null)
    }
    fun submitOwnedSpeech(clientRequestId: String, prompt: String, update: (JSONObject) -> Unit, terminal: (Result<Any?>) -> Unit,
        owner: SpeechInputOwner) {
        submitForOwner(clientRequestId, prompt, update, terminal, owner)
    }
    @Synchronized private fun submitForOwner(clientRequestId: String, prompt: String, update: (JSONObject) -> Unit, terminal: (Result<Any?>) -> Unit,
        owner: SpeechInputOwner?) {
        if (ingressRetired.get()) { terminal(Result.failure(retiredFailure())); return }
        val current = PhoneRequestDelivery(clientRequestId,
            { value -> synchronized(this) { if (!ingressRetired.get()) update(value) } },
            { synchronized(this) { if (!ingressRetired.get()) engine.rejectDelivery(clientRequestId) } })
        val admitted = synchronized(admission) { !preferencesPending.get() && !initializingConversation && delivery.compareAndSet(null, current) }
        if (!admitted) {
            terminal(Result.failure(AssistantPhoneFailure("busy", "A request is already active"))); return
        }
        val listener = dev.gitblame.voxgolem.assistant.runtime.AssistantResultListener { result, resultState ->
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
                        if (owner != null) committedVoiceInputContext(record).let { committed ->
                            outcome.put("voiceContext", JSONObject().put("conversation", committed.conversation)
                                .put("selectedHead", committed.selectedHead))
                        }
                        val reconciliationFailed = !ingressRetired.get() && runCatching { reconcile() }.isFailure
                        if (result.code == "saved_local_state_failed" || reconciliationFailed) {
                            outcome.put("warning", providerFailure("saved_local_state_failed").boundedMessage)
                        } else if (!ingressRetired.get()) {
                            attachTerminalSnapshot(outcome, result, resultState)
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
        if (owner == null) engine.submit(clientRequestId, prompt, current.gate, listener, terminalContext = true)
        else engine.submitSpeech(clientRequestId, prompt, owner.conversation, listener,
            expectedHead = owner.selectedHead, delivery = current.gate, terminalContext = true)
    }

    @Synchronized override fun acknowledge(clientRequestId: String, providerRequestId: String, revision: String, accepted: Boolean): Boolean =
        if (ingressRetired.get()) false else delivery.get()?.takeIf { it.clientRequestId == clientRequestId }?.acknowledge(providerRequestId, revision, accepted) ?: false

    internal fun attachTerminalSnapshot(outcome: JSONObject,
        result: dev.gitblame.voxgolem.assistant.runtime.AssistantRequestResult, resultState: AssistantState) {
        if (ingressRetired.get()) return
        val raw = result.terminalContextJson ?: return
        // Encode first; the transactional fence then observes any already-committed mutation,
        // including imports committed while encoding or while reconcileNow was being queued.
        runCatching { encode(resultState, JSONObject(raw)) }.onSuccess { encoded ->
            if (engine.terminalContextStillCurrent(result, resultState)) outcome.put("app_snapshot", encoded)
        }
    }

    @Synchronized override fun cancelDelivery(clientRequestId: String) {
        delivery.get()?.takeIf { it.clientRequestId == clientRequestId }?.cancel()
        if (!ingressRetired.get()) engine.cancel(clientRequestId)
    }

    @Synchronized override fun foreground(visible: Boolean) {
        if (ingressRetired.get()) return
        if (visible) {
            engine.resume()
            runCatching { enginePrewarm() }
            reconcile()
        } else {
            delivery.get()?.cancel()
            engine.background()
        }
    }

    private fun retiredFailure() = AssistantPhoneFailure("unavailable", "This phone request owner has retired")

    companion object {
        internal fun newConversationDraft(arguments: JSONObject): String =
            if (arguments.isNull("draft")) "" else arguments.optString("draft", "")

        fun providerFailure(code: String?): AssistantPhoneFailure = when (code) {
            "busy" -> AssistantPhoneFailure("busy", "A request is already active")
            "invalid_request", "empty_prompt", "prompt_too_large" -> AssistantPhoneFailure("invalid_input", "Prompt or request ID is invalid")
            "unsupported_web_permission" -> AssistantPhoneFailure("unavailable", "Selected model cannot enforce disabled web tools. Choose another model or change Web permission explicitly.")
            "retirement_failed" -> AssistantPhoneFailure("unavailable", "Engine retirement exceeded its deadline. The draft is retained; retry explicitly.")
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
            "aicore_not_available" -> AssistantPhoneFailure("unavailable", "Gemini Nano is not available on this device yet. It needs a locked bootloader, an up-to-date AICore, and (on Pixel 8) enrolment in the AICore developer preview. Other models still work.")
            "aicore_needs_update" -> AssistantPhoneFailure("unavailable", "Gemini Nano requires an AICore update from Google Play")
            "aicore_busy" -> AssistantPhoneFailure("busy", "Gemini Nano on-device service is busy")
            "aicore_downloading" -> AssistantPhoneFailure("unavailable", "Gemini Nano model is downloading in the background. Please wait and try again shortly.")
            "aicore_unavailable" -> AssistantPhoneFailure("unavailable", "Gemini Nano on-device model is currently unavailable")
            "internal_error" -> AssistantPhoneFailure("internal", "Assistant internal error. The draft is retained; retry explicitly.")
            else -> AssistantPhoneFailure("unavailable", "Assistant request failed. The draft is retained; retry explicitly.")
        }
    }
}
