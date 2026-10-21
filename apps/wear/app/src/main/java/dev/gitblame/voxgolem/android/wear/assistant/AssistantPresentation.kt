package dev.gitblame.voxgolem.android.wear.assistant

import androidx.activity.compose.BackHandler
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.gestures.awaitEachGesture
import androidx.compose.foundation.gestures.awaitFirstDown
import androidx.compose.foundation.gestures.awaitHorizontalTouchSlopOrCancellation
import androidx.compose.foundation.gestures.horizontalDrag
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.snapshots.Snapshot
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.draw.drawWithContent
import androidx.compose.ui.platform.LocalLayoutDirection
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.wear.compose.material3.*
import androidx.wear.compose.foundation.BasicSwipeToDismissBox
import androidx.wear.compose.foundation.LocalScreenIsActive
import dev.gitblame.voxgolem.android.wear.listRotary
import androidx.wear.compose.foundation.rememberSwipeToDismissBoxState as rememberWearDismissState
import dev.gitblame.voxgolem.assistant.replication.PeerPairingState
import dev.gitblame.voxgolem.assistant.runtime.AssistantModels
import dev.gitblame.voxgolem.assistant.runtime.AssistantRuntime
import dev.gitblame.voxgolem.assistant.runtime.OptionalProviderReadiness
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.runtime.speechAdmission
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import dev.gitblame.voxgolem.assistant.speech.SpeechPhase
import dev.gitblame.voxgolem.assistant.speech.description
import dev.gitblame.voxgolem.android.wear.wearTextStyle
import dev.gitblame.voxgolem.android.wear.WearAppTheme
import dev.gitblame.voxgolem.android.wear.CompactEdgeButton
import dev.gitblame.voxgolem.android.wear.CompactEdgeButtonDefaults
import dev.gitblame.voxgolem.android.wear.formatVisibleDuration
import org.json.JSONObject
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.delay

@Composable
internal fun AssistantTheme(content: @Composable () -> Unit) {
    WearAppTheme(content)
}

@Composable
private fun WearSurface(modifier: Modifier = Modifier, content: @Composable BoxScope.() -> Unit) {
    CompositionLocalProvider(LocalContentColor provides MaterialTheme.colorScheme.onBackground) {
        ProvideTextStyle(wearTextStyle(MaterialTheme.typography.bodyMedium)) {
            Box(modifier.fillMaxSize().background(MaterialTheme.colorScheme.background), content = content)
        }
    }
}

internal data class AssistantActions(
    val edit: () -> Unit, val send: () -> Unit, val stop: () -> Unit,
    val new: () -> Unit, val selectConversation: (String) -> Unit, val selectHead: (String) -> Unit,
    val selectModel: (String) -> Unit, val dictate: () -> Unit, val conversationMode: () -> Unit,
    val endUtterance: () -> Unit, val tts: (Boolean) -> Unit, val speak: (String) -> Unit,
    val recheck: () -> Unit, val older: () -> Unit, val retryHistory: () -> Unit,
    val setConversationMode: (Boolean) -> Unit = {},
    val setWebEnabled: (Boolean) -> Unit,
    val setSpeechNetworkFallback: (Boolean) -> Unit,
    val selectSpeechLanguage: (String) -> Unit,
    val selectSpeechEngine: (String) -> Unit,
    val selectSpeechVoice: (String) -> Unit,
    val resetSpeechSelection: () -> Unit,
    val prepareSpeechOptions: () -> Unit = {},
    val openSpeechStore: () -> Unit = {},
    val replyLayout: (String?) -> Unit = {},
    val replyDrawSubmitted: (String?) -> Unit = {},
    val loadConversations: (String, Int, (String?, AssistantState, String?) -> Unit) -> Unit = { _, _, _ -> },
    val loadBranches: (String, String, String?, Int, (String?, AssistantState, String?) -> Unit) -> Unit = { _, _, _, _, _ -> },
)

private enum class AssistantPage { CHAT, CHATS, SETTINGS, MODEL, SPEECH_LANGUAGE, SPEECH_ENGINE, SPEECH_VOICE, MISSING_SPEECH }

private fun AssistantPage.backPage() = when (this) {
    AssistantPage.SPEECH_LANGUAGE, AssistantPage.SPEECH_ENGINE, AssistantPage.SPEECH_VOICE -> AssistantPage.SETTINGS
    else -> AssistantPage.CHAT
}

internal fun assistantCompletedReplyLabel(turn: AssistantTurn, replyMetrics: (String?) -> Long? = { null }): String =
    assistantReplyLabel(turn.requestKey?.let(replyMetrics), refusal = turn.refusal)

internal fun assistantStreamingReplyLabel(state: AssistantState, replyMetrics: (String?) -> Long? = { null }): String {
    val key = state.providerRequestId?.takeIf(String::isNotBlank)
    val milliseconds = key?.let { replyMetrics(it) ?: state.firstTextMs }
    return assistantReplyLabel(milliseconds, provisional = true)
}

private fun assistantReplyLabel(firstTextMs: Long?, refusal: Boolean = false, provisional: Boolean = false): String = buildString {
    append("Assistant")
    firstTextMs?.takeIf { it >= 0 }?.let { append(" · ${formatVisibleDuration(it)}") }
    if (refusal) append(" · declined")
    if (provisional) append(" · not saved yet")
}

internal data class AssistantPagedChatsState(
    val revision: String = "0",
    val selectedConversationKey: String? = null,
    val chats: List<AssistantChat> = emptyList(),
    val count: Int = 0,
    val nextOffset: Int = 0,
    val loading: Boolean = false,
    val error: String? = null,
) {
    val hasMore: Boolean get() = nextOffset < count
}

internal data class AssistantPagedBranchesState(
    val revision: String = "0",
    val conversationKey: String? = null,
    val selectedHeadKey: String? = null,
    val heads: List<String> = emptyList(),
    val labels: Map<String, String> = emptyMap(),
    val count: Int = 0,
    val nextOffset: Int = 0,
    val loading: Boolean = false,
    val error: String? = null,
) {
    val hasMore: Boolean get() = nextOffset < count
}

internal fun assistantInitialPagedChats(view: JSONObject, selectedConversation: String?): AssistantPagedChatsState {
    val initial = assistantChats(view)
    val count = view.optInt("conversation_count", initial.size).coerceAtLeast(initial.size)
    val offset = if (view.has("conversation_offset")) view.getInt("conversation_offset") else initial.size
    return AssistantPagedChatsState(
        revision = view.optString("revision", "0"),
        selectedConversationKey = selectedConversation?.let(::assistantIdKey),
        chats = initial,
        count = count,
        nextOffset = offset.coerceAtMost(count),
    )
}

internal fun assistantMergePagedChats(
    current: AssistantPagedChatsState,
    rawPage: String?,
    settled: AssistantState,
    errorCode: String? = null,
): AssistantPagedChatsState {
    val settledView = runCatching { JSONObject(settled.viewJson) }.getOrNull()
    val settledRev = settledView?.optString("revision")
    val settledSelected = settled.conversation?.let(::assistantIdKey)
    if (errorCode == "revision_conflict" || settledRev != current.revision || settledSelected != current.selectedConversationKey) {
        return current.copy(loading = false, error = "Chats changed. Recheck and try again.")
    }
    return try {
        require(rawPage != null)
        val page = JSONObject(rawPage)
        require(page.getString("revision") == current.revision)
        val records = page.getJSONArray("conversations")
        val incoming = (0 until records.length()).map { records.getJSONObject(it) }
            .filter { !it.optBoolean("deleted") && !it.isNull("title") }
            .map { AssistantChat(it.getJSONObject("id").toString(), it.getString("title")) }
        val existingKeys = current.chats.map { assistantIdKey(it.id) }.toHashSet()
        val merged = current.chats + incoming.filter { existingKeys.add(assistantIdKey(it.id)) }
        val total = page.optInt("conversation_count", current.count).coerceAtLeast(merged.size)
        val nextOffset = page.optInt("next_offset", (current.nextOffset + incoming.size).coerceAtMost(total)).coerceAtMost(total)
        require(nextOffset > current.nextOffset || nextOffset >= total)
        current.copy(chats = merged, count = total, nextOffset = nextOffset, loading = false, error = null)
    } catch (_: Exception) {
        current.copy(loading = false, error = (errorCode ?: settled.error)?.let(::assistantErrorText) ?: "Chats unavailable. Retry.")
    }
}

internal fun assistantInitialPagedBranches(view: JSONObject, state: AssistantState): AssistantPagedBranchesState {
    val initial = state.branchChoices
    val labels = initial.associateWith { assistantBranchLabel(view, it) }
    val count = view.optInt("branch_count", initial.size).coerceAtLeast(initial.size)
    val offset = if (view.has("branch_offset") && initial.isNotEmpty()) view.getInt("branch_offset") else initial.size
    return AssistantPagedBranchesState(
        revision = view.optString("revision", "0"),
        conversationKey = state.conversation?.let(::assistantIdKey),
        selectedHeadKey = state.selectedHead?.let(::assistantIdKey),
        heads = initial,
        labels = labels,
        count = count,
        nextOffset = offset.coerceAtMost(count),
    )
}

