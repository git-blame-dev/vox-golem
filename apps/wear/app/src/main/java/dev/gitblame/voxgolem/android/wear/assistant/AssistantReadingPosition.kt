package dev.gitblame.voxgolem.android.wear.assistant

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.Saver
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import kotlin.math.abs

internal fun assistantRequestOwnerKey(state: AssistantState): String? =
    state.clientRequestToken?.takeIf(String::isNotBlank) ?: state.providerRequestId?.takeIf(String::isNotBlank)

internal fun assistantReadingRequestKey(state: AssistantState): String? =
    if (state.busy) assistantRequestOwnerKey(state) else null

internal class AssistantChatScreenIdentity {
    private var initialized = false
    private var conversation: String? = null
    private var requestOwner: String? = null
    private var priorConversationRequest: String? = null
    private var epoch = 0L

    fun key(state: AssistantState): Long {
        val selected = state.conversation?.let(::assistantIdKey)
        val request = assistantRequestOwnerKey(state)
        if (initialized) {
            val firstRequestPromotion = conversation == null && selected != null && request != null &&
                request != priorConversationRequest && (requestOwner == null || requestOwner == request)
            val clearedUncreatedRequest = conversation == null && selected == null && requestOwner != null && request == null
            if ((conversation != selected && !firstRequestPromotion) || clearedUncreatedRequest) epoch++
        }
        initialized = true
        if (selected != null && request != null) priorConversationRequest = request
        conversation = selected
        requestOwner = if (selected == null && request == priorConversationRequest) null else request
        return epoch
    }

    companion object {
        val Saver = Saver<AssistantChatScreenIdentity, List<Any>>(
            save = { listOf(it.initialized, it.conversation.orEmpty(), it.requestOwner.orEmpty(), it.epoch,
                it.priorConversationRequest.orEmpty()) },
            restore = { saved -> AssistantChatScreenIdentity().apply {
                initialized = saved[0] as Boolean
                conversation = (saved[1] as String).ifEmpty { null }
                requestOwner = (saved[2] as String).ifEmpty { null }
                epoch = saved[3] as Long
                priorConversationRequest = (saved.getOrNull(4) as? String)?.ifEmpty { null }
            } },
        )
    }
}

internal fun assistantReplyReadingCap(viewportHeight: Int, topPadding: Int, bottomPadding: Int): Int =
    ((viewportHeight - topPadding - bottomPadding).coerceAtLeast(0) * 0.15f).toInt()

internal data class AssistantReplyGeometry(
    val rowOffset: Int?, val rowSize: Int?, val viewportHeight: Int,
    val topPadding: Int, val bottomPadding: Int, val canScrollForward: Boolean,
    val itemCount: Int,
) {
    val relativeCap: Int get() = assistantReplyReadingCap(viewportHeight, topPadding, bottomPadding)
    // Compose 1.10 LazyListItemInfo.offset excludes the visual content-padding offset.
    val targetOffset: Int get() = relativeCap
}

internal enum class AssistantReplyFollowDecision { NONE, POSITION, HOLD }
internal enum class AssistantReadingPhase { INITIAL, ARMED, HELD, MANUAL }

internal class AssistantReadingPosition(followLatest: Boolean = true) {
    var phase by mutableStateOf(if (followLatest) AssistantReadingPhase.INITIAL else AssistantReadingPhase.MANUAL)
        private set
    var currentRequestKey: String? by mutableStateOf(null)
        private set
    private var providerRequestKey: String? = null
    var replyRowKey: String? = null
        private set
    private var eligibleNext = followLatest
    private var manualScrolling = false
    private var visualEnd by mutableStateOf(false)
    private var lastGeometry: AssistantReplyGeometry? = null
    val followsLatest: Boolean get() = phase == AssistantReadingPhase.INITIAL || phase == AssistantReadingPhase.ARMED
    val readingOlder: Boolean get() = !followsLatest && !visualEnd
    private var following = 0
    var isFollowing by mutableStateOf(false)
        private set

