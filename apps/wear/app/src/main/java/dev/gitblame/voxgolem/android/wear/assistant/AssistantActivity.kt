package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Intent
import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.view.InputDevice
import android.view.MotionEvent
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.*
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import dev.gitblame.voxgolem.assistant.runtime.*
import dev.gitblame.voxgolem.assistant.replication.*
import dev.gitblame.voxgolem.assistant.speech.SpeechSelection
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference

internal class AssistantHistoryMemory : ViewModel() {
    var snapshot = AssistantHistoryState()
    val prompts = AssistantPromptPresentation()
}

private data class AssistantPromptTransition(
    val conversation: String?, val head: String?, val request: String?, val busy: Boolean,
    val draft: String, val error: String?, val branches: List<String>,
) {
    constructor(state: AssistantState) : this(state.conversation, state.selectedHead, state.clientRequestToken,
        state.busy, state.draft, state.error, state.branchChoices)
}

private data class AssistantPendingSpeechSelection(val token: Long, val candidate: SpeechSelection)

open class AssistantActivity : ComponentActivity() {
    private lateinit var client: AssistantClient
    private var state by mutableStateOf(AssistantState())
    private lateinit var history: AssistantHistory
    private lateinit var replyMetrics: AssistantReplyMetrics
    private lateinit var promptPresentation: AssistantPromptPresentation
    private var promptSource = AssistantState()
    private val promptTransition = AtomicReference<AssistantPromptTransition?>(null)
    internal var pendingPrompts by mutableStateOf(emptyList<AssistantPendingPrompt>())
        private set
    internal var historyState by mutableStateOf(AssistantHistoryState())
        private set
    private var subscription: AutoCloseable? = null
    private lateinit var voice: AssistantVoiceController
    private lateinit var voiceSettings: WearVoiceSettings
    internal var voicePreferences by mutableStateOf(WearVoicePreferences())
        private set
    private var voiceState by mutableStateOf(AssistantVoiceState())
    private var voiceSubscription: AutoCloseable? = null
    private data class VoiceAdmission(val conversationMode: Boolean, val draft: String, val conversation: String?, val head: String?)
    private var permissionAction: VoiceAdmission? by mutableStateOf(null)
    private var entryMode by mutableStateOf<AssistantVoiceEntryMode?>(null)
    private var permissionError by mutableStateOf<String?>(null)
    internal var speechSelectionError by mutableStateOf<String?>(null)
        private set
    private var pendingSpeechSelection by mutableStateOf<AssistantPendingSpeechSelection?>(null)
    private var speechSelectionToken = 0L
    internal val speechSelectionPending: Boolean get() = pendingSpeechSelection != null
    internal var pairing by mutableStateOf(PeerPairingState(PeerPairingReason.WAITING_FOR_PEER))
        private set
    private var linkStatusPending = false
    private var linkEpoch = 0L
    private val handler = Handler(Looper.getMainLooper())
    private val screenAwake by lazy { AssistantScreenAwake(handler) { enabled ->
        if (enabled) window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        else window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    } }
    private val queued = AtomicBoolean(false)
    private val latest = AtomicReference(AssistantState())
    @Volatile private var visible = false
    private var resumed = false
    private var pendingVoiceLaunch: Boolean? by mutableStateOf(null)
    internal val voiceAdmissionPending: Boolean get() = pendingVoiceLaunch != null || permissionAction != null
    internal fun presentedVoiceState() = voiceState.copy(error = permissionError ?: voiceState.error, entryMode = entryMode,
        admissionPending = voiceAdmissionPending)
    private var startupTiming: AssistantTiming? = null
    @Volatile private var observerEpoch = 0L
    private var input = AssistantImeSession()
    private val editor = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result -> receiveText(result.resultCode, result.data) }
    private val microphone = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted -> receiveMicrophonePermission(granted) }
    private val updateUi = Runnable {
        queued.set(false)
        if (visible) {
            val projection = latest.get()
            replyMetrics.remember(projection.providerRequestId, projection.firstTextMs)
            state = projection
            if (resumed && state.foreground) startupTiming?.finish(Stage.FIRST_PROJECTION)
            if (resumed) { history.select(state); consumeVoiceLaunch() }
        }
    }
    private val updateLink = object : Runnable {
        override fun run() {
            if (!visible || !resumed) return
            refreshLinkStatus()
            handler.postDelayed(this, 3000)
        }
    }

    protected open fun assistantClient(): AssistantClient = AssistantRuntime.get(applicationContext)
    protected open fun assistantVoice(client: AssistantClient): AssistantVoiceController = WearAssistantVoice(this, client as AssistantEngine)
    internal open fun assistantVoiceSettings(): WearVoiceSettings = WearVoiceSettings(this)
    private fun admitLaunch(entry: Boolean?) {
        pendingVoiceLaunch = null
        if (entry == null || voice.snapshot().active || permissionAction != null) return
        entryMode = if (entry) AssistantVoiceEntryMode.TALK else AssistantVoiceEntryMode.ASK
        pendingVoiceLaunch = entry
    }

    private fun beginStartup(stage: Stage) {
        startupTiming?.finish(Stage.REPLACED)
        startupTiming = AssistantTiming.begin(applicationContext, Operation.ACTIVITY).also { it.mark(stage) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        beginStartup(Stage.ACTIVITY_CREATED)
        replyMetrics = AssistantReplyMetrics(applicationContext)
        val launchEntry = if (savedInstanceState == null) voiceEntry(intent) else null
        entryMode = savedInstanceState?.getString("assistant.voice.entry")?.let { runCatching { AssistantVoiceEntryMode.valueOf(it) }.getOrNull() }
        client = assistantClient()
        if (client is AssistantEngine) AssistantRuntime.configureCaptureResourceAdmission(true)
        voice = assistantVoice(client)
        voiceSettings = assistantVoiceSettings()
        readVoicePreferences()
        state = client.snapshot()
        if (launchEntry != null) admitLaunch(launchEntry)
        val historyMemory = ViewModelProvider(this)[AssistantHistoryMemory::class.java]
        promptPresentation = historyMemory.prompts
        promptPresentation.beginRecognitionOwner()
        promptSource = state
        historyState = historyMemory.snapshot
        history = AssistantHistory(request = { before, settled ->
            client.history(before) { page, projection -> handler.post { settled(page, projection) } }
        }, initial = historyState) { historyState = it; historyMemory.snapshot = it; projectPrompts() }
        projectPrompts()
        input = AssistantImeSession(savedInstanceState?.getBoolean("assistant.editor.pending") ?: false)
        setContent {
            SideEffect {
                val engine = client as? AssistantEngine
                if (state.busy && state.provisional.isNotBlank()) engine?.markProvisionalStateApplied(state.clientRequestToken)
            }
            AssistantTheme {
                AssistantScreen(state, presentedVoiceState(), historyState, pairing, voicePreferences,
                    AssistantActions(::editDraft, ::sendManually, ::stopAll, ::newConversation,
                        ::selectConversation, ::selectHead, client::selectModel,
                        { requestVoice(false) }, { requestVoice(true) }, voice::endUtterance, ::setReadRepliesAloud, voice::speak,
                         ::recheck, history::older, history::retry, ::setConversationMode, client::selectSearchMode,
                           ::setWebEnabled, ::setSpeechNetworkFallback, ::setSpeechLanguage, ::setSpeechTtsProvider,
                             ::setSpeechTtsVoice, ::resetSpeechSelection, prepareSpeechOptions = ::prepareSpeechOptions, openSpeechStore = ::openSpeechStore), pendingPrompts, speechSelectionError, speechSelectionPending,
                    replyMetrics = replyMetrics::get)
            }
        }
    }

    override fun onStart() {
        super.onStart()
        visible = true
        val token = ++observerEpoch
        promptTransition.set(null)
        subscription = client.subscribe { projection ->
            if (visible && observerEpoch == token) {
                val awakeToken = screenAwake.token
                val occurredAt = SystemClock.uptimeMillis()
                handler.post {
                    if (visible && observerEpoch == token) screenAwake.chatProgress(projection, awakeToken, occurredAt)
                }
                latest.set(projection)
                // Request ownership survives coalesced streaming redraws.
                val transition = AssistantPromptTransition(projection)
                if (promptTransition.getAndSet(transition) != transition) handler.post {
                    if (visible && observerEpoch == token) {
                        promptSource = projection
                        projectPrompts()
                    }
                }
                if (queued.compareAndSet(false, true)) handler.post(updateUi)
            }
        }
        voiceSubscription = voice.subscribe { projection ->
            val awakeToken = screenAwake.token
            val occurredAt = SystemClock.uptimeMillis()
            handler.post {
                if (visible && observerEpoch == token) {
                    screenAwake.voiceProgress(projection, awakeToken, occurredAt)
                    voiceState = projection
                    projectPrompts()
                }
            }
        }
    }

    override fun onResume() {
        super.onResume()
        if (startupTiming?.isFinished == true) beginStartup(Stage.RESUMED) else startupTiming?.mark(Stage.RESUMED)
        resumed = true
        screenAwake.resume(client.snapshot(), voice.snapshot())
        client.resume()
        history.select(client.snapshot())
        voice.foreground(true)
        readVoicePreferences()
        voice.tts(voicePreferences.readRepliesAloud)
        if (client is AssistantEngine) recheck()
        handler.removeCallbacks(updateLink)
        handler.post(updateLink)
        consumeVoiceLaunch()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        beginStartup(Stage.REQUESTED)
        if (visible && resumed && client.snapshot().foreground) startupTiming?.finish(Stage.READY_REUSED)
        setIntent(intent)
        admitLaunch(voiceEntry(intent))
        consumeVoiceLaunch()
    }

    private fun consumeVoiceLaunch() {
        val conversation = pendingVoiceLaunch ?: return
        if (!visible || !resumed || !client.snapshot().foreground) return
        pendingVoiceLaunch = null
        if (isFinishing || isDestroyed || client.snapshot().busy || voice.snapshot().active || input.pending || permissionAction != null) return
        requestVoice(conversation)
    }

    override fun onPause() {
        startupTiming?.finish(Stage.CANCELLED)
        retireSpeechSelection()
        pendingVoiceLaunch = null
        screenAwake.suspend()
        permissionAction = null
        history.suspendReads()
        linkEpoch++
        linkStatusPending = false
        handler.removeCallbacks(updateLink)
        if (resumed) { resumed = false; voice.foreground(false); client.background() }
        super.onPause()
    }

    override fun onStop() {
        retireSpeechSelection()
        pendingVoiceLaunch = null
        permissionAction = null
        screenAwake.suspend()
        visible = false
        history.suspendReads()
        observerEpoch++
        linkStatusPending = false
        handler.removeCallbacks(updateLink)
        subscription?.close(); subscription = null
        voiceSubscription?.close(); voiceSubscription = null
        if (resumed) { resumed = false; voice.foreground(false); client.background() }
        super.onStop()
    }

    override fun onSaveInstanceState(outState: Bundle) {
        outState.putBoolean("assistant.editor.pending", input.pending)
        entryMode?.let { outState.putString("assistant.voice.entry", it.name) }
        super.onSaveInstanceState(outState)
    }

    internal fun editDraft() {
        if (!visible || !resumed || isFinishing || isDestroyed || client.snapshot().busy || !input.begin()) return
        val currentVoice = voice.snapshot()
        val prefill = currentVoice.dictation.takeIf {
            it.phase in setOf(dev.gitblame.voxgolem.assistant.recognition.DictationPhase.LISTENING,
                dev.gitblame.voxgolem.assistant.recognition.DictationPhase.FINISHING) && it.draft.isNotBlank()
        }?.draft ?: client.snapshot().draft
        pendingVoiceLaunch = null; permissionAction = null
        voice.stop()
        editor.launch(Intent(this, AssistantTextEntryActivity::class.java).putExtra(AssistantTextInput.DRAFT, prefill))
    }

    private fun projectPrompts() { pendingPrompts = promptPresentation.project(promptSource, voiceState, historyState) }

    internal fun receiveText(resultCode: Int, intent: Intent?) {
        input.accept(resultCode, intent)?.let(client::setDraft)
    }

    internal fun sendManually() {
        if (!visible || !resumed || isFinishing || isDestroyed || client.snapshot().busy || voiceAdmissionPending) return
        permissionAction = null
        if (!voice.send()) client.send()
    }

    internal fun requestVoice(conversation: Boolean) {
        if (!visible || !resumed || isFinishing || isDestroyed || !client.snapshot().foreground || client.snapshot().busy || voice.snapshot().active || input.pending || permissionAction != null) return
        entryMode = if (conversation) AssistantVoiceEntryMode.TALK else AssistantVoiceEntryMode.ASK
        pendingVoiceLaunch = null
        permissionError = null
        if (!voice.snapshot().admissionReady) {
            if (conversation) voice.conversation(client.snapshot().draft) else voice.ask(client.snapshot().draft)
            return
        }
        val current = client.snapshot()
        permissionAction = VoiceAdmission(conversation, current.draft, current.conversation, current.selectedHead)
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) receiveMicrophonePermission(true)
        else microphone.launch(Manifest.permission.RECORD_AUDIO)
    }

    internal fun receiveMicrophonePermission(granted: Boolean) {
        val admission = permissionAction ?: return
        permissionAction = null
        if (!visible || !resumed || isFinishing || isDestroyed || !client.snapshot().foreground || client.snapshot().busy || voice.snapshot().active ||
            client.snapshot().conversation != admission.conversation || client.snapshot().selectedHead != admission.head || client.snapshot().draft != admission.draft) return
        if (granted) {
            if (admission.conversationMode) voice.conversation(admission.draft) else voice.ask(admission.draft)
        } else permissionError = "microphone_permission_required"
    }

    internal fun stopAll() { pendingVoiceLaunch = null; permissionAction = null; voice.stop(); client.stop() }
    internal fun openSpeechStore() { dev.gitblame.voxgolem.assistant.speech.SpeechServiceRequirement.openPlayStore(this) }
    private fun canChangeVoiceContext() = visible && resumed && !voiceAdmissionPending && !voice.snapshot().active && !client.snapshot().busy
    internal fun newConversation() { if (canChangeVoiceContext()) client.newConversation() }
    internal fun selectConversation(id: String) { if (canChangeVoiceContext()) client.selectConversation(id) }
    internal fun selectHead(id: String) { if (canChangeVoiceContext()) client.selectHead(id) }
    internal fun setWebEnabled(enabled: Boolean) { client.setWebEnabled(enabled) }
    internal fun setSpeechNetworkFallback(enabled: Boolean) {
        if (assistantCanChangeSpeechFallback(client.snapshot(), voice.snapshot())) {
            client.setSpeechNetworkFallback(enabled)
            speechSelectionError = null
        }
        else speechSelectionError = "speech_selection_rejected"
    }
    internal fun setSpeechLanguage(value: String) = submitSpeechSelection(assistantSpeechCandidate(
        client.snapshot().speechAdmission().selection, AssistantSpeechField.LANGUAGE, value))
    internal fun setSpeechTtsProvider(value: String) = submitSpeechSelection(assistantSpeechCandidate(
        client.snapshot().speechAdmission().selection, AssistantSpeechField.ENGINE, value))
    internal fun setSpeechTtsVoice(value: String) = submitSpeechSelection(assistantSpeechCandidate(
        client.snapshot().speechAdmission().selection, AssistantSpeechField.VOICE, value))
    internal fun resetSpeechSelection() = submitSpeechSelection(SpeechSelection())
    internal fun prepareSpeechOptions() {
        if (!visible || !resumed || isFinishing || isDestroyed || voiceAdmissionPending) return
        voice.prepareSpeechOptions()
    }
    private fun submitSpeechSelection(selection: SpeechSelection) {
        if (speechSelectionPending) {
            speechSelectionError = "speech_selection_pending"
            return
        }
        val current = client.snapshot()
        if (!visible || !resumed || isFinishing || isDestroyed || input.pending || permissionAction != null ||
            !assistantCanSelectSpeech(current, voice.snapshot())) {
            speechSelectionError = "speech_selection_rejected"
            return
        }
        val pending = AssistantPendingSpeechSelection(++speechSelectionToken, selection)
        pendingSpeechSelection = pending
        speechSelectionError = null
        val accepted = try {
            voice.configureSpeech(pending.candidate) { persisted -> handler.post {
                // Ordinary snapshots, including equal fields, are not this operation's settlement.
                if (pendingSpeechSelection?.token != pending.token || !visible || !resumed || isFinishing || isDestroyed) return@post
                pendingSpeechSelection = null
                speechSelectionError = if (persisted) null else "speech_selection_not_saved"
            } }
        } catch (_: RuntimeException) { false }
        if (!accepted && pendingSpeechSelection?.token == pending.token) {
            pendingSpeechSelection = null
            speechSelectionError = "speech_selection_rejected"
        }
    }
    private fun retireSpeechSelection() {
        speechSelectionToken++
        if (speechSelectionPending) speechSelectionError = "speech_selection_unconfirmed"
        pendingSpeechSelection = null
    }
    private fun readVoicePreferences() {
        voicePreferences = voiceSettings.current()
        try { voicePreferences = voiceSettings.read() }
        catch (_: IllegalStateException) { permissionError = "settings_not_saved" }
    }
    internal fun setReadRepliesAloud(enabled: Boolean) {
        try {
            voiceSettings.setReadRepliesAloud(enabled)
            voicePreferences = voiceSettings.read()
            permissionError = null
            voice.tts(enabled)
        } catch (_: IllegalStateException) { permissionError = "settings_not_saved" }
    }

    internal fun setConversationMode(enabled: Boolean) {
        try {
            voiceSettings.setConversationMode(enabled)
            voicePreferences = voiceSettings.read()
            entryMode = null
            permissionError = null
        } catch (_: IllegalStateException) { permissionError = "settings_not_saved" }
    }
    override fun dispatchTouchEvent(event: MotionEvent): Boolean {
        if (visible && resumed && !isFinishing && !isDestroyed && event.actionMasked in setOf(
                MotionEvent.ACTION_DOWN, MotionEvent.ACTION_MOVE, MotionEvent.ACTION_UP,
                MotionEvent.ACTION_POINTER_DOWN, MotionEvent.ACTION_POINTER_UP)) screenAwake.interaction()
        return super.dispatchTouchEvent(event)
    }

    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        if (visible && resumed && !isFinishing && !isDestroyed && event.actionMasked == MotionEvent.ACTION_SCROLL &&
            event.isFromSource(InputDevice.SOURCE_ROTARY_ENCODER) && event.getAxisValue(MotionEvent.AXIS_SCROLL) != 0f)
            screenAwake.interaction()
        return super.dispatchGenericMotionEvent(event)
    }

    override fun onDestroy() {
        screenAwake.suspend()
        retireSpeechSelection(); voice.close(); handler.removeCallbacks(updateUi); handler.removeCallbacks(updateLink)
        super.onDestroy()
    }

    protected open fun readLinkStatus(result: (PeerPairingState) -> Unit) {
        if (client is AssistantEngine) AssistantReplication.pairingState(applicationContext, result)
        else result(PeerPairingState(PeerPairingReason.UNAVAILABLE))
    }

    private fun refreshLinkStatus() {
        if (linkStatusPending) return
        linkStatusPending = true
        val token = linkEpoch
        readLinkStatus { value -> handler.post {
            if (visible && resumed && token == linkEpoch) {
                linkStatusPending = false
                pairing = value
            }
        } }
    }

    private fun recheck() {
        client.refresh()
        if (client !is AssistantEngine) return
        refreshLinkStatus()
        AssistantReplication.reconcileNow(applicationContext)
    }

    companion object {
        internal const val VOICE_ENTRY = "dev.gitblame.voxgolem.android.wear.assistant.AssistantVoiceEntry"
        internal const val CONVERSATION_ENTRY = "dev.gitblame.voxgolem.android.wear.assistant.AssistantConversationEntry"
        private fun voiceEntry(intent: Intent?): Boolean? {
            if (intent == null || intent.action != Intent.ACTION_MAIN || !intent.hasCategory(Intent.CATEGORY_LAUNCHER) ||
                intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0) return null
            return when (intent.component?.className) {
                VOICE_ENTRY -> false
                CONVERSATION_ENTRY -> true
                else -> null
            }
        }
    }
}