internal fun assistantMergePagedBranches(
    current: AssistantPagedBranchesState,
    rawPage: String?,
    settled: AssistantState,
    errorCode: String? = null,
): AssistantPagedBranchesState {
    val settledView = runCatching { JSONObject(settled.viewJson) }.getOrNull()
    val settledRev = settledView?.optString("revision")
    val settledConv = settled.conversation?.let(::assistantIdKey)
    val settledHead = settled.selectedHead?.let(::assistantIdKey)
    if (errorCode == "revision_conflict" || settledRev != current.revision || settledConv != current.conversationKey || settledHead != current.selectedHeadKey) {
        return current.copy(loading = false, error = "Continuations changed. Recheck and try again.")
    }
    return try {
        require(rawPage != null)
        val page = JSONObject(rawPage)
        require(page.getString("revision") == current.revision)
        require(assistantIdKey(page.getJSONObject("conversation").toString()) == current.conversationKey)
        val headsJson = page.getJSONArray("heads")
        val incoming = (0 until headsJson.length()).map { headsJson.getJSONObject(it).toString() }
        val existingKeys = current.heads.map(::assistantIdKey).toHashSet()
        val mergedHeads = current.heads + incoming.filter { existingKeys.add(assistantIdKey(it)) }
        val mergedLabels = current.labels.toMutableMap()
        incoming.forEach { id -> mergedLabels[id] = assistantBranchLabel(page, id) }
        val total = page.optInt("head_count", current.count).coerceAtLeast(mergedHeads.size)
        val nextOffset = page.optInt("next_offset", (current.nextOffset + incoming.size).coerceAtMost(total)).coerceAtMost(total)
        require(nextOffset > current.nextOffset || nextOffset >= total)
        current.copy(heads = mergedHeads, labels = mergedLabels, count = total, nextOffset = nextOffset, loading = false, error = null)
    } catch (_: Exception) {
        current.copy(loading = false, error = (errorCode ?: settled.error)?.let(::assistantErrorText) ?: "Continuations unavailable. Retry.")
    }
}

@Composable
internal fun AssistantScreen(
    state: AssistantState, voice: AssistantVoiceState, history: AssistantHistoryState,
    pairing: PeerPairingState, preferences: WearVoicePreferences, actions: AssistantActions,
    observedPrompts: List<AssistantPendingPrompt>? = null,
    speechSelectionError: String? = null,
    speechSelectionPending: Boolean = false,
    replyMetrics: (String?) -> Long? = { null },
) {
    var page by rememberSaveable { mutableStateOf(if (voice.missingSpeechServices && voice.entryMode != null) AssistantPage.MISSING_SPEECH else AssistantPage.CHAT) }
    var modelOrigin by rememberSaveable { mutableStateOf(AssistantPage.CHAT) }
    var requestedModel by rememberSaveable { mutableStateOf<String?>(null) }
    var modelRequestToken by remember { mutableStateOf(0L) }
    var modelSelectionError by rememberSaveable { mutableStateOf<String?>(null) }
    val settledModel by rememberUpdatedState(state.model)
    val modelEnabled = !state.busy && !voice.active
    val backPage = if (page == AssistantPage.MODEL) modelOrigin else page.backPage()
    val openModels: (AssistantPage) -> Unit = { origin ->
        if (modelEnabled) {
            modelOrigin = origin
            requestedModel = null
            modelRequestToken++
            modelSelectionError = null
            page = AssistantPage.MODEL
        }
    }
    LaunchedEffect(requestedModel, modelRequestToken, page) {
        val requested = requestedModel ?: return@LaunchedEffect
        val token = modelRequestToken
        if (page != AssistantPage.MODEL) return@LaunchedEffect
        delay(5_000)
        if (assistantModelConfirmationExpired(token, modelRequestToken, requested, requestedModel,
                settledModel, page == AssistantPage.MODEL)) {
            requestedModel = null
            modelSelectionError = "Model selection was not confirmed. Try again."
        }
    }
    LaunchedEffect(state.model, requestedModel) {
        if (requestedModel != null && state.model == requestedModel) {
            requestedModel = null
            if (page == AssistantPage.MODEL) page = modelOrigin
        }
    }
    LaunchedEffect(state.busy, voice.active) {
        if ((state.busy || voice.active) && page == AssistantPage.MODEL) page = AssistantPage.CHAT
    }
    LaunchedEffect(state.error) {
        if (state.error != null) requestedModel = null
    }
    val view = remember(state.viewJson) { JSONObject(state.viewJson) }
    val viewRevision = view.optString("revision", "0")
    val selectedConversationKey = state.conversation?.let(::assistantIdKey)
    val selectedHeadKey = state.selectedHead?.let(::assistantIdKey)
    var pagedChats by remember(viewRevision, selectedConversationKey, state.viewJson) {
        mutableStateOf(assistantInitialPagedChats(view, state.conversation))
    }
    var chatsRequestToken by remember(viewRevision, selectedConversationKey, state.viewJson) { mutableStateOf(0L) }
    val loadMoreChats: () -> Unit = {
        if (!pagedChats.loading && pagedChats.hasMore && !state.busy && !voice.active) {
            val token = ++chatsRequestToken
            val expectedRevision = pagedChats.revision
            val offset = pagedChats.nextOffset
            pagedChats = pagedChats.copy(loading = true, error = null)
            actions.loadConversations(expectedRevision, offset) { raw, settled, errorCode ->
                if (token == chatsRequestToken) {
                    pagedChats = assistantMergePagedChats(pagedChats, raw, settled, errorCode)
                }
            }
        }
    }
    var pagedBranches by remember(viewRevision, selectedConversationKey, selectedHeadKey, state.branchChoices, state.viewJson) {
        mutableStateOf(assistantInitialPagedBranches(view, state))
    }
    var branchesRequestToken by remember(viewRevision, selectedConversationKey, selectedHeadKey, state.branchChoices, state.viewJson) { mutableStateOf(0L) }
    val loadMoreBranches: () -> Unit = {
        val conv = state.conversation
        if (conv != null && !pagedBranches.loading && pagedBranches.hasMore && !state.busy && !voice.active) {
            val token = ++branchesRequestToken
            val expectedRevision = pagedBranches.revision
            val offset = pagedBranches.nextOffset
            pagedBranches = pagedBranches.copy(loading = true, error = null)
            actions.loadBranches(expectedRevision, conv, state.selectedHead, offset) { raw, settled, errorCode ->
                if (token == branchesRequestToken) {
                    pagedBranches = assistantMergePagedBranches(pagedBranches, raw, settled, errorCode)
                }
            }
        }
    }
    val chats = pagedChats.chats
    val branchLabels = pagedBranches.labels
    val title = chats.firstOrNull { it.id.let(::assistantIdKey) == state.conversation?.let(::assistantIdKey) }?.title ?: "Assistant"
    val promptPresentation = remember { AssistantPromptPresentation() }
    val chatScreenIdentity = rememberSaveable(saver = AssistantChatScreenIdentity.Saver) { AssistantChatScreenIdentity() }
    val chatScreenKey = chatScreenIdentity.key(state)
    val pendingPrompts = observedPrompts ?: promptPresentation.project(state, voice, history)
    LaunchedEffect(page, state.foreground, state.busy, voice.active) {
        if (page in setOf(AssistantPage.SETTINGS, AssistantPage.SPEECH_LANGUAGE, AssistantPage.SPEECH_ENGINE, AssistantPage.SPEECH_VOICE) &&
            state.foreground && !state.busy && !voice.active) actions.prepareSpeechOptions()
    }
    BackHandler(page != AssistantPage.CHAT) { page = backPage }
    WearSurface {
        AssistantArtwork(state, page == AssistantPage.CHAT, assistantArtworkListening(voice))
        key(chatScreenKey) {
            CompositionLocalProvider(LocalScreenIsActive provides (page == AssistantPage.CHAT && LocalScreenIsActive.current)) {
                val repeatAfterReply = assistantRepeatsAfterReply(voice, preferences)
                ChatScreen(state, voice, history, pendingPrompts, actions, repeatAfterReply, page == AssistantPage.CHAT,
                    { page = AssistantPage.SETTINGS }, replyMetrics, branchLabels,
                    pagedBranches = pagedBranches, onLoadMoreBranches = loadMoreBranches,
                    onMissingSpeech = { page = AssistantPage.MISSING_SPEECH },
                    openModels = { openModels(AssistantPage.CHAT) })
            }
        }
        if (page != AssistantPage.CHAT) {
            key(page) {
                var entered by remember { mutableStateOf(false) }
                val entrance by animateFloatAsState(if (entered || page != AssistantPage.MODEL) 1f else 0f,
                    animationSpec = tween(150), label = "Assistant page entrance")
                LaunchedEffect(Unit) { entered = true }
                Box(Modifier.fillMaxSize().graphicsLayer {
                    alpha = entrance
                    scaleX = 0.97f + 0.03f * entrance
                    scaleY = scaleX
                }) {
                BasicSwipeToDismissBox(onDismissed = { page = backPage }, state = rememberWearDismissState()) { background ->
                    WearSurface {
                        if (background) Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
                            Text(if (backPage == AssistantPage.SETTINGS) "Settings" else title)
                        }
                        else when (page) {
                            AssistantPage.CHATS -> ChatsScreen(pagedChats, state, voice, actions, loadMoreChats) { page = AssistantPage.CHAT }
                             AssistantPage.SETTINGS -> SettingsScreen(state, voice, pairing, actions, preferences,
                                 { page = AssistantPage.CHAT }, { page = AssistantPage.CHATS },
                                  { page = AssistantPage.CHAT; actions.edit() }, speechSelectionError, speechSelectionPending,
                                  { openModels(AssistantPage.SETTINGS) }) { field ->
                                     page = when (field) {
                                         AssistantSpeechField.LANGUAGE -> AssistantPage.SPEECH_LANGUAGE
                                         AssistantSpeechField.ENGINE -> AssistantPage.SPEECH_ENGINE
                                         AssistantSpeechField.VOICE -> AssistantPage.SPEECH_VOICE
                                     }
                                 }
                             AssistantPage.SPEECH_LANGUAGE, AssistantPage.SPEECH_ENGINE, AssistantPage.SPEECH_VOICE -> {
                                 val field = when (page) {
                                     AssistantPage.SPEECH_LANGUAGE -> AssistantSpeechField.LANGUAGE
                                     AssistantPage.SPEECH_ENGINE -> AssistantSpeechField.ENGINE
                                     else -> AssistantSpeechField.VOICE
                                 }
                                 SpeechSelectionScreen(state, voice, actions, field, speechSelectionError, speechSelectionPending) { page = AssistantPage.SETTINGS }
                             }
                             AssistantPage.MODEL -> ScreenList("Model", { page = modelOrigin },
                                 if (modelOrigin == AssistantPage.SETTINGS) "Back to Settings" else "Back to chat") {
                                 requestedModel?.let { item { Text("Waiting for model selection") } }
                                 modelSelectionError?.let { error -> item { Text(error, color = MaterialTheme.colorScheme.error) } }
                                 assistantRequestError(state)?.let { error -> item { Text(assistantErrorText(error)) } }
                                 items(assistantSelectableModels(AssistantRuntime.optionalProviderReadiness(), state.model), key = { it }) { model ->
                                     val label = AssistantModels.presentation[model]?.fullLabel ?: model
                                     ActionButton((if (model == state.model) "✓ " else "") + label,
                                         modelEnabled && requestedModel == null) {
                                         if (!state.busy && !voice.active) {
                                             if (model == state.model) page = modelOrigin
                                             else {
                                                 modelSelectionError = null
                                                 modelRequestToken++
                                                 requestedModel = model
                                                 actions.selectModel(model)
                                             }
                                         }
                                     }
                                 }
                             }
                             AssistantPage.MISSING_SPEECH -> MissingSpeechScreen(voice, actions) { page = AssistantPage.CHAT }
                            AssistantPage.CHAT -> Unit
                        }
                    }
                }
                }
            }
        }
    }
}

