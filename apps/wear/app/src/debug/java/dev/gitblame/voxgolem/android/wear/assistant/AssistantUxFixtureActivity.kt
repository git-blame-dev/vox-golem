package dev.gitblame.voxgolem.android.wear.assistant

import android.os.Build
import android.os.Bundle
import android.view.WindowManager
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.Density
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.recognition.DictationState
import dev.gitblame.voxgolem.assistant.recognition.RecognitionRoute
import dev.gitblame.voxgolem.assistant.replication.PeerPairingReason
import dev.gitblame.voxgolem.assistant.replication.PeerPairingState
import dev.gitblame.voxgolem.assistant.runtime.AssistantModels
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.speech.SpeechState
import dev.gitblame.voxgolem.assistant.speech.SpeechPhase
import dev.gitblame.voxgolem.assistant.speech.ConversationLoopState
import dev.gitblame.voxgolem.assistant.speech.LoopPhase
import dev.gitblame.voxgolem.assistant.speech.TtsRoute
import dev.gitblame.voxgolem.assistant.speech.SpeechOptions
import dev.gitblame.voxgolem.assistant.speech.SpeechVoiceOption
import dev.gitblame.voxgolem.assistant.runtime.speechAdmission
import org.json.JSONArray
import org.json.JSONObject

/** Debug-only screen host: no AssistantRuntime, store, credentials, recognizer or TTS engine. */
class AssistantUxFixtureActivity : ComponentActivity() {
    private lateinit var memory: AssistantUxFixtureMemory

    override fun onCreate(savedInstanceState: Bundle?) {
        check(packageName == FIXTURE_PACKAGE) { "Synthetic assistant UX fixture requires isolated .uxfixtures identity" }
        check(Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish") {
            "Synthetic assistant UX fixture is emulator-only"
        }
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        memory = ViewModelProvider(this)[AssistantUxFixtureMemory::class.java]
        if (!memory.initialized) {
            memory.settings = WearVoiceSettings(getSharedPreferences(memory.preferenceName, MODE_PRIVATE))
            memory.preferences = memory.settings.read()
            if (intent.hasExtra(CONVERSATION_CHOICE)) memory.setConversationMode(intent.getBooleanExtra(CONVERSATION_CHOICE, true))
            memory.initialized = true
        }
        memory.preferences = memory.settings.read()
        setContent {
            val density = LocalDensity.current
            val fontScale = intent.getFloatExtra(FONT_SCALE, density.fontScale)
            CompositionLocalProvider(LocalDensity provides Density(density.density, fontScale)) {
                AssistantTheme {
                    AssistantScreen(memory.state, memory.voice, memory.history,
                        PeerPairingState(PeerPairingReason.UNAVAILABLE), memory.preferences, memory.actions)
                }
            }
        }
    }

    override fun onResume() {
        super.onResume()
        memory.state = memory.state.copy(foreground = true)
    }

    override fun onPause() {
        memory.background()
        memory.state = memory.state.copy(foreground = false)
        super.onPause()
    }

    fun snapshot() = AssistantUxFixtureSnapshot(memory.state, memory.voice.dictation.phase,
        memory.dictates, memory.conversations, memory.finishes, memory.sends, memory.stops, memory.edits, memory.speaks,
        memory.preferences.readRepliesAloud, memory.preferences.conversationMode,
        getSharedPreferences(memory.preferenceName, MODE_PRIVATE).all.filterValues { it is Boolean },
        memory.voice.entryMode, memory.voice.loop.phase, memory.voice.speech.phase, memory.micStarts,
        memory.voice.launchConfirmationRequired, memory.olderRequests)

    fun setDraft(text: String) { memory.state = memory.state.copy(draft = text, error = null, interrupted = false) }

    fun withholdNextResponseText() { memory.nextProvisional = "" }
    fun publishProvisional(text: String) {
        check(memory.state.busy)
        memory.state = memory.state.copy(provisional = text)
    }

    fun publishPartial(text: String, utterance: Long = memory.voice.dictation.utterance) {
        if (!memory.ownsInput(utterance) || memory.voice.dictation.phase != DictationPhase.LISTENING) return
        memory.voice = memory.voice.copy(dictation = memory.voice.dictation.copy(draft = text))
        memory.state = memory.state.copy(draft = text)
    }