internal fun assistantLinkStatusText(reason: PeerPairingReason): String = when (reason) {
    PeerPairingReason.ENROLLED -> "Phone linked automatically · conversations sync on reconnect"
    PeerPairingReason.WAITING_FOR_PEER -> "Waiting for the paired phone's VoxGolem app"
    PeerPairingReason.PEER_UPDATE_REQUIRED -> "Update VoxGolem on the paired phone to enable sharing"
    PeerPairingReason.MULTIPLE_PEERS -> "Multiple matching phones found · automatic sharing needs one counterpart"
    PeerPairingReason.DISCOVERY_FAILED -> "Phone discovery failed · native sync will retry"
    PeerPairingReason.STORAGE_FAILURE -> "Phone link could not be saved · check device storage"
    PeerPairingReason.UNAVAILABLE -> "Automatic phone sharing is unavailable"
}

internal fun assistantErrorText(code: String): String = when (code) {
    "auth_unavailable" -> "Provider access is unavailable. Draft retained."
    "auth_expired", "unauthorized" -> "Provider access expired or was rejected. Draft retained."
    "forbidden" -> "Provider denied access to this model."
    "transport", "timeout" -> "Network request failed. Draft retained; retry deliberately."
    "not_saved" -> "Answer not saved. Draft retained; no synced completion."
    "delivery_failed" -> "Delivery was not acknowledged. Answer not saved; draft retained."
    "interrupted" -> "Stopped. Draft retained."
    "choose_continuation" -> "Choose a continuation before sending."
    "busy" -> "Wait for the current request to retire."
    "unsupported_model" -> "Choose a supported Custom model."
    "prompt_too_large" -> "Draft exceeds 32 KiB. Previous draft retained."
    "saved_local_state_failed" -> "Answer saved, but local selection/draft update failed."
    else -> "Assistant error ($code). Draft retained; no automatic retry."
}

internal fun assistantVoiceErrorText(code: String): String = when (code) {
    "settings_not_saved" -> "Settings could not be saved. Recheck your choice before continuing."
    "capture_admission_unavailable" -> "Microphone sharing is not connected. Use keyboard."
    "on_device_recognition_unavailable", "local_language_unavailable" -> "Native dictation is unavailable for this language. Use keyboard; no voice-model download."
    "recognition_busy" -> "Audio is busy. Note is not preempted; use keyboard."
    "recognition_error_7", "no_speech" -> "No speech recognized. Tap Mic to try again."
    "local_voice_unavailable" -> "No installed matching TTS voice. No voice-model download."
    "microphone_permission_required" -> "Microphone permission required. Tap Mic again deliberately."
    else -> "Speech stopped ($code). Typing remains available; resume deliberately."
}