@Composable
private fun ChatScreen(
    state: AssistantState, voice: AssistantVoiceState, history: AssistantHistoryState,
    pendingPrompts: List<AssistantPendingPrompt>,
    actions: AssistantActions, conversationMode: Boolean, active: Boolean, settings: () -> Unit,
    replyMetrics: (String?) -> Long?,
    branchLabels: Map<String, String>,
    pagedBranches: AssistantPagedBranchesState = AssistantPagedBranchesState(heads = state.branchChoices, labels = branchLabels),
    onLoadMoreBranches: () -> Unit = {},
    onMissingSpeech: () -> Unit = {},
    openModels: () -> Unit = {},
) {
    val list = rememberLazyListState()
    val reading = rememberSaveable(saver = AssistantReadingPosition.Saver) { AssistantReadingPosition() }
    var initialHistoryPositioned by rememberSaveable { mutableStateOf(false) }
    val requestIdentity = assistantReadingRequestKey(state)
    val scope = rememberCoroutineScope()
    val manualScroll = remember(list, reading) { object : NestedScrollConnection {
        override fun onPreScroll(available: Offset, source: NestedScrollSource): Offset {
            if (source == NestedScrollSource.UserInput && available.y != 0f) {
                reading.readOlder(list.canScrollForward || (available.y > 0f && list.canScrollBackward))
            }
            return Offset.Zero
        }
    } }
    var olderAnchor by remember { mutableStateOf<Pair<String, Int>?>(null) }
    val continuity = remember { AssistantHistoryPresentation() }
    val visibleAnchor = assistantVisibleReadingAnchor(list, reading.replyRowKey)
    val transition = continuity.update(state, history, reading.followsLatest, visibleAnchor)
    val readyHistory = transition.ready
    val turns = transition.turns
    val reply = transition.reply
    val itemKeys = transition.itemKeys
    val visiblePrompts = pendingPrompts.filterNot { it.key in itemKeys.rowKeys || it.key == transition.retiredPromptKey }
    val replyPromptIndex = visiblePrompts.indexOfLast { it.key == reply?.requestKey?.let(::assistantPromptItemKey) }
    val promptsBeforeReply = if (replyPromptIndex < 0) visiblePrompts else visiblePrompts.take(replyPromptIndex + 1)
    val promptsAfterReply = if (replyPromptIndex < 0) emptyList() else visiblePrompts.drop(replyPromptIndex + 1)
    val livePromptOwner = if (voice.listening) "pending-dictation:${voice.dictation.utterance}" else null
    val livePrompt = visiblePrompts.lastOrNull { it.key == livePromptOwner }
    reading.observeLivePrompt(livePromptOwner, livePrompt?.text?.isNotBlank() == true)
    val followLivePrompt = reading.followsLivePrompt(livePromptOwner)
    val currentLivePrompt by rememberUpdatedState(livePrompt)
    LaunchedEffect(livePromptOwner, followLivePrompt, active) {
        val owner = livePromptOwner ?: return@LaunchedEffect
        if (!active || !followLivePrompt) return@LaunchedEffect
        snapshotFlow {
            val layout = list.layoutInfo
            currentLivePrompt?.let { it.key == owner && it.text.isNotBlank() } == true &&
                layout.viewportSize.height > 0 && layout.totalItemsCount > 0
        }.first { it }
        if (!reading.followsLivePrompt(owner)) return@LaunchedEffect
        if (reading.beginLivePromptReveal(owner)) {
            reading.jumpLatest({ list.canScrollForward }) { list.animateToEnd() }
        }
        // Stable utterance identity survives partial rewrites; manual input revokes eligibility.
        snapshotFlow {
            val layout = list.layoutInfo
            Triple(list.canScrollForward, layout.totalItemsCount,
                layout.visibleItemsInfo.lastOrNull()?.let { it.offset to it.size })
        }.collect { (canForward, count, _) ->
            if (canForward && count > 0 && reading.followsLivePrompt(owner) &&
                currentLivePrompt?.let { it.key == owner && it.text.isNotBlank() } == true) reading.following {
                list.scrollToEnd()
            }
        }
    }
    LaunchedEffect(transition.scroll) {
        val scroll = transition.scroll ?: return@LaunchedEffect
        olderAnchor = null
        reading.following {
            if (scroll.index != null) list.scrollToItem(scroll.index + if (history.error != null) 1 else 0, scroll.offset)
            else if (scroll.fallbackToLatest) {
                reading.latest()
                list.scrollToEnd()
            }
        }
        continuity.scrolled(scroll)
    }
    LaunchedEffect(turns, history.loading, transition.scroll) {
        val anchor = olderAnchor
        if (anchor != null && !history.loading && transition.scroll == null) {
            val index = itemKeys.index(anchor.first)
            if (index != null) list.scrollToItem(index + if (history.error != null) 1 else 0, anchor.second)
            olderAnchor = null
        }
    }
    LaunchedEffect(list, state.conversation) {
        snapshotFlow { Triple(list.isScrollInProgress, list.canScrollForward, reading.isFollowing) }.collect { (scrolling, canForward, following) ->
            if (!following) reading.scrolled(scrolling, canForward)
        }
    }
    val activeRequestOwner = assistantRequestOwnerKey(state)
    val settledCanonicalTurn = if (readyHistory && reply == null && !state.busy && reading.followsLatest && activeRequestOwner != null &&
        reading.currentRequestKey == activeRequestOwner) {
        turns.lastOrNull {
            (state.providerRequestId != null && it.requestKey == state.providerRequestId) ||
                assistantIdKey(it.id) == state.selectedHead?.let(::assistantIdKey)
        }
    } else null
    val replyIndex = when {
        reply != null -> 1 + (if (state.conversation == null) 1 + (if (voice.missingSpeechServices) 1 else 0) else 0) +
            (if (history.error != null) 1 else 0) + turns.size * 2 + promptsBeforeReply.size
        settledCanonicalTurn != null -> itemKeys.index(itemKeys.answer(settledCanonicalTurn))?.let {
            it + if (history.error != null) 1 else 0
        }
        else -> null
    }
    val replyRowKey = reply?.key ?: settledCanonicalTurn?.let(itemKeys::answer)
    LaunchedEffect(requestIdentity, state.providerRequestId, state.busy, readyHistory, replyRowKey, state.error, state.interrupted) {
        if (requestIdentity != null) reading.observeRequest(requestIdentity,
            manuallyScrolling = list.isScrollInProgress && !reading.isFollowing,
            providerKey = state.providerRequestId?.takeIf(String::isNotBlank))
        else if (!state.busy && (replyRowKey == null || !reading.followsLatest) &&
            (readyHistory || state.error != null || state.interrupted || state.conversation == null)) {
            reading.completed(list.canScrollForward)
        }
    }
    LaunchedEffect(replyRowKey, replyIndex, requestIdentity, state.busy, readyHistory, active, reading.phase, transition.scroll) {
        val index = replyIndex ?: return@LaunchedEffect
        val rowKey = replyRowKey ?: return@LaunchedEffect
        initialHistoryPositioned = true
        if (!active || !reading.followsLatest || transition.scroll != null) return@LaunchedEffect
        reading.trackReplyRow(rowKey)
        snapshotFlow {
            val layout = list.layoutInfo
            val row = layout.visibleItemsInfo.firstOrNull { it.key == rowKey }
            AssistantReplyGeometry(row?.offset, row?.size, layout.viewportSize.height,
                layout.beforeContentPadding, layout.afterContentPadding, list.canScrollForward, layout.totalItemsCount)
        }.collect { geometry ->
            if (!reading.followsLatest || geometry.itemCount <= index) return@collect
            when (reading.followDecision(geometry)) {
                AssistantReplyFollowDecision.NONE -> if (!state.busy && readyHistory && reply == null && geometry.rowOffset != null) {
                    reading.completed(geometry.canScrollForward)
                }
                AssistantReplyFollowDecision.HOLD -> if (reply?.text?.isNotBlank() != false) {
                    reading.replyPositioned(geometry.rowOffset!!, geometry.relativeCap, geometry.canScrollForward)
                }
                AssistantReplyFollowDecision.POSITION -> {
                    // End-clamping gives short replies their natural position without a footer.
                    // A huge first chunk targets its label directly, never its tail.
                    reading.following { list.scrollToItem(index, -geometry.relativeCap) }
                }
            }
        }
    }
    LaunchedEffect(readyHistory, turns.size, active, state.busy) {
        if (!active || !readyHistory || state.busy || initialHistoryPositioned || reading.currentRequestKey != null ||
            !reading.followsLatest) return@LaunchedEffect
        snapshotFlow { list.layoutInfo }.first { it.viewportSize.height > 0 && it.totalItemsCount >= 1 + turns.size * 2 }
        if (!reading.followsLatest) return@LaunchedEffect
        initialHistoryPositioned = true
        reading.following { list.scrollToEnd() }
    }
    val showLatest = assistantShowsLatest(state, voice,
        readingOlder = !reading.followsLatest, measuredCanScrollForward = list.canScrollForward)
    val layoutDirection = LocalLayoutDirection.current
    var headerHeight by remember { mutableStateOf(56.dp) }
    ScreenScaffold(scrollState = list, timeText = {
        AssistantModelHeader(
            model = AssistantModels.presentation[state.model],
            enabled = !state.busy && !voice.active,
            openModels = openModels,
            onExclusionHeight = { headerHeight = it },
        )
    }, modifier = Modifier.fillMaxSize().settingsSwipe(active, settings)) { contentPadding ->
        BoxWithConstraints(Modifier.fillMaxSize()) {
            val inset = maxHeight * 0.10f
            val readingPadding = PaddingValues(
                start = contentPadding.calculateStartPadding(layoutDirection),
                top = maxOf(0.dp, headerHeight - inset),
                end = contentPadding.calculateEndPadding(layoutDirection),
                bottom = maxOf(0.dp, maxOf(48.dp, CompactEdgeButtonDefaults.Height) + 3.dp - inset),
            )
            LazyColumn(Modifier.fillMaxSize()
                // Intentional project override: chrome may overlap middle history;
                // end padding clears first/last text, bezel clipping reserves 10% each.
                .padding(vertical = inset)
                .listRotary(list, active) { delta ->
                    if (delta != 0f) reading.readOlder(list.canScrollForward || (delta < 0f && list.canScrollBackward))
                }
                .nestedScroll(manualScroll)
                .clipToBounds(), state = list,
                contentPadding = readingPadding,
                verticalArrangement = Arrangement.spacedBy(8.dp)) {
                item(key = "older") {
                     if (history.olderBefore != null && readyHistory) ActionButton("Older messages", !history.loading && history.error == null) {
                        reading.readOlder()
                        list.layoutInfo.visibleItemsInfo.firstOrNull { itemKeys.index(it.key.toString()) != null }?.let {
                            olderAnchor = itemKeys.anchorKey(it.key.toString()) to -it.offset
                        }
                        actions.older()
                    }
                }
                if (state.conversation == null) {
                    item(key = "choose-chat") {
                        Text("Type or dictate a question. Send starts a new chat.")
                    }
                    if (voice.missingSpeechServices) item(key = "missing-speech-chat") {
                        ActionButton("Speech services required · Open Play Store", true, onMissingSpeech)
                    }
                }
                if (state.branchChoices.isNotEmpty()) {
                    val displayedBranches = pagedBranches.heads.ifEmpty { state.branchChoices }
                    item(key = "branches") { Text("Choose a continuation", color = MaterialTheme.colorScheme.primary) }
                    items(displayedBranches, key = { "branch:${assistantIdKey(it)}" }) { id ->
                        ActionButton(pagedBranches.labels[id] ?: branchLabels[id] ?: "Continuation awaiting messages", !state.busy && !voice.active) { actions.selectHead(id) }
                    }
                    pagedBranches.error?.let { branchError ->
                        item(key = "branches-error") {
                            Text(branchError, color = MaterialTheme.colorScheme.error)
                        }
                    }
                    if (pagedBranches.hasMore) {
                        item(key = "branches-more") {
                            ActionButton(
                                if (pagedBranches.loading) "Loading continuations…" else "More continuations",
                                !state.busy && !voice.active && !pagedBranches.loading,
                                onLoadMoreBranches,
                            )
                        }
                    }
                } else if (state.conversation != null && turns.isEmpty() && reply == null && visiblePrompts.isEmpty()) item(key = "empty") {
                    Text(if (history.error != null) "Conversation unavailable" else if (history.loading || !readyHistory) "Loading conversation…" else "No messages yet. Ask a question.")
                }
                history.error?.let { error -> item(key = "history-error") {
                    Text(error, color = MaterialTheme.colorScheme.error)
                    ActionButton("Retry history", !history.loading, actions.retryHistory)
                } }
                turns.forEach { turn ->
                    item(key = itemKeys.prompt(turn)) {
                        Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text("You", style = wearTextStyle(MaterialTheme.typography.labelMedium), color = MaterialTheme.colorScheme.primary)
                            Text(turn.prompt)
                        }
                    }
                    item(key = itemKeys.answer(turn)) {
                        Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text(assistantCompletedReplyLabel(turn, replyMetrics), style = wearTextStyle(MaterialTheme.typography.labelMedium))
                            Text(turn.answer, onTextLayout = { actions.replyLayout(turn.requestKey) },
                                modifier = Modifier.drawWithContent {
                                    drawContent()
                                    actions.replyDrawSubmitted(turn.requestKey)
                                })
                        }
                    }
                }
                items(promptsBeforeReply, key = { it.key }) { prompt ->
                    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text("You", style = wearTextStyle(MaterialTheme.typography.labelMedium), color = MaterialTheme.colorScheme.primary)
                        Text(prompt.text)
                    }
                }
                reply?.let { projected -> item(key = projected.key) {
                    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        val waiting = projected.text.isBlank()
                        Text(assistantReplyLabel(if (waiting) null else projected.requestKey?.let { replyMetrics(it) ?: projected.firstTextMs }, provisional = true),
                            style = wearTextStyle(MaterialTheme.typography.labelMedium))
                        Text(if (waiting) assistantToolActivity(state) ?: "Thinking…" else projected.text,
                            onTextLayout = { if (!waiting) actions.replyLayout(projected.requestKey) },
                            modifier = Modifier.drawWithContent {
                                drawContent()
                                if (!waiting) actions.replyDrawSubmitted(projected.requestKey)
                            })
                    }
                } }
                items(promptsAfterReply, key = { it.key }) { prompt ->
                    Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text("You", style = wearTextStyle(MaterialTheme.typography.labelMedium), color = MaterialTheme.colorScheme.primary)
                        Text(prompt.text)
                    }
                }
                assistantToolActivity(state)?.takeIf { reply == null || reply.text.isNotBlank() }?.let { activity -> item(key = "tool-activity") {
                    Text(activity, style = wearTextStyle(MaterialTheme.typography.labelMedium), color = MaterialTheme.colorScheme.primary)
                } }
                assistantRequestError(state)?.let { item(key = "request-error") { Text(assistantErrorText(it), color = MaterialTheme.colorScheme.error) } }
                assistantVisibleVoiceErrors(voice).forEach { error ->
                    item(key = "voice-error:$error") {
                        Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            Text(assistantVoiceErrorText(error), color = MaterialTheme.colorScheme.error)
                            if (assistantIsInstallableVoiceError(error, voice)) {
                                ActionButton(
                                    if (voice.missingSpeechServices) "Open Play Store" else "Install voice data",
                                    true,
                                    actions.openSpeechStore
                                )
                            }
                        }
                    }
                }
                if (voice.launchConfirmationRequired) item(key = "voice-confirmation") {
                    Text("Tap ${if (voice.entryMode == AssistantVoiceEntryMode.TALK) "Talk" else "Ask"} to start voice.")
                }
                item(key = "tail") { Spacer(Modifier.height(1.dp)) }
            }
            if (!showLatest) {
                Box(Modifier.align(Alignment.BottomCenter).fillMaxWidth().height(maxOf(48.dp, CompactEdgeButtonDefaults.Height)),
                    contentAlignment = Alignment.BottomCenter) {
                    PrimaryEdgeButton(state, voice, actions.copy(stop = { continuity.stopReply(); actions.stop() }), conversationMode, onMissingSpeech)
                }
            }
            if (showLatest) {
                // Both dock controls share the same viewport clearance, so swapping preserves the readable end.
                IconButton(onClick = {
                    scope.launch { reading.jumpLatest({ list.canScrollForward }) {
                        list.scrollToEnd()
                    } }
                }, modifier = Modifier.align(Alignment.BottomCenter)
                    .padding(bottom = 6.dp).size(48.dp).semantics { contentDescription = "Jump to latest" },
                    colors = IconButtonDefaults.iconButtonColors(containerColor = Color.Transparent,
                        disabledContainerColor = Color.Transparent, contentColor = MaterialTheme.colorScheme.onBackground)) {
                    LatestGlyph()
                }
            }
        }
    }
}