    fun beginEmptyFollowup() {
        memory.beginFollowup()
    }

    fun publishListening(utterance: Long = memory.voice.dictation.utterance) {
        if (!memory.ownsInput(utterance) || memory.voice.dictation.phase != DictationPhase.CHECKING) return
        memory.voice = memory.voice.copy(dictation = memory.voice.dictation.copy(phase = DictationPhase.LISTENING, readyForSpeech = true))
    }

    fun publishQuietNoMatchRetirement() {
        check(memory.voice.listening && memory.voice.dictation.draft.isEmpty())
        memory.voice = memory.voice.copy(dictation = memory.voice.dictation.copy(phase = DictationPhase.IDLE, error = null, readyForSpeech = false))
        memory.retireSession()
    }

    fun prepareFreshConversation() {
        check(!memory.state.busy)
        memory.state = memory.state.copy(conversation = null, selectedHead = null, draft = "")
        memory.history = AssistantHistoryState()
    }

    fun seedSpeechRoutes() {
        memory.voice = memory.voice.copy(dictation = memory.voice.dictation.copy(route = syntheticRecognitionRoute()),
            speech = memory.voice.speech.copy(route = TtsRoute("com.google.synthetic.tts", "en-GB", "en-US", false,
                providerIsPreference = true)))
    }

    fun seedSpeechChoices() {
        memory.voice = memory.voice.copy(localOnlyRecognitionAvailable = true, speechOptions = SpeechOptions(
            ttsProviders = listOf("", "synthetic.engine.one", "synthetic.engine.two"),
            ttsVoices = listOf(SpeechVoiceOption("synthetic.engine.one", "synthetic-local-voice", "en-AU", false),
                SpeechVoiceOption("synthetic.engine.one", "synthetic-network-voice", "en-AU", true)),
            recognitionInstalledLanguages = listOf("en-AU"), recognitionSupportedLanguages = listOf("en-AU"),
            localOnlyRecognitionAvailable = true))
    }

    fun publishFinishing(utterance: Long = memory.voice.dictation.utterance) {
        if (!memory.ownsInput(utterance) || memory.voice.dictation.phase != DictationPhase.LISTENING) return
        memory.voice = memory.voice.copy(dictation = memory.voice.dictation.copy(phase = DictationPhase.FINISHING, readyForSpeech = false))
    }

    /** SDK-like callbacks carry their original input owner; duplicates/retired callbacks are ignored. */
    fun publishFinal(text: String = FINAL_DRAFT, utterance: Long = memory.voice.dictation.utterance) = memory.final(text, utterance)
    fun inputOwner(): Long = memory.voice.dictation.utterance
    fun completePlayback(request: Int = memory.sends) = memory.completePlayback(request)
    fun beginManualDictation() = memory.startManualDictation()
    fun beginVoiceEntry(mode: AssistantVoiceEntryMode) {
        if (mode == AssistantVoiceEntryMode.ASK) memory.actions.dictate() else memory.actions.conversationMode()
    }
    fun selectTransientEntry(mode: AssistantVoiceEntryMode) { memory.voice = memory.voice.copy(entryMode = mode) }
    fun requireLaunchConfirmation(mode: AssistantVoiceEntryMode) {
        check(!memory.state.busy && !memory.voice.active)
        memory.voice = memory.voice.copy(entryMode = mode, launchConfirmationRequired = true)
    }
    fun setRecognitionAvailable(available: Boolean) { memory.voice = memory.voice.copy(recognitionServiceAvailable = available) }
    fun setDedicatedRecognitionAvailable(available: Boolean) { memory.voice = memory.voice.copy(localOnlyRecognitionAvailable = available) }

    fun completeRequest(answer: String = SAVED_ANSWER, deferHistory: Boolean = false, request: Int = memory.sends) {
        if (!memory.ownsRequest(request)) return
        val id = syntheticId(100 + memory.sends)
        memory.completedTurn = AssistantTurn(id, memory.state.draft, answer, false)
        memory.state = memory.state.copy(busy = false, provisional = "", draft = "", selectedHead = id,
            clientRequestToken = null, providerRequestId = null, error = null)
        if (!deferHistory) publishCompletedHistory()
        memory.replyCompleted()
    }