    fun observeRequest(requestKey: String, manuallyScrolling: Boolean, providerKey: String? = null) {
        val promotingProviderIdentity = providerKey != null && providerKey == providerRequestKey &&
            currentRequestKey == providerRequestKey && requestKey != providerKey
        if (currentRequestKey == requestKey || promotingProviderIdentity) {
            currentRequestKey = requestKey
            providerRequestKey = providerKey ?: providerRequestKey
            return
        }
        currentRequestKey = requestKey
        providerRequestKey = providerKey
        // Eligibility belongs to the previous reading intent, not geometry after this
        // request has appended its own potentially overflowing prompt.
        phase = if (eligibleNext && !manuallyScrolling) AssistantReadingPhase.ARMED else AssistantReadingPhase.HELD
        visualEnd = false
        replyRowKey = null
        lastGeometry = null
    }

    fun trackReplyRow(key: String) { replyRowKey = key; lastGeometry = null }

    fun completed(canScrollForward: Boolean = false) {
        if (phase == AssistantReadingPhase.ARMED) {
            phase = AssistantReadingPhase.HELD
            val atEnd = eligibleNext && !canScrollForward
            visualEnd = atEnd
            eligibleNext = atEnd
        }
    }

    fun scrolled(inProgress: Boolean, canScrollForward: Boolean) {
        if (isFollowing) return
        if (inProgress) {
            readOlder(canScrollForward)
            manualScrolling = true
            eligibleNext = !canScrollForward
        } else if (manualScrolling) {
            manualScrolling = false
            eligibleNext = !canScrollForward
        }
        // Measured end position controls the dock, never current/next follow intent.
        if (!followsLatest) visualEnd = !canScrollForward
    }

    fun readOlder(canScrollForward: Boolean = true) {
        phase = AssistantReadingPhase.MANUAL
        visualEnd = !canScrollForward
        eligibleNext = !canScrollForward
        replyRowKey = null
    }

    fun latest() {
        phase = AssistantReadingPhase.HELD
        visualEnd = true
        eligibleNext = true
        replyRowKey = null
    }

    fun followDecision(geometry: AssistantReplyGeometry): AssistantReplyFollowDecision {
        if (!followsLatest || lastGeometry == geometry) return AssistantReplyFollowDecision.NONE
        lastGeometry = geometry
        if (geometry.viewportHeight <= geometry.topPadding + geometry.bottomPadding) return AssistantReplyFollowDecision.NONE
        val offset = geometry.rowOffset
        return when {
            offset == null -> AssistantReplyFollowDecision.POSITION
            abs(offset - geometry.targetOffset) <= 1 -> AssistantReplyFollowDecision.HOLD
            offset < geometry.targetOffset || geometry.canScrollForward -> AssistantReplyFollowDecision.POSITION
            else -> AssistantReplyFollowDecision.NONE
        }
    }

    fun replyPositioned(rowOffset: Int, cap: Int, canScrollForward: Boolean = true) {
        if (rowOffset <= cap + 1) {
            phase = AssistantReadingPhase.HELD
            visualEnd = !canScrollForward
            eligibleNext = !canScrollForward
        }
    }

    suspend fun following(action: suspend () -> Unit) {
        following++
        isFollowing = true
        try { action() } finally { following--; isFollowing = following > 0 }
    }

    companion object {
        val Saver = Saver<AssistantReadingPosition, Any>(
            save = { listOf(it.phase.name, it.currentRequestKey.orEmpty(), it.eligibleNext, it.visualEnd,
                it.replyRowKey.orEmpty(), it.providerRequestKey.orEmpty()) },
            restore = { saved -> when (saved) {
                is Boolean -> AssistantReadingPosition(saved)
                is List<*> -> if (saved.firstOrNull() is Boolean) {
                    AssistantReadingPosition(saved[0] as Boolean).apply { visualEnd = saved.getOrNull(1) == true }
                } else AssistantReadingPosition().apply {
                    phase = AssistantReadingPhase.valueOf(saved[0] as String)
                    currentRequestKey = (saved[1] as String).ifEmpty { null }
                    eligibleNext = saved[2] as Boolean
                    visualEnd = saved[3] as Boolean
                    replyRowKey = (saved[4] as String).ifEmpty { null }
                    providerRequestKey = (saved.getOrNull(5) as? String)?.ifEmpty { null }
                }
                else -> null
            } },
        )
    }
}