private suspend fun LazyListState.animateToEnd() {
    val layout = layoutInfo
    if (layout.totalItemsCount == 0 || layout.viewportSize.height <= 0) return
    animateScrollToItem(layout.totalItemsCount - 1, layout.viewportSize.height)
}

private suspend fun LazyListState.scrollToEnd() {
    val layout = layoutInfo
    if (layout.totalItemsCount == 0 || layout.viewportSize.height <= 0) return
    // Positive offsets move the tail up; one measured viewport lets end-clamping
    // settle at the actual bottom rather than the start of the tail anchor.
    scrollToItem(layout.totalItemsCount - 1, layout.viewportSize.height)
}

@Composable
private fun Modifier.settingsSwipe(active: Boolean, open: () -> Unit): Modifier {
    val openSettings by rememberUpdatedState(open)
    return semantics {
        if (active) customActions = listOf(CustomAccessibilityAction("Open Settings") { openSettings(); true })
    }.pointerInput(active) {
        if (!active) return@pointerInput
        awaitEachGesture {
            val down = awaitFirstDown(requireUnconsumed = false)
            var distance = 0f
            val start = awaitHorizontalTouchSlopOrCancellation(down.id) { change, overSlop ->
                if (overSlop < 0f) { change.consume(); distance = overSlop }
            }
            if (start != null && horizontalDrag(start.id) { change ->
                distance += change.position.x - change.previousPosition.x
                change.consume()
            } && distance < -size.width * 0.25f) openSettings()
        }
    }
}