    fun publishCompletedHistory() {
        val turn = requireNotNull(memory.completedTurn)
        memory.history = memory.history.copy(conversation = memory.state.conversation, head = turn.id,
            turns = memory.history.turns + turn)
        memory.completedTurn = null
    }

    fun failRequest() {
        check(memory.state.busy)
        memory.state = memory.state.copy(busy = false, provisional = "", error = "transport",
            clientRequestToken = null, providerRequestId = null)
        memory.retireSession()
    }

    fun seedHistory(count: Int = 16) {
        check(!memory.state.busy)
        val turns = (1..count).map { index ->
            AssistantTurn(syntheticId(200 + index), "Synthetic prompt $index",
                "Synthetic answer $index. This fixture has no provider or private history.", false)
        }
        memory.history = AssistantHistoryState(conversation = memory.state.conversation,
            head = turns.lastOrNull()?.id, turns = turns)
        memory.state = memory.state.copy(selectedHead = memory.history.head)
    }

    fun seedLongAnswer() {
        check(!memory.state.busy)
        val answer = (1..40).joinToString("\n\n") { "Synthetic long paragraph $it for reading clearance and scrolling." } +
            "\nSynthetic long answer end."
        val turn = AssistantTurn(syntheticId(301), "Synthetic long answer question", answer, false)
        memory.history = AssistantHistoryState(conversation = memory.state.conversation, head = turn.id, turns = listOf(turn))
        memory.state = memory.state.copy(selectedHead = turn.id)
    }

    /** Synthetic history projection controls: no native paging, store or provider. */
    fun seedReadingAnchorHistory() {
        check(!memory.state.busy && !memory.voice.active)
        val turn = AssistantTurn(syntheticId(601), ANCHOR_PROMPT, ANCHOR_ANSWER, false)
        memory.history = AssistantHistoryState(conversation = memory.state.conversation, head = turn.id,
            turns = listOf(turn), olderBefore = "synthetic-older-cursor")
        memory.state = memory.state.copy(selectedHead = turn.id)
    }

    fun completeOlderHistory() {
        check(memory.history.loading && memory.olderRequests > 0)
        val older = AssistantTurn(syntheticId(600), "Synthetic preceding prompt", "Synthetic preceding answer", false)
        memory.history = memory.history.copy(turns = listOf(older) + memory.history.turns, loading = false)
    }

    fun beginHistoryRevalidation() {
        check(!memory.history.loading && !memory.state.busy && !memory.voice.active)
        memory.history = memory.history.copy(head = null, loading = true)
    }

    fun completeHistoryRevalidation() {
        check(memory.history.loading)
        memory.history = memory.history.copy(head = memory.state.selectedHead, loading = false)
    }

    companion object {
        const val FIXTURE_PACKAGE = "dev.gitblame.voxgolem.android.uxfixtures"
        const val FONT_SCALE = "synthetic-font-scale"
        const val FINAL_DRAFT = "Synthetic authoritative voice question."
        const val SAVED_ANSWER = "Synthetic completed reply."
        const val CONVERSATION_CHOICE = "synthetic-conversation-choice"
        val ANCHOR_PROMPT = (1..24).joinToString("\n") { "Synthetic anchor prompt line $it preserves its reading position." }
        val ANCHOR_ANSWER = (1..24).joinToString("\n") { "Synthetic anchor answer line $it preserves its reading position." }
    }
}

data class AssistantUxFixtureSnapshot(
    val state: AssistantState,
    val dictationPhase: DictationPhase,
    val dictates: Int,
    val conversations: Int,
    val finishes: Int,
    val sends: Int,
    val stops: Int,
    val edits: Int,
    val speaks: Int,
    val readRepliesAloud: Boolean,
    val conversationMode: Boolean,
    val savedChoices: Map<String, *>,
    val entryMode: AssistantVoiceEntryMode?,
    val loopPhase: LoopPhase,
    val speechPhase: SpeechPhase,
    val micStarts: Int,
    val launchConfirmationRequired: Boolean,
    val olderRequests: Int,
)

