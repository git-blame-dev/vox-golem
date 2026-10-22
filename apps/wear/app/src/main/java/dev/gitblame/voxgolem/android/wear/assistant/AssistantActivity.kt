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
import dev.gitblame.voxgolem.assistant.AssistantDiagnosticJournal.Metadata
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
    private var voiceSettingsReady = false
    private var voiceSettingsLoadFinished = false
    private var voiceSettingsRequest = 0L
    private var voiceSettingsClosed = false
    private var voiceSettingsRejected = false
    // Accessed only by the ordered settings worker; failed commits must not project their
    // SharedPreferences in-memory mutation as a durable acknowledgement.
    private var acknowledgedVoicePreferences: WearVoicePreferences? = null
    internal var voicePreferences by mutableStateOf(WearVoicePreferences())
        private set
    private var voiceState by mutableStateOf(AssistantVoiceState())
    private var voiceSubscription: AutoCloseable? = null
    private data class VoiceAdmission(val conversationMode: Boolean, val draft: String, val conversation: String?, val head: String?, val timing: AssistantTiming)
    private var voiceFlowTiming: AssistantTiming? = null
    private var permissionAction: VoiceAdmission? by mutableStateOf(null)
    private var entryMode by mutableStateOf<AssistantVoiceEntryMode?>(null)
    private var voiceEntryOrdinal = 0L
    private var freshEntryBeforeSettingsRead = false
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
    private lateinit var editorDraftMemory: AssistantEditorDraftMemory
    private val editor = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { result -> receiveText(result.resultCode, result.data) }
    private val microphone = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted -> receiveMicrophonePermission(granted) }
    private val updateUi = Runnable {
        queued.set(false)
        if (visible) {
            val projection = latest.get()
            replyMetrics.remember(projection.providerRequestId, projection.firstTextMs)
            state = projection
            if (resumed && state.contextReady && client.snapshot().foreground) startupTiming?.finish(Stage.FIRST_PROJECTION)
            if (resumed && client.snapshot().foreground) {
                consumeVoiceLaunch()
                if (!startupSyncDeferred && state.contextReady) history.select(state)
            }
        }
    }
    private var startupSyncDeferred = false
    private val deferredStartupSync = Runnable { flushDeferredStartupSync() }
    private val updateLink = object : Runnable {
        override fun run() {
            if (!visible || !resumed) return
            refreshLinkStatus()
            handler.postDelayed(this, 3000)
        }
    }

    private fun startBackgroundSync() {
        if (SyntheticAskBenchmark.active(client) || TypedAskBenchmark.active(client)) return
        if (client is AssistantEngine) {
            (client as AssistantEngine).prewarm()
            AssistantReplication.reconcileNow(applicationContext)
        }
        handler.removeCallbacks(updateLink)
        updateLink.run()
    }

    private fun flushDeferredStartupSync() {
        if (!startupSyncDeferred) return
        startupSyncDeferred = false
        handler.removeCallbacks(deferredStartupSync)
        if (!visible || !resumed) return
        startBackgroundSync()
        val current = client.snapshot()
        if (current.foreground && current.contextReady) history.select(current)
    }

    private fun deferStartupSync() {
        startupSyncDeferred = true
        handler.removeCallbacks(updateLink)
        handler.removeCallbacks(deferredStartupSync)
        handler.postDelayed(deferredStartupSync, DEFERRED_STARTUP_SYNC_DELAY_MS)
    }

    protected open fun assistantClient(): AssistantClient = SyntheticAskBenchmark.client(this)
        ?: TypedAskBenchmark.client(this)
        ?: AssistantRuntime.get(applicationContext, startupTiming)
    protected open fun assistantVoice(client: AssistantClient): AssistantVoiceController = SyntheticAskBenchmark.voice(this, client)
        ?: TypedAskBenchmark.voice(client)
        ?: WearAssistantVoice(this, client as AssistantEngine)
    internal fun typedBenchmarkProjection(): AssistantHistoryState = historyState
    internal fun typedBenchmarkProvisional(): String = state.provisional
    internal fun typedBenchmarkDraft(): String = state.draft
    internal fun typedBenchmarkPrimary(): Pair<String, Boolean> {
        val presented = presentedVoiceState()
        return assistantPrimaryControl(state, presented, assistantRepeatsAfterReply(presented, voicePreferences))
            .let { it.action.name to it.enabled }
    }
    internal open fun assistantVoiceSettings(): WearVoiceSettings = WearVoiceSettings.production(applicationContext)
    private fun admitLaunch(entry: Boolean?) {
        pendingVoiceLaunch = null
        if (entry == null || voice.snapshot().active || permissionAction != null) return
        entryMode = if (entry) AssistantVoiceEntryMode.TALK else AssistantVoiceEntryMode.ASK
        freshEntryBeforeSettingsRead = !WearVoiceIntentOrder.seeded
        voiceEntryOrdinal = WearVoiceIntentOrder.next()
        pendingVoiceLaunch = entry
    }

    private fun beginStartup(stage: Stage) {
        startupTiming?.finish(Stage.REPLACED)
        startupTiming = (SyntheticAskBenchmark.timing(applicationContext, Operation.ACTIVITY)
            ?: AssistantTiming.begin(applicationContext, Operation.ACTIVITY)).also { it.mark(stage) }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        beginStartup(Stage.ACTIVITY_CREATED)
        val launchEntry = if (savedInstanceState == null) voiceEntry(intent) else null
        entryMode = savedInstanceState?.getString("assistant.voice.entry")?.let { runCatching { AssistantVoiceEntryMode.valueOf(it) }.getOrNull() }
        if (entryMode != null) {
            voiceEntryOrdinal = if (savedInstanceState?.containsKey("assistant.voice.entry.ordinal") == true) {
                WearVoiceIntentOrder.restore(savedInstanceState.getLong("assistant.voice.entry.ordinal"))
            } else WearVoiceIntentOrder.next()
        }
        client = assistantClient()
        replyMetrics = AssistantReplyMetrics(SyntheticAskBenchmark.context(client) ?: TypedAskBenchmark.context(client) ?: applicationContext)
        if (client is AssistantEngine && !SyntheticAskBenchmark.active(client) && !TypedAskBenchmark.active(client)) AssistantRuntime.configureCaptureResourceAdmission(true)
        voice = assistantVoice(client)
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
        editorDraftMemory = ViewModelProvider(this)[AssistantEditorDraftMemory::class.java]
        editorDraftMemory.restore(savedInstanceState, state)
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
                         ::recheck, history::older, history::retry, ::setConversationMode,
                           ::setWebEnabled, ::setSpeechNetworkFallback, ::setFastMode, ::setSpeechLanguage, ::setSpeechTtsProvider,
                              ::setSpeechTtsVoice, ::resetSpeechSelection, prepareSpeechOptions = ::prepareSpeechOptions, openSpeechStore = ::openSpeechStore,
                              replyLayout = { (client as? AssistantEngine)?.markReplyLayout(it) },
                              replyDrawSubmitted = { (client as? AssistantEngine)?.markReplyDrawSubmitted(it) },
                              loadConversations = { expectedRevision, offset, settled ->
                                  client.conversationPage(expectedRevision, offset) { page, projection, errorCode ->
                                      handler.post { settled(page, projection, errorCode) }
                                  }
                              },
                              loadBranches = { expectedRevision, conversation, selectedHead, offset, settled ->
                                  client.headPage(expectedRevision, conversation, selectedHead, offset) { page, projection, errorCode ->
                                      handler.post { settled(page, projection, errorCode) }
                                  }
                              }), pendingPrompts, speechSelectionError, speechSelectionPending,
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
            val deliver = Runnable {
                if (visible && observerEpoch == token) {
                    screenAwake.voiceProgress(projection, awakeToken, occurredAt)
                    voiceState = projection
                    projectPrompts()
                    if (pendingVoiceLaunch == null && startupVoiceSettled(projection)) flushDeferredStartupSync()
                }
            }
            if (Looper.myLooper() == Looper.getMainLooper()) deliver.run() else handler.post(deliver)
        }
    }

    override fun onResume() {
        super.onResume()
        if (startupTiming?.isFinished == true) beginStartup(Stage.RESUMED) else startupTiming?.mark(Stage.RESUMED)
        resumed = true
        screenAwake.resume(client.snapshot(), voice.snapshot())
        client.resume()
        voice.foreground(true)
        if (voiceSettingsReady) voice.tts(voicePreferences.readRepliesAloud)
        val launchedVoice = consumeVoiceLaunch()
        if (pendingVoiceLaunch != null || (launchedVoice && !startupVoiceSettled(voiceState))) {
            deferStartupSync()
        } else {
            startupSyncDeferred = false
            handler.removeCallbacks(deferredStartupSync)
            startBackgroundSync()
        }
        val current = client.snapshot()
        if (!startupSyncDeferred && current.foreground && current.contextReady) history.select(current)
    }

    private fun startupVoiceSettled(projection: AssistantVoiceState): Boolean =
        projection.dictation.readyForSpeech || projection.error != null || projection.loop.error != null ||
            projection.dictation.phase in setOf(
                dev.gitblame.voxgolem.assistant.recognition.DictationPhase.FINAL,
                dev.gitblame.voxgolem.assistant.recognition.DictationPhase.ERROR)

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        SyntheticAskBenchmark.requireAttached(client)
        beginStartup(Stage.REQUESTED)
        if (visible && resumed && client.snapshot().let { it.foreground && it.contextReady }) startupTiming?.finish(Stage.READY_REUSED)
        setIntent(intent)
        admitLaunch(voiceEntry(intent))
        val launchedVoice = consumeVoiceLaunch()
        if (visible && resumed && (pendingVoiceLaunch != null || (launchedVoice && !startupVoiceSettled(voiceState)))) deferStartupSync()
    }

    private fun consumeVoiceLaunch(): Boolean {
        val conversation = pendingVoiceLaunch ?: return false
        if (!visible || !resumed || !voiceSettingsReady) return false
        val current = client.snapshot()
        // Preference loading and context hydration may keep a launch pending;
        // all other admission fences retire it.
        if (current.busy || voice.snapshot().active || input.pending || permissionAction != null || isFinishing || isDestroyed) {
            pendingVoiceLaunch = null
            return false
        }
        if (!current.contextReady) return false
        pendingVoiceLaunch = null
        requestVoice(conversation)
        return true
    }

    override fun onPause() {
        voiceFlowTiming?.finish(Stage.CANCELLED)
        startupTiming?.finish(Stage.CANCELLED)
        retireSpeechSelection()
        pendingVoiceLaunch = null
        startupSyncDeferred = false
        handler.removeCallbacks(deferredStartupSync)
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
        startupSyncDeferred = false
        handler.removeCallbacks(deferredStartupSync)
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
        editorDraftMemory.save(outState)
        // A fresh, unadmitted launch has no durable-comparable rank until the first
        // read. Do not serialize its provisional ordinal as an acknowledged override.
        // Ranked restored/explicit entries retain the existing restoration behavior.
        if (!freshEntryBeforeSettingsRead) entryMode?.let {
            outState.putString("assistant.voice.entry", it.name)
            outState.putLong("assistant.voice.entry.ordinal", voiceEntryOrdinal)
        }
        super.onSaveInstanceState(outState)
    }

    internal fun editDraft() {
        if (!visible || !resumed || isFinishing || isDestroyed || client.snapshot().busy || !input.begin()) return
        val currentVoice = voice.snapshot()
        val prefill = editorDraftMemory.prefill(client.snapshot()) ?: currentVoice.dictation.takeIf {
            it.phase in setOf(dev.gitblame.voxgolem.assistant.recognition.DictationPhase.LISTENING,
                dev.gitblame.voxgolem.assistant.recognition.DictationPhase.FINISHING) && it.draft.isNotBlank()
        }?.draft ?: client.snapshot().draft
        pendingVoiceLaunch = null; permissionAction = null
        voice.stop()
        if (!SyntheticAskBenchmark.active(client) && !TypedAskBenchmark.active(client)) (client as? AssistantEngine)?.prewarm()
        editor.launch(Intent(this, AssistantTextEntryActivity::class.java).putExtra(AssistantTextInput.DRAFT, prefill))
    }

    private fun projectPrompts() { pendingPrompts = promptPresentation.project(promptSource, voiceState, historyState) }

    internal fun receiveText(resultCode: Int, intent: Intent?) {
        val text = input.accept(resultCode, intent) ?: return
        val memory = editorDraftMemory
        val ticket = memory.begin(text, client.snapshot())
        if (!AssistantTextInput.accepts(text)) return
        saveEditorDraft(text) { settled -> memory.settle(ticket, settled) }
    }

    internal open fun saveEditorDraft(text: String, onSettled: (AssistantState) -> Unit) {
        val engine = client as? AssistantEngine
        if (engine != null) engine.setDraft(text, onSettled)
        else { client.setDraft(text); onSettled(client.snapshot()) }
    }

    internal fun sendManually() {
        if (!visible || !resumed || isFinishing || isDestroyed || client.snapshot().busy || voiceAdmissionPending) return
        if (client.snapshot().model !in AssistantModels.aliases) return
        if (!TypedAskBenchmark.permitUiSend(client)) return
        permissionAction = null
        if (!voice.send()) client.send()
    }

    internal fun requestVoice(conversation: Boolean) {
        if (!voiceSettingsReady) {
            if (!voiceSettingsLoadFinished && visible && resumed && !isFinishing && !isDestroyed)
                pendingVoiceLaunch = conversation
            return
        }
        val mode = if (conversation) AssistantVoiceEntryMode.TALK else AssistantVoiceEntryMode.ASK
        if (!AssistantVoiceLaunchGate.shouldStart(
                visible = visible,
                resumed = resumed,
                state = client.snapshot(),
                pendingMode = mode,
                voiceActive = voice.snapshot().active,
                inputPending = input.pending,
                permissionPending = permissionAction != null,
                finishingOrDestroyed = isFinishing || isDestroyed,
            )) return
        entryMode = mode
        freshEntryBeforeSettingsRead = !WearVoiceIntentOrder.seeded
        voiceEntryOrdinal = WearVoiceIntentOrder.next()
        pendingVoiceLaunch = null
        permissionError = null
        voiceFlowTiming?.finish(Stage.REPLACED)
        val timing = (startupTiming?.fork(Operation.VOICE_FLOW) ?: AssistantTiming.begin(applicationContext, Operation.VOICE_FLOW)).also {
            it.mark(Stage.REQUESTED)
            it.metadata(Metadata(entry = if (conversation) "talk" else "ask"))
        }
        voiceFlowTiming = timing
        if (!voice.snapshot().admissionReady) {
            if (conversation) voice.conversation(client.snapshot().draft, timing) else voice.ask(client.snapshot().draft, timing)
            return
        }
        val current = client.snapshot()
        permissionAction = VoiceAdmission(conversation, current.draft, current.conversation, current.selectedHead, timing)
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) receiveMicrophonePermission(true)
        else microphone.launch(Manifest.permission.RECORD_AUDIO)
    }

    internal fun receiveMicrophonePermission(granted: Boolean) {
        val admission = permissionAction ?: return
        permissionAction = null
        val mode = if (admission.conversationMode) AssistantVoiceEntryMode.TALK else AssistantVoiceEntryMode.ASK
        if (!AssistantVoiceLaunchGate.shouldStart(
                visible = visible,
                resumed = resumed,
                state = client.snapshot(),
                pendingMode = mode,
                voiceActive = voice.snapshot().active,
                finishingOrDestroyed = isFinishing || isDestroyed,
            ) ||
            client.snapshot().conversation != admission.conversation || client.snapshot().selectedHead != admission.head || client.snapshot().draft != admission.draft) return
        if (granted) {
            if (admission.conversationMode) voice.conversation(admission.draft, admission.timing) else voice.ask(admission.draft, admission.timing)
        } else { admission.timing.finish(Stage.FAILED); permissionError = "microphone_permission_required" }
    }

    internal fun stopAll() { voiceFlowTiming?.finish(Stage.CANCELLED); pendingVoiceLaunch = null; permissionAction = null; voice.stop(); client.stop() }
    internal fun openSpeechStore() {
        if (!voiceState.missingSpeechServices && assistantNeedsTtsDataInstall(presentedVoiceState())) {
            dev.gitblame.voxgolem.assistant.speech.SpeechServiceRequirement.openTtsDataInstall(this)
            return
        }
        dev.gitblame.voxgolem.assistant.speech.SpeechServiceRequirement.openPlayStore(this)
    }
    private fun canChangeVoiceContext() = visible && resumed && !voiceAdmissionPending && !voice.snapshot().active && !client.snapshot().busy
    internal fun newConversation() { if (canChangeVoiceContext()) client.newConversation() }
    internal fun selectConversation(id: String) { if (canChangeVoiceContext()) client.selectConversation(id) }
    internal fun selectHead(id: String) { if (canChangeVoiceContext()) client.selectHead(id) }
    internal fun setWebEnabled(enabled: Boolean) { client.setWebEnabled(enabled) }
    internal fun setFastMode(enabled: Boolean) { client.setFastMode(enabled) }
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
        if (TypedAskBenchmark.active(client)) {
            voicePreferences = WearVoicePreferences(readRepliesAloud = false, conversationMode = false)
            voiceSettingsReady = true
            voiceSettingsLoadFinished = true
            return
        }
        if (SyntheticAskBenchmark.active(client)) {
            voicePreferences = WearVoicePreferences(readRepliesAloud = true, conversationMode = false)
            voiceSettingsReady = true
            voiceSettingsLoadFinished = true
            return
        }
        settleVoiceSettings {
            voiceSettings = assistantVoiceSettings()
            voiceSettings.read()
        }
    }
    internal fun setReadRepliesAloud(enabled: Boolean) {
        if (!voiceSettingsReady) { permissionError = "settings_not_saved"; return }
        settleVoiceSettings {
            voiceSettings.setReadRepliesAloud(enabled)
            voiceSettings.read()
        }
    }

    internal fun setConversationMode(enabled: Boolean) {
        if (!voiceSettingsReady) { permissionError = "settings_not_saved"; return }
        val intentOrdinal = WearVoiceIntentOrder.next()
        settleVoiceSettings {
            voiceSettings.setConversationMode(enabled, intentOrdinal)
            voiceSettings.read()
        }
    }
    private fun settleVoiceSettings(operation: () -> WearVoicePreferences) {
        val request = voiceSettingsRequest + 1
        try {
            WearVoiceSettingsIo.executor.execute {
                val result = runCatching(operation)
                result.onSuccess { acknowledgedVoicePreferences = it }
                val acknowledged = acknowledgedVoicePreferences
                val successfulModeOrdinal = if (::voiceSettings.isInitialized) voiceSettings.successfulModeOrdinal else 0L
                val saveFailed = if (::voiceSettings.isInitialized) voiceSettings.saveFailed else false
                handler.post {
                    if (voiceSettingsClosed || request != voiceSettingsRequest) return@post
                    voiceSettingsLoadFinished = true
                    voiceSettingsReady = acknowledged != null
                    if (!voiceSettingsReady) pendingVoiceLaunch = null
                    if (voiceSettingsReady && freshEntryBeforeSettingsRead) {
                        // A fresh launch predates this process's disk read, not the saved
                        // mode intent from the previous process. Restored entries never rebase.
                        if (voiceEntryOrdinal <= successfulModeOrdinal) voiceEntryOrdinal = WearVoiceIntentOrder.next()
                        freshEntryBeforeSettingsRead = false
                    }
                    // A mode write's successful effect survives snapshot coalescing, but
                    // must not retire an Ask/Talk choice made after that write was requested.
                    if (successfulModeOrdinal > voiceEntryOrdinal) entryMode = null
                    acknowledged?.let {
                        voicePreferences = it
                        if (visible && resumed) voice.tts(it.readRepliesAloud)
                    }
                    result.onSuccess {
                        if (!voiceSettingsRejected) permissionError = if (saveFailed) "settings_not_saved" else null
                    }.onFailure { permissionError = "settings_not_saved" }
                    if (voiceSettingsReady && visible && resumed) consumeVoiceLaunch()
                }
            }
            voiceSettingsRequest = request
            voiceSettingsRejected = false
        } catch (_: java.util.concurrent.RejectedExecutionException) {
            // Keep the last accepted token valid: its acknowledgement must still project.
            voiceSettingsRejected = true
            permissionError = "settings_not_saved"
            if (!voiceSettingsReady && voiceSettingsRequest == 0L) {
                voiceSettingsLoadFinished = true
                pendingVoiceLaunch = null
            }
        }
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
        voiceSettingsClosed = true
        voiceFlowTiming?.finish(Stage.CANCELLED)
        screenAwake.suspend()
        startupSyncDeferred = false
        handler.removeCallbacks(deferredStartupSync)
        retireSpeechSelection(); voice.close(); handler.removeCallbacks(updateUi); handler.removeCallbacks(updateLink)
        super.onDestroy()
    }

    protected open fun readLinkStatus(result: (PeerPairingState) -> Unit) {
        if (client is AssistantEngine && !SyntheticAskBenchmark.active(client) && !TypedAskBenchmark.active(client)) AssistantReplication.pairingState(applicationContext, result)
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

    internal fun recheck() {
        client.refresh()
        if (SyntheticAskBenchmark.active(client) || TypedAskBenchmark.active(client)) return
        if (!voiceSettingsReady && voiceSettingsLoadFinished) readVoicePreferences()
        if (client !is AssistantEngine) return
        refreshLinkStatus()
        AssistantReplication.reconcileNow(applicationContext)
    }

    companion object {
        internal const val DEFERRED_STARTUP_SYNC_DELAY_MS = 500L
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

internal fun assistantIsInstallableVoiceError(error: String, voice: AssistantVoiceState = AssistantVoiceState()): Boolean =
    voice.missingSpeechServices ||
        error in setOf("local_language_unavailable", "recognition_error_12", "recognition_error_13") ||
        error == "local_voice_unavailable" ||
        error.contains("voice data", ignoreCase = true) ||
        error.contains("not installed", ignoreCase = true)

internal fun assistantNeedsTtsDataInstall(voice: AssistantVoiceState): Boolean {
    val messages = assistantVisibleVoiceErrors(voice) + listOfNotNull(voice.speechRequirementMessage)
    return messages.any { error ->
        error == "local_voice_unavailable" ||
            error.contains("voice data", ignoreCase = true) ||
            error.contains("not installed", ignoreCase = true)
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
    "unsupported_model" -> "Selected model is not available on the watch. Choose Nano, Agy or Custom GPT in Settings. Draft retained."
    "prompt_too_large" -> "Draft exceeds 32 KiB. Previous draft retained."
    "saved_local_state_failed" -> "Answer saved, but local selection/draft update failed."
    "aicore_not_available", "aicore_unavailable", "aicore_needs_update" -> "Gemini Nano is unavailable on the paired phone. Draft retained."
    "aicore_busy", "aicore_downloading" -> "Gemini Nano is busy or downloading on the phone. Retry shortly."
    else -> "Assistant error ($code). Draft retained; no automatic retry."
}

internal fun assistantVoiceErrorText(code: String): String = when (code) {
    "settings_not_saved" -> "Settings could not be saved. Recheck your choice before continuing."
    "capture_admission_unavailable" -> "Microphone sharing is not connected. Use keyboard."
    "on_device_recognition_unavailable", "local_language_unavailable", "recognition_error_12", "recognition_error_13" ->
        "Install the matching offline language in Speech Recognition & Synthesis by Google, then retry. Typing remains available."
    "recognition_busy" -> "Audio is busy. Note is not preempted; use keyboard."
    "recognition_error_7", "no_speech" -> "No speech recognized. Tap Mic to try again."
    "local_voice_unavailable" -> "Install a matching offline Google TTS voice, then retry."
    "microphone_permission_required" -> "Microphone permission required. Tap Mic again deliberately."
    else -> "Speech stopped ($code). Typing remains available; resume deliberately."
}