@Composable
private fun ChatsScreen(
    pagedChats: AssistantPagedChatsState,
    state: AssistantState,
    voice: AssistantVoiceState,
    actions: AssistantActions,
    onLoadMore: () -> Unit = {},
    back: () -> Unit,
) {
    val chats = pagedChats.chats
    val enabled = !state.busy && !voice.active
    ScreenList("Chats", back) {
        item { ActionButton("New chat", enabled) { actions.new(); back() } }
        if (chats.isEmpty()) item { Text("No saved chats yet. New chat preserves existing drafts.") }
        items(chats, key = { assistantIdKey(it.id) }) { chat ->
            FilledTonalButton(onClick = { actions.selectConversation(chat.id); back() }, enabled = enabled,
                label = { Text(chat.title, maxLines = Int.MAX_VALUE, overflow = TextOverflow.Clip) }, secondaryLabel = {
                    Text(if (chat.id.let(::assistantIdKey) == state.conversation?.let(::assistantIdKey)) "Selected" else "Open conversation")
                }, modifier = Modifier.fillMaxWidth())
        }
        pagedChats.error?.let { chatError ->
            item { Text(chatError, color = MaterialTheme.colorScheme.error) }
        }
        if (pagedChats.hasMore) {
            item {
                ActionButton(
                    if (pagedChats.loading) "Loading chats…" else "More chats",
                    enabled && !pagedChats.loading,
                    onLoadMore,
                )
            }
        }
    }
}

@Composable
private fun SettingsScreen(state: AssistantState, voice: AssistantVoiceState, pairing: PeerPairingState, actions: AssistantActions,
    preferences: WearVoicePreferences, back: () -> Unit, chats: () -> Unit, edit: () -> Unit,
    speechSelectionError: String?, speechSelectionPending: Boolean, openModels: () -> Unit,
    openSpeech: (AssistantSpeechField) -> Unit) {
    val enabled = !state.busy && !voice.active
    val speechSettings = assistantSpeechSettings(voice, preferences)
    ScreenList("Settings", back) {
        item { ActionButton("New chat", state.foreground && enabled) { actions.new(); back() } }
        item { ActionButton("Chats", state.foreground && enabled, chats) }
        item { ActionButton("Keyboard", state.foreground && !state.busy, edit) }
        item { SwitchButton(checked = state.webEnabled, onCheckedChange = actions.setWebEnabled,
            label = { Text("Web research") }, secondaryLabel = { Text(if (state.webEnabled) "Allows web tools when supported by the selected model" else "Web tools will not be dispatched",
                maxLines = Int.MAX_VALUE, overflow = TextOverflow.Clip) }, modifier = Modifier.fillMaxWidth()) }
        item { ActionButton("Model: ${AssistantModels.presentation[state.model]?.fullLabel ?: state.model}", enabled, openModels) }
        item { SwitchButton(checked = preferences.readRepliesAloud, onCheckedChange = actions.tts, enabled = enabled,
            label = { Text("Speak replies") }, secondaryLabel = { Text("Local voice preferred") },
            modifier = Modifier.fillMaxWidth()) }
        item { SwitchButton(checked = state.speechNetworkFallback, onCheckedChange = actions.setSpeechNetworkFallback,
            enabled = assistantCanChangeSpeechFallback(state, voice),
            label = { Text("Speech network fallback") }, secondaryLabel = { Text(assistantSpeechFallbackDescription(state, voice),
                maxLines = Int.MAX_VALUE, overflow = TextOverflow.Clip) },
            modifier = Modifier.fillMaxWidth()) }
        item { SwitchButton(checked = preferences.conversationMode, onCheckedChange = actions.setConversationMode, enabled = enabled,
            label = { Text("Talk by default") }, secondaryLabel = { Text(if (preferences.conversationMode) "Reopen mic after reply" else "Ask once, then stop",
                maxLines = Int.MAX_VALUE, overflow = TextOverflow.Clip) }, modifier = Modifier.fillMaxWidth()) }
        item { Text("Ask sends one finalized voice turn. Talk listens again after replying. Square Stop cancels listening, response and playback; it never sends. Backgrounding stops voice; restart deliberately.") }
        item { Text("Saved speech requests", style = wearTextStyle(MaterialTheme.typography.titleMedium)) }
        item { Text("Language, engine and voice are persisted requests, not proof of the active processing route. Labels change only when native state reports the choice.") }
        if (speechSelectionPending) item { Text("Waiting for native speech-settings settlement") }
        speechSelectionError?.let { error -> item { Text(assistantSpeechSelectionErrorText(error), color = MaterialTheme.colorScheme.error) } }
        assistantRequestError(state)?.let { error -> item { Text(assistantErrorText(error), color = MaterialTheme.colorScheme.error) } }
        item { ActionButton("Speech language: ${state.speechLanguage.ifEmpty { "Automatic" }}", true) { openSpeech(AssistantSpeechField.LANGUAGE) } }
        item { Text("Changing engine explicitly clears the old voice to Automatic. No other engine or voice is substituted.") }
        item { ActionButton("Reply voice engine: ${state.speechTtsProvider.ifEmpty { "Automatic" }}", true) { openSpeech(AssistantSpeechField.ENGINE) } }
        item { ActionButton("Speech voice: ${state.speechTtsVoice.ifEmpty { "Automatic" }}", true) { openSpeech(AssistantSpeechField.VOICE) } }
        item { Text("Speech model selection unavailable · model identifiers not reported") }
        item { ActionButton("Reset all speech to Automatic", assistantCanSelectSpeech(state, voice, speechSelectionPending), actions.resetSpeechSelection) }
        item { Text("Speech routes", style = wearTextStyle(MaterialTheme.typography.titleMedium)) }
        if (voice.missingSpeechServices) {
            item {
                Text(voice.speechRequirementMessage ?: "Speech Recognition & Synthesis by Google is required for local STT or TTS.", color = MaterialTheme.colorScheme.error)
            }
            item {
                ActionButton("Open Play Store", true, actions.openSpeechStore)
            }
        }
        item { Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(speechSettings.dictationStatus, style = wearTextStyle(MaterialTheme.typography.labelMedium))
            speechSettings.dictationDetails?.let { Text(it) }
        } }
        item { Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(speechSettings.replyStatus, style = wearTextStyle(MaterialTheme.typography.labelMedium))
            speechSettings.replyDetails?.let { Text(it) }
        } }
        item { Text("Requested languages and engine preferences are configuration, not route proof. Actual processing and hidden speech model identities: not reported. Installed languages and offline verification are unknown unless supplied by the native provider.") }
         item { Text(assistantDictationAvailabilityDescription(state, voice)) }
        item { Text("Keyboard microphone follows your IME; it is not guaranteed offline.") }
        item { Text("Conversation sync", style = wearTextStyle(MaterialTheme.typography.titleMedium)) }
        item { Text(assistantLinkStatusText(pairing.reason)) }
        item { ActionButton("Recheck connection", true) { actions.recheck(); actions.prepareSpeechOptions() } }
    }
}