internal class AssistantUxFixtureMemory : ViewModel() {
    var state by mutableStateOf(AssistantState(viewJson = syntheticView(), conversation = syntheticId(1),
        model = AssistantModels.aliases.first()))
    var history by mutableStateOf(AssistantHistoryState(conversation = state.conversation))
    var voice by mutableStateOf(AssistantVoiceState(admissionReady = true, recognitionServiceAvailable = true,
        speech = SpeechState(enabled = true)))
    var preferences by mutableStateOf(WearVoicePreferences())
    val preferenceName = "synthetic-assistant-ux-${java.util.UUID.randomUUID()}"
    lateinit var settings: WearVoiceSettings
    var initialized = false
    var completedTurn: AssistantTurn? = null
    var dictates = 0
    var conversations = 0
    var finishes = 0
    var sends = 0
    var stops = 0
    var edits = 0
    var speaks = 0
    var nextProvisional = "Synthetic streaming reply"
    var micStarts = 0
    var olderRequests = 0
    private var sessionActive = false
    private var manualInput = false
    private var inputConversation: String? = null
    private var inputHead: String? = null
    private var inputDraft = ""
    private var ownedRequest: Int? = null
    private var acceptedUtterance: Long? = null

    val actions = AssistantActions(
        edit = { edits++ },
        send = ::send,
        stop = { stops++; cancel() },
        new = {
            history = AssistantHistoryState(conversation = state.conversation)
            state = state.copy(selectedHead = null, draft = "", error = null)
        },
        selectConversation = {},
        selectHead = {},
        selectModel = { state = state.copy(model = it) },
        dictate = { dictates++; startEntry(AssistantVoiceEntryMode.ASK) },
        conversationMode = { conversations++; startEntry(AssistantVoiceEntryMode.TALK) },
        endUtterance = {
            finishes++
            voice = voice.copy(dictation = voice.dictation.copy(phase = DictationPhase.FINISHING))
        },
        tts = {
            settings.setReadRepliesAloud(it)
            preferences = settings.read()
            voice = voice.copy(speech = voice.speech.copy(enabled = it))
        },
        speak = { speaks++ },
        recheck = {},
        older = {
            check(history.olderBefore != null && !history.loading)
            olderRequests++
            history = history.copy(loading = true)
        },
        retryHistory = {},
        setConversationMode = ::setConversationMode,
        selectSearchMode = { state = state.copy(searchMode = it) },
        setWebEnabled = { state = state.copy(webEnabled = it) },
        setSpeechNetworkFallback = { state = state.copy(speechNetworkFallback = it) },
        selectSpeechLanguage = { state = state.copy(speechLanguage = it) },
        selectSpeechEngine = {
            val candidate = assistantSpeechCandidate(state.speechAdmission().selection, AssistantSpeechField.ENGINE, it)
            state = state.copy(speechTtsProvider = candidate.ttsProvider, speechTtsVoice = candidate.ttsVoice)
        },
        selectSpeechVoice = { state = state.copy(speechTtsVoice = it) },
        resetSpeechSelection = { state = state.copy(speechLanguage = "", speechTtsProvider = "", speechTtsVoice = "") },
    )

    private fun startEntry(entry: AssistantVoiceEntryMode) {
        check(state.foreground && !state.busy && !voice.active)
        sessionActive = true
        manualInput = false
        voice = voice.copy(entryMode = entry, launchConfirmationRequired = false)
        startInput(state.draft)
    }

    fun startManualDictation() {
        check(state.foreground && !state.busy && !voice.active)
        sessionActive = false
        manualInput = true
        startInput(state.draft)
    }

    private fun startInput(draft: String) {
        inputConversation = state.conversation
        inputHead = state.selectedHead
        inputDraft = draft
        micStarts++
        voice = voice.copy(dictation = DictationState(phase = DictationPhase.CHECKING,
            draft = draft, utterance = voice.dictation.utterance + 1, route = syntheticRecognitionRoute()),
            loop = ConversationLoopState(if (sessionActive) LoopPhase.LISTENING else LoopPhase.IDLE, draft))
    }