@Composable
private fun ScreenList(title: String, back: () -> Unit, backLabel: String = "Back to chat", content: androidx.compose.foundation.lazy.LazyListScope.() -> Unit) {
    val state = rememberLazyListState()
    ScreenScaffold(scrollState = state) { contentPadding ->
        LazyColumn(Modifier.listRotary(state, true).fillMaxSize(), state = state,
            contentPadding = contentPadding, verticalArrangement = Arrangement.spacedBy(8.dp)) {
            item { Text(title, style = wearTextStyle(MaterialTheme.typography.titleMedium)) }
            item { ActionButton(backLabel, true, back) }
            content()
        }
    }
}

@Composable
private fun SpeechSelectionScreen(state: AssistantState, voice: AssistantVoiceState, actions: AssistantActions,
    field: AssistantSpeechField, error: String?, pending: Boolean, back: () -> Unit) {
    val catalog = remember(state.speechAdmission(), voice.speechOptions, field) { assistantSpeechCatalog(state, voice, field) }
    val enabled = assistantCanSelectSpeech(state, voice, pending)
    ScreenList(field.title, back, "Back to Settings") {
        item { Text("Saved request: ${catalog.savedLabel}" + when {
            !catalog.savedReported -> if (catalog.savedAvailable) " · requested variant not reported exactly" else " · not reported / unsupported"
            catalog.savedValue.isNotEmpty() && !catalog.savedAvailable -> " · unavailable under current policy or saved choices"
            else -> ""
        }) }
        item { Text(catalog.evidence) }
        if (pending) item { Text("Waiting for native speech-settings settlement") }
        item { Text("Automatic uses the system/default request; it is not an observed engine, voice or local-processing claim.") }
        if (field == AssistantSpeechField.ENGINE) item { Text("Choosing a different engine clears the old voice to Automatic. Choose a new voice only from that engine's reported inventory.") }
        if (!catalog.savedReported) item { Text("The saved value is not listed in this catalog. Automatic clears the explicit request; reset all requests if another saved choice also blocks admission.") }
        error?.let { item { Text(assistantSpeechSelectionErrorText(it), color = MaterialTheme.colorScheme.error) } }
        state.error?.let { item { Text(assistantErrorText(it), color = MaterialTheme.colorScheme.error) } }
        item { ActionButton("Reset all speech to Automatic", enabled, actions.resetSpeechSelection) }
        items(catalog.choices, key = { it.value }) { choice ->
            ActionButton((if (choice.value == catalog.savedValue) "✓ " else "") + choice.label, enabled && choice.enabled) {
                when (field) {
                    AssistantSpeechField.LANGUAGE -> actions.selectSpeechLanguage(choice.value)
                    AssistantSpeechField.ENGINE -> actions.selectSpeechEngine(choice.value)
                    AssistantSpeechField.VOICE -> actions.selectSpeechVoice(choice.value)
                }
            }
        }
        item { Text("Disabled reported choices cannot be admitted with the current language, voice or network policy. No model downloads or hidden model choices are offered.") }
    }
}

@Composable
private fun ActionButton(label: String, enabled: Boolean, action: () -> Unit) {
    FilledTonalButton(onClick = action, enabled = enabled,
        label = { Text(label, style = wearTextStyle(LocalTextStyle.current), maxLines = Int.MAX_VALUE, overflow = TextOverflow.Clip) },
        modifier = Modifier.fillMaxWidth())
}

@Composable
private fun MissingSpeechScreen(voice: AssistantVoiceState, actions: AssistantActions, back: () -> Unit) {
    val message = voice.speechRequirementMessage
        ?: "Speech Recognition & Synthesis by Google is required for local voice dictation and speech output on your watch."
    ScreenList("Speech Required", back, "Back to chat") {
        item {
            Text(message, style = wearTextStyle(MaterialTheme.typography.bodyMedium))
        }
        item {
            ActionButton("Open Play Store", true, actions.openSpeechStore)
        }
    }
}

@Composable
private fun PrimaryEdgeButton(state: AssistantState, voice: AssistantVoiceState, actions: AssistantActions, conversationMode: Boolean, onMissingSpeech: () -> Unit = {}) {
    val control = assistantPrimaryControl(state, voice, conversationMode)
    val action = when (control.action) {
        AssistantPrimaryAction.STOP -> actions.stop
        AssistantPrimaryAction.SEND -> actions.send
        AssistantPrimaryAction.TALK -> if (voice.missingSpeechServices) onMissingSpeech else actions.conversationMode
        AssistantPrimaryAction.ASK -> if (voice.missingSpeechServices) onMissingSpeech else actions.dictate
    }
    CompactEdgeButton(onClick = action, enabled = control.enabled, contentDescription = control.action.description) {
        when (control.action) {
            AssistantPrimaryAction.STOP -> ActionGlyph(AssistantActionIcon.STOP, control.action.description)
            AssistantPrimaryAction.SEND -> ActionGlyph(AssistantActionIcon.SEND, control.action.description)
            else -> InputGlyph(true, control.action.description)
        }
    }
}

@Composable
private fun LatestGlyph() {
    val color = LocalContentColor.current
    Canvas(Modifier.size(24.dp)) {
        val unit = size.width / 24f
        drawLine(color, Offset(12 * unit, 3 * unit), Offset(12 * unit, 16 * unit), 2 * unit)
        drawLine(color, Offset(6 * unit, 10 * unit), Offset(12 * unit, 16 * unit), 2 * unit)
        drawLine(color, Offset(18 * unit, 10 * unit), Offset(12 * unit, 16 * unit), 2 * unit)
        drawLine(color, Offset(5 * unit, 21 * unit), Offset(19 * unit, 21 * unit), 2 * unit)
    }
}

@Composable
private fun InputGlyph(microphone: Boolean, description: String) {
    val color = LocalContentColor.current
    Canvas(Modifier.size(CompactEdgeButtonDefaults.IconSize).semantics { contentDescription = description }) {
        val unit = size.width / 24f
        val stroke = Stroke(1.8f * unit)
        if (microphone) {
            drawRoundRect(color, Offset(9 * unit, 2 * unit), Size(6 * unit, 12 * unit), CornerRadius(3 * unit), style = stroke)
            drawPath(Path().apply {
                moveTo(6 * unit, 10 * unit); lineTo(6 * unit, 12 * unit)
                cubicTo(6 * unit, 20 * unit, 18 * unit, 20 * unit, 18 * unit, 12 * unit)
                lineTo(18 * unit, 10 * unit)
            }, color, style = stroke)
            drawLine(color, Offset(12 * unit, 18 * unit), Offset(12 * unit, 22 * unit), 1.8f * unit)
            drawLine(color, Offset(8 * unit, 22 * unit), Offset(16 * unit, 22 * unit), 1.8f * unit)
        } else {
            drawRoundRect(color, Offset(unit, 5 * unit), Size(22 * unit, 14 * unit), CornerRadius(2 * unit), style = stroke)
            for (row in 0..1) for (column in 0..4) drawRect(color,
                Offset((4 + column * 3.5f) * unit, (8 + row * 4) * unit), Size(1.8f * unit, 1.8f * unit))
            drawLine(color, Offset(8 * unit, 16 * unit), Offset(16 * unit, 16 * unit), 1.8f * unit)
        }
    }
}

private enum class AssistantActionIcon { SEND, STOP }

@Composable
private fun ActionGlyph(action: AssistantActionIcon, description: String) {
    val color = LocalContentColor.current
    Canvas(Modifier.size(CompactEdgeButtonDefaults.IconSize).semantics { contentDescription = description }) {
        val unit = size.width / 24f
        val stroke = Stroke(1.8f * unit)
        when (action) {
            AssistantActionIcon.STOP -> drawRect(color, Offset(5 * unit, 5 * unit), Size(14 * unit, 14 * unit))
            AssistantActionIcon.SEND -> {
                drawPath(Path().apply {
                    moveTo(3 * unit, 3 * unit); lineTo(21 * unit, 12 * unit)
                    lineTo(3 * unit, 21 * unit); lineTo(6 * unit, 12 * unit); close()
                }, color, style = stroke)
                drawLine(color, Offset(6 * unit, 12 * unit), Offset(21 * unit, 12 * unit), 1.8f * unit)
            }
        }
    }
}

internal fun assistantCanSend(state: AssistantState, voice: AssistantVoiceState): Boolean = state.foreground &&
    !state.busy && !voice.active && state.draft.isNotBlank() &&
    state.branchChoices.isEmpty() && state.model in AssistantModels.aliases

internal fun assistantRequestError(state: AssistantState): String? =
    if (state.model !in AssistantModels.aliases) "unsupported_model" else state.error

internal enum class AssistantPrimaryAction(val description: String) {
    STOP("Stop microphone, response and playback"), SEND("Send draft"),
    TALK("Talk continuously"), ASK("Ask once"),
}

internal data class AssistantPrimaryControl(val action: AssistantPrimaryAction, val enabled: Boolean)

internal fun assistantRepeatsAfterReply(voice: AssistantVoiceState, preferences: WearVoicePreferences): Boolean = when (voice.entryMode) {
    AssistantVoiceEntryMode.ASK -> false
    AssistantVoiceEntryMode.TALK -> true
    null -> preferences.conversationMode
}

internal fun assistantPrimaryControl(state: AssistantState, voice: AssistantVoiceState, conversationMode: Boolean): AssistantPrimaryControl = when {
    state.busy -> AssistantPrimaryControl(AssistantPrimaryAction.STOP, true)
    voice.listening -> AssistantPrimaryControl(AssistantPrimaryAction.STOP, true)
    voice.active -> AssistantPrimaryControl(AssistantPrimaryAction.STOP, true)
    voice.launchConfirmationRequired -> AssistantPrimaryControl(
        if (voice.entryMode == AssistantVoiceEntryMode.TALK) AssistantPrimaryAction.TALK else AssistantPrimaryAction.ASK,
        state.foreground && state.model in AssistantModels.aliases && state.branchChoices.isEmpty() && (voice.missingSpeechServices || (voice.admissionReady && voice.recognitionServiceAvailable)))
    state.draft.isNotBlank() -> AssistantPrimaryControl(AssistantPrimaryAction.SEND, assistantCanSend(state, voice))
    else -> AssistantPrimaryControl(if (conversationMode) AssistantPrimaryAction.TALK else AssistantPrimaryAction.ASK,
        state.foreground && state.model in AssistantModels.aliases && state.branchChoices.isEmpty() && (voice.missingSpeechServices || (voice.admissionReady && voice.recognitionServiceAvailable)))
}

internal fun assistantShowsLatest(
    state: AssistantState, voice: AssistantVoiceState, readingOlder: Boolean,
    measuredCanScrollForward: Boolean = true,
): Boolean = readingOlder && measuredCanScrollForward && !state.busy && !voice.active && !voice.launchConfirmationRequired

internal fun assistantProvisionalAnswer(state: AssistantState): String? = state.provisional.takeIf { state.busy && it.isNotBlank() }

internal fun assistantVisibleVoiceErrors(voice: AssistantVoiceState): List<String> =
    listOfNotNull(voice.error, voice.dictation.error, voice.speech.error, voice.loop.error).distinct()
        .filterNot { it == "recognition_error_7" || it == "no_speech" }

internal fun assistantToolActivity(state: AssistantState): String? = state.toolStatus.takeIf { state.busy }

internal data class AssistantSpeechSettings(
    val dictationStatus: String, val dictationDetails: String?, val replyStatus: String, val replyDetails: String?,
)

internal fun assistantSpeechSettings(voice: AssistantVoiceState, preferences: WearVoicePreferences): AssistantSpeechSettings {
    val dictation = voice.dictation.route
    val replies = voice.speech.route
    val dictationStatus = when {
        dictation == null -> "not selected"
        voice.listening -> "active selection"
        else -> "last selection"
    }
    val replyStatus = when {
        replies == null -> "not selected"
        voice.speech.phase == SpeechPhase.SPEAKING -> "active selection"
        else -> "last selection"
    }
    return AssistantSpeechSettings("Dictation · $dictationStatus", dictation?.let { "Dictation: ${it.description()}" },
        "Replies · $replyStatus" + if (preferences.readRepliesAloud) "" else " · disabled",
        replies?.let { "Replies: ${it.description()}" })
}

internal data class AssistantPendingPrompt(val key: String, val text: String)

internal class AssistantPromptPresentation {
    private data class Request(
        val entry: AssistantPendingPrompt, val token: String?, val conversation: String?, val head: String?, val settledHead: String? = null,
    )
    private var selection: Pair<String?, String?>? = null
    private var request: Request? = null
    private val submitted = mutableListOf<Request>()
    private var generation = 0L
    private var liveUtterance: Long? = null
    private var retiredUtterance: Long? = null
    private var lastUtterance: Long? = null
    private var livePrompt = ""

    fun beginRecognitionOwner() {
        liveUtterance = null
        retiredUtterance = null
        lastUtterance = null
        livePrompt = ""
    }

    fun project(state: AssistantState, voice: AssistantVoiceState, history: AssistantHistoryState): List<AssistantPendingPrompt> {
        val conversation = state.conversation?.let(::assistantIdKey)
        val head = state.selectedHead?.let(::assistantIdKey)
        val historyReady = history.conversation?.let(::assistantIdKey) == conversation &&
            history.head?.let(::assistantIdKey) == head && state.branchChoices.isEmpty()
        val selected = conversation to head
        if (state.clientRequestToken != null && request?.token != state.clientRequestToken) request = null
        val pending = request
        val settling = pending != null && pending.conversation == conversation && pending.head != head && !state.busy &&
            state.branchChoices.isEmpty() &&
            (state.error == null || state.error == "saved_local_state_failed")
        val creating = selection?.first == null && selection?.second == null && conversation != null && head == null &&
            state.busy && state.draft.isNotBlank()
        if (selection != null && selection != selected && !settling && !creating) {
            request = null
            submitted.clear()
            liveUtterance = null
            livePrompt = ""
            retiredUtterance = lastUtterance
        }
        if (creating) request = request?.copy(conversation = conversation)
        if (settling) {
            submitted += pending!!.copy(settledHead = head)
            request = null
        }
        selection = selected
        if (historyReady) {
            val canonicalIds = history.turns.map { assistantIdKey(it.id) }.toSet()
            submitted.removeAll { it.conversation == conversation && it.settledHead in canonicalIds }
        }
        lastUtterance = voice.dictation.utterance
        if (voice.listening && voice.dictation.utterance != retiredUtterance && !state.busy) {
            liveUtterance = voice.dictation.utterance
            livePrompt = voice.dictation.draft
        } else if (voice.dictation.phase == DictationPhase.FINAL && voice.dictation.utterance == liveUtterance) {
            livePrompt = voice.dictation.draft
        } else if (!voice.listening && voice.dictation.phase != DictationPhase.FINAL) {
            liveUtterance = null
            livePrompt = ""
        }
        if (state.busy) {
            if (request == null || request?.token != state.clientRequestToken) {
                request = Request(AssistantPendingPrompt(state.providerRequestId?.takeIf(String::isNotBlank)?.let(::assistantPromptItemKey)
                    ?: "pending-request:${++generation}", state.draft.ifBlank { livePrompt }),
                    state.clientRequestToken, conversation, head)
            }
            retiredUtterance = voice.dictation.utterance
            liveUtterance = null
            livePrompt = ""
            return (submitted + listOfNotNull(request)).map { it.entry }.filter { it.text.isNotBlank() }
        }
        request = null
        val nativeDraft = if (state.error == "saved_local_state_failed") "" else state.draft
        val prompt = if (voice.listening && liveUtterance != null) livePrompt else nativeDraft.ifBlank { livePrompt }
        val current = prompt.takeIf(String::isNotBlank)?.let {
            AssistantPendingPrompt(liveUtterance?.let { "pending-dictation:$it" } ?: "pending-draft", it)
        }
        if (!voice.listening && state.draft.isNotBlank()) {
            retiredUtterance = voice.dictation.utterance
            liveUtterance = null
            livePrompt = ""
        }
        return submitted.map { it.entry } + listOfNotNull(current)
    }
}

internal fun assistantTurnItemIndex(turns: List<AssistantTurn>, key: String): Int? {
    return AssistantTurnItemKeys(turns).index(key)
}

private fun assistantVisibleReadingAnchor(list: LazyListState, preferredKey: String? = null): Pair<String, Int>? = Snapshot.withoutReadObservation {
    val layout = list.layoutInfo
    (layout.visibleItemsInfo.firstOrNull { it.key == preferredKey } ?: layout.visibleItemsInfo.firstOrNull {
        it.key.toString().startsWith("prompt:") || it.key.toString().startsWith("answer:")
    })?.let { it.key.toString() to -it.offset }
}