    fun final(text: String, utterance: Long) {
        if (!ownsInput(utterance) || utterance == acceptedUtterance ||
            voice.dictation.phase !in setOf(DictationPhase.LISTENING, DictationPhase.FINISHING)) return
        if (state.conversation != inputConversation || state.selectedHead != inputHead) { cancel(); return }
        acceptedUtterance = utterance
        voice = voice.copy(dictation = voice.dictation.copy(phase = DictationPhase.FINAL, draft = text, readyForSpeech = false))
        state = state.copy(draft = text)
        if (sessionActive) {
            voice = voice.copy(loop = ConversationLoopState(LoopPhase.GENERATING, text))
            send()
        } else manualInput = false
    }

    fun ownsInput(utterance: Long) = state.foreground && (sessionActive || manualInput) &&
        utterance == voice.dictation.utterance

    fun ownsRequest(request: Int) = state.foreground && state.busy && ownedRequest == request

    fun replyCompleted() {
        if (!sessionActive) return
        if (preferences.readRepliesAloud) {
            voice = voice.copy(speech = voice.speech.copy(phase = SpeechPhase.SPEAKING),
                loop = voice.loop.copy(phase = LoopPhase.SPEAKING))
        } else settleReply()
    }

    fun completePlayback(request: Int) {
        if (!state.foreground || ownedRequest != request || !sessionActive || voice.speech.phase != SpeechPhase.SPEAKING) return
        voice = voice.copy(speech = voice.speech.copy(phase = SpeechPhase.IDLE))
        settleReply()
    }

    private fun settleReply() {
        ownedRequest = null
        if (voice.entryMode == AssistantVoiceEntryMode.TALK) {
            voice = voice.copy(loop = voice.loop.copy(phase = LoopPhase.BETWEEN_TURNS))
        } else retireSession()
    }

    /** Explicit fixture scheduler tick, only after durable reply and optional playback settlement. */
    fun beginFollowup() {
        if (!state.foreground || !sessionActive || voice.entryMode != AssistantVoiceEntryMode.TALK ||
            voice.loop.phase != LoopPhase.BETWEEN_TURNS) return
        startInput("")
    }

    fun retireSession() {
        sessionActive = false
        manualInput = false
        ownedRequest = null
        voice = voice.copy(dictation = voice.dictation.copy(phase = DictationPhase.IDLE, draft = "", readyForSpeech = false),
            loop = ConversationLoopState(), speech = voice.speech.copy(phase = SpeechPhase.IDLE))
    }

    fun background() { if (sessionActive || manualInput) cancel() }

    private fun cancel() {
        val retained = if (voice.dictation.phase in setOf(DictationPhase.LISTENING, DictationPhase.FINISHING))
            voice.dictation.draft.ifBlank { inputDraft } else state.draft
        retireSession()
        state = state.copy(busy = false, provisional = "", draft = retained, interrupted = true, error = "interrupted",
            clientRequestToken = null, providerRequestId = null)
    }

    fun setConversationMode(enabled: Boolean) {
        settings.setConversationMode(enabled)
        preferences = settings.read()
    }

    fun send() {
        check(state.foreground && !state.busy && state.draft.isNotBlank())
        sends++
        ownedRequest = sends
        state = state.copy(conversation = state.conversation ?: syntheticId(1), busy = true,
            provisional = nextProvisional, error = null, interrupted = false,
            clientRequestToken = "synthetic-token-$sends", providerRequestId = "synthetic-request-$sends")
        nextProvisional = "Synthetic streaming reply"
    }
}

private fun syntheticId(sequence: Int) = JSONObject().put("replica", "synthetic-assistant-ux")
    .put("sequence", sequence.toString()).toString()

private fun syntheticRecognitionRoute() = RecognitionRoute("com.google.synthetic/Recognizer", "en-GB", "en-US", "en-US")

private fun syntheticView() = JSONObject().put("revision", "synthetic-ux-1").put("records", JSONArray())
    .put("pending", JSONArray()).put("conversations", JSONArray().put(JSONObject()
        .put("id", JSONObject(syntheticId(1))).put("title", "Synthetic UX chat").put("deleted", false))).toString()