internal data class AssistantHistoryScroll(
    val generation: Long, val index: Int?, val offset: Int = 0, val fallbackToLatest: Boolean = false,
)
internal data class AssistantProjectedReply(val requestKey: String?, val text: String, val firstTextMs: Long?) {
    val key: String get() = requestKey?.let(::assistantAnswerItemKey) ?: "stream"
}
internal data class AssistantHistoryTransition(
    val ready: Boolean, val turns: List<AssistantTurn>, val scroll: AssistantHistoryScroll? = null,
    val reply: AssistantProjectedReply? = null,
    val itemKeys: AssistantTurnItemKeys = AssistantTurnItemKeys(turns, reply?.requestKey),
    val retiredPromptKey: String? = null,
)
internal class AssistantHistoryPresentation {
    private data class ReplyOwner(
        val conversation: String?, val baseHead: String?, val token: String, val requestKey: String,
        val text: String, val firstTextMs: Long?, val settledHead: String? = null,
    )
    private var replyOwner: ReplyOwner? = null
    private var stoppedRequest by mutableStateOf<String?>(null)
    private var validatedSelection: Pair<String?, String?>? = null
    private var waiting = false
    private var wasReading = false
    private var savedAnchor: Pair<String, Int>? = null
    private var generation = 0L
    private var pendingScroll: AssistantHistoryScroll? = null
    private var keyedTurns: List<AssistantTurn> = emptyList()
    private var keyedRequest: String? = null
    private var cachedItemKeys = AssistantTurnItemKeys(keyedTurns)
    private var previousItemKeys = cachedItemKeys

    fun update(state: AssistantState, history: AssistantHistoryState, followsLatest: Boolean, anchor: Pair<String, Int>?): AssistantHistoryTransition {
        val selected = state.conversation?.let(::assistantIdKey) to state.selectedHead?.let(::assistantIdKey)
        val ready = history.conversation?.let(::assistantIdKey) == state.conversation?.let(::assistantIdKey) &&
            history.head?.let(::assistantIdKey) == state.selectedHead?.let(::assistantIdKey) && state.branchChoices.isEmpty()
        val requestKey = state.providerRequestId?.takeIf(String::isNotBlank)
        val token = state.clientRequestToken
        val previous = replyOwner
        val sameOwner = previous != null && previous.conversation == selected.first && previous.token == token &&
            previous.requestKey == requestKey
        val successful = !state.interrupted && (state.error == null || state.error == "saved_local_state_failed")
        replyOwner = when {
            !state.foreground || !successful || state.branchChoices.isNotEmpty() ||
                (stoppedRequest != null && requestKey == stoppedRequest) -> null
            state.busy && token != null && requestKey != null -> {
                if (sameOwner && previous.baseHead != selected.second) null
                else ReplyOwner(selected.first, selected.second, token, requestKey, state.provisional,
                    state.firstTextMs ?: previous?.takeIf { sameOwner }?.firstTextMs)
            }
            sameOwner && selected.second != null && previous.baseHead != selected.second &&
                (previous.settledHead == null || previous.settledHead == selected.second) -> previous.copy(settledHead = selected.second)
            else -> null
        }
        val owner = replyOwner
        // A witnessed local commit permits retaining only its already validated prefix, not an arbitrary fork.
        val retainPrefix = owner != null && owner.settledHead != null && owner.settledHead == selected.second &&
            validatedSelection == (owner.conversation to owner.baseHead) &&
            history.conversation?.let(::assistantIdKey) == owner.conversation && history.head?.let(::assistantIdKey) == owner.baseHead
        val canonicalTurn = if (ready && owner?.settledHead != null) history.turns.firstOrNull {
            assistantIdKey(it.id) == owner.settledHead && it.requestKey == owner.requestKey
        } else null
        val canonicalReply = canonicalTurn != null
        val reply = if (canonicalReply) null else owner?.takeIf { it.text.isNotBlank() || state.busy }?.let {
            AssistantProjectedReply(it.requestKey, it.text, it.firstTextMs)
        } ?: if (owner == null && (requestKey == null || token == null) && state.foreground && successful &&
            (stoppedRequest == null || requestKey != stoppedRequest) && state.branchChoices.isEmpty()) {
            assistantProvisionalAnswer(state)?.let { AssistantProjectedReply(requestKey, it, state.firstTextMs) }
        } else null
        val displayedTurns = if (ready || retainPrefix) history.turns else emptyList()
        val projectedRequestKey = if (state.busy || (owner != null && !canonicalReply)) requestKey else reply?.requestKey
        if (keyedTurns != displayedTurns || keyedRequest != projectedRequestKey) {
            keyedTurns = displayedTurns.toList()
            keyedRequest = projectedRequestKey
            cachedItemKeys = AssistantTurnItemKeys(keyedTurns, keyedRequest)
        }
        val itemKeys = cachedItemKeys
        val readingAnchor = anchor?.let { previousItemKeys.anchorKey(it.first) to it.second }
        val retiredPromptKey = if (ready && !state.busy && requestKey != null) history.turns.firstOrNull {
            assistantIdKey(it.id) == selected.second && it.requestKey == requestKey
        }?.requestKey?.let(::assistantPromptItemKey) else null
        if (canonicalReply) replyOwner = null
        if (!ready) {
            if (!waiting) {
                wasReading = validatedSelection != null && !followsLatest
                savedAnchor = if (wasReading) readingAnchor else null
                pendingScroll = null
            }
            waiting = true
            previousItemKeys = itemKeys
            return AssistantHistoryTransition(false, displayedTurns, reply = reply, itemKeys = itemKeys)
        }
        if (waiting || (validatedSelection != null && validatedSelection != selected)) {
            if (!waiting) {
                wasReading = !followsLatest
                savedAnchor = if (wasReading) readingAnchor else null
            }
            if (wasReading) {
                val key = savedAnchor?.first?.let { savedKey ->
                    when {
                        canonicalTurn != null && savedKey == canonicalTurn.requestKey?.let(::assistantPromptItemKey) -> itemKeys.prompt(canonicalTurn)
                        canonicalTurn != null && savedKey == canonicalTurn.requestKey?.let(::assistantAnswerItemKey) -> itemKeys.answer(canonicalTurn)
                        else -> savedKey
                    }
                }
                val index = key?.let(itemKeys::index)
                pendingScroll = AssistantHistoryScroll(++generation, index, if (index != null) savedAnchor!!.second else 0,
                    fallbackToLatest = index == null && canonicalTurn == null && owner?.settledHead == null)
            }
            waiting = false
            wasReading = false
            savedAnchor = null
        }
        validatedSelection = selected
        previousItemKeys = itemKeys
        return AssistantHistoryTransition(true, displayedTurns, pendingScroll, reply, itemKeys, retiredPromptKey)
    }

    fun stopReply() { stoppedRequest = replyOwner?.requestKey; replyOwner = null }

    fun scrolled(scroll: AssistantHistoryScroll) {
        if (pendingScroll == scroll) pendingScroll = null
    }
}

internal data class AssistantChat(val id: String, val title: String)
internal fun assistantChats(view: String): List<AssistantChat> = assistantChats(JSONObject(view))
private fun assistantChats(view: JSONObject): List<AssistantChat> {
    val conversations = view.getJSONArray("conversations")
    return (0 until conversations.length()).map { conversations.getJSONObject(it) }
        .filter { !it.optBoolean("deleted") && !it.isNull("title") }
        .map { AssistantChat(it.getJSONObject("id").toString(), it.getString("title")) }
}

/** Models this device can actually run; the watch has no Agy executable for its ARM32 userspace. */
internal fun assistantSelectableModels(readiness: Map<String, OptionalProviderReadiness>, selected: String): List<String> =
    AssistantModels.aliases.filter { it == selected || it in AssistantModels.customLabels || readiness[it]?.available == true }

internal fun assistantBranchLabel(view: String, id: String): String = assistantBranchLabel(JSONObject(view), id)
private fun assistantBranchLabel(view: JSONObject, id: String): String {
    val targetKey = assistantIdKey(id)
    val previews = view.optJSONArray("branch_previews") ?: view.optJSONArray("head_previews")
    if (previews != null) {
        val preview = (0 until previews.length()).map { previews.getJSONObject(it) }
            .firstOrNull { assistantIdKey(it.getJSONObject("id").toString()) == targetKey }
            ?.optString("prompt_preview")
            .orEmpty()
            .replace(Regex("\\s+"), " ")
            .trim()
        if (preview.isNotEmpty()) return "Continue: ${preview.take(100)}"
    }
    val records = view.optJSONArray("records")
    if (records != null) {
        val operation = (0 until records.length()).map { records.getJSONObject(it).getJSONObject("operation") }
            .firstOrNull { assistantIdKey(it.getJSONObject("id").toString()) == targetKey }
        val prompt = operation?.optJSONObject("body")?.optString("prompt").orEmpty().replace('\n', ' ').trim()
        if (prompt.isNotBlank()) return "Continue: ${prompt.take(100)}"
    }
    return "Continuation awaiting messages"
}
