package dev.gitblame.voxgolem.android.wear.assistant

import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import org.json.JSONObject

internal data class AssistantTurn(
    val id: String, val prompt: String, val answer: String, val refusal: Boolean,
    val requestKey: String? = null,
)
internal fun assistantPromptItemKey(requestKey: String): String = "prompt:request:$requestKey"
internal fun assistantAnswerItemKey(requestKey: String): String = "answer:request:$requestKey"
internal fun assistantPromptItemKey(turn: AssistantTurn, collisions: Set<String> = emptySet()): String =
    turn.requestKey?.takeUnless { it in collisions }?.let(::assistantPromptItemKey) ?: "prompt:${assistantIdKey(turn.id)}"
internal fun assistantAnswerItemKey(turn: AssistantTurn, collisions: Set<String> = emptySet()): String =
    turn.requestKey?.takeUnless { it in collisions }?.let(::assistantAnswerItemKey) ?: "answer:${assistantIdKey(turn.id)}"

/** Derived once for the displayed page; request identity is not durable turn uniqueness. */
internal class AssistantTurnItemKeys(turns: List<AssistantTurn>, projectedRequestKey: String? = null) {
    private val collisions = buildSet {
        val seen = mutableSetOf<String>()
        turns.forEach { turn -> turn.requestKey?.let { if (!seen.add(it)) add(it) } }
        if (projectedRequestKey != null && projectedRequestKey in seen) add(projectedRequestKey)
    }
    private val indices = mutableMapOf<String, Int>()
    private val anchors = mutableMapOf<String, String>()
    val rowKeys: Set<String>

    init {
        val keys = mutableSetOf<String>()
        turns.forEachIndexed { index, turn ->
            val id = assistantIdKey(turn.id)
            val prompt = prompt(turn)
            val answer = answer(turn)
            keys += prompt
            keys += answer
            indices[prompt] = 1 + index * 2
            indices[answer] = 2 + index * 2
            // Reading anchors keep operation identity even if later paging discovers a request collision.
            indices["prompt:$id"] = 1 + index * 2
            indices["answer:$id"] = 2 + index * 2
            anchors[prompt] = "prompt:$id"
            anchors[answer] = "answer:$id"
        }
        rowKeys = keys
    }

    fun prompt(turn: AssistantTurn): String = assistantPromptItemKey(turn, collisions)
    fun answer(turn: AssistantTurn): String = assistantAnswerItemKey(turn, collisions)
    fun index(key: String): Int? = indices[key]
    fun anchorKey(key: String): String = anchors[key] ?: key
}

internal data class AssistantHistoryState(
    val conversation: String? = null,
    val head: String? = null,
    val turns: List<AssistantTurn> = emptyList(),
    val olderBefore: String? = null,
    val loading: Boolean = false,
    val error: String? = null,
)

/** UI page ownership only. Native history supplies ordering, ancestry and record eligibility. */
internal class AssistantHistory(
    private val request: (String?, (String?, AssistantState) -> Unit) -> Unit,
    initial: AssistantHistoryState = AssistantHistoryState(),
    private val changed: (AssistantHistoryState) -> Unit,
) {
    var snapshot = initial
        private set
    private data class Selection(val conversation: String?, val head: String?, val revision: String, val unresolved: Boolean)
    private var selection: Selection? = null
    private var epoch = 0L
    private var active = false
    private var enabled = false
    private var reload = false
    private var requestNumber = 0L
    private val cursors = mutableSetOf<String>()

    fun select(state: AssistantState) {
        val next = Selection(state.conversation?.let(::assistantIdKey), state.selectedHead?.let(::assistantIdKey),
            JSONObject(state.viewJson).getString("revision"), state.branchChoices.isNotEmpty())
        enabled = true
        if (next != selection) {
            epoch++
            val sameConversation = next.conversation == (selection?.conversation ?: snapshot.conversation?.let(::assistantIdKey))
            selection = next
            cursors.clear()
            reload = next.conversation != null && !next.unresolved
            publish(if (sameConversation && !next.unresolved) snapshot.copy(error = null, loading = reload)
                else AssistantHistoryState(conversation = state.conversation, head = state.selectedHead, loading = reload))
        }
        pump()
    }

    fun suspendReads() { enabled = false; epoch++; reload = selection?.conversation != null && selection?.unresolved == false }
    fun retry() { if (!active) { epoch++; cursors.clear(); reload = true; pump() } }
    fun older() {
        val cursor = snapshot.olderBefore ?: return
        if (!enabled || active || reload || snapshot.error != null) return
        load(cursor)
    }

    private fun pump() {
        if (enabled && !active && reload && selection?.conversation != null && selection?.unresolved == false) {
            reload = false
            load(null)
        }
    }

    private fun load(before: String?) {
        val selected = selection ?: return
        val token = epoch
        val number = ++requestNumber
        var completed = false
        active = true
        publish(snapshot.copy(loading = true, error = null))
        request(before) { raw, settled ->
            if (completed || number != requestNumber) return@request
            completed = true
            active = false
            if (enabled && token == epoch && selected == selection) {
                val settledMatches = settled.conversation?.let(::assistantIdKey) == selected.conversation &&
                    settled.selectedHead?.let(::assistantIdKey) == selected.head && settled.branchChoices.isEmpty() &&
                    JSONObject(settled.viewJson).getString("revision") == selected.revision
                if (!settledMatches) {
                    publish(snapshot.copy(loading = false, error = "History changed. Recheck to load the selected continuation."))
                } else try {
                    require(raw != null)
                    val page = JSONObject(raw)
                    require(assistantIdKey(page.getJSONObject("conversation").toString()) == selected.conversation)
                    require(page.nullableObject("head")?.let(::assistantIdKey) == selected.head)
                    require(page.getString("revision") == selected.revision)
                    val nextCursor = if (page.isNull("older_before")) null else page.getString("older_before")
                    require(nextCursor == null || (nextCursor != before && nextCursor !in cursors))
                    val records = page.getJSONArray("turns")
                    val turns = (0 until records.length()).map { index ->
                        val operation = records.getJSONObject(index).getJSONObject("operation")
                        val body = operation.getJSONObject("body")
                        require(body.getString("kind") == "turn_completed")
                        require(assistantIdKey(body.getJSONObject("conversation").toString()) == selected.conversation)
                        val answer = body.getJSONObject("answer")
                        val refusal = answer.getString("kind") == "refusal"
                        require(refusal || answer.getString("kind") == "text")
                        AssistantTurn(operation.getJSONObject("id").toString(), body.getString("prompt"),
                            answer.getString(if (refusal) "reason" else "text"), refusal, body.assistantRequestKey())
                    }
                    require(turns.map { assistantIdKey(it.id) }.distinct().size == turns.size)
                    val oldById = snapshot.turns.associateBy { assistantIdKey(it.id) }
                    turns.forEach { turn -> oldById[assistantIdKey(turn.id)]?.let { require(it.prompt == turn.prompt && it.answer == turn.answer && it.refusal == turn.refusal) } }
                    val merged = if (before != null) {
                        if (nextCursor != null) require(turns.isNotEmpty())
                        turns.filter { assistantIdKey(it.id) !in oldById } + snapshot.turns
                    } else {
                        val oldHead = snapshot.head?.let(::assistantIdKey)
                        val intersection = turns.indexOfFirst { assistantIdKey(it.id) == oldHead }
                        if (intersection >= 0 && snapshot.turns.isNotEmpty()) snapshot.turns + turns.drop(intersection + 1)
                        else turns
                    }
                    before?.let(cursors::add)
                    publish(AssistantHistoryState(page.getJSONObject("conversation").toString(), page.nullableObject("head"), merged, nextCursor))
                } catch (_: Exception) {
                    publish(snapshot.copy(loading = false, error = settled.error?.let(::assistantErrorText)
                        ?: "History unavailable. Recheck or retry; your draft is retained."))
                }
            }
            pump()
        }
    }

    private fun publish(value: AssistantHistoryState) { snapshot = value; changed(value) }
}

internal fun assistantIdKey(value: String): String = JSONObject(value).let { "${it.getString("replica")}:${it.getString("sequence")}" }
private fun JSONObject.assistantRequestKey(): String? {
    val request = optJSONObject("request") ?: return null
    val replica = request.opt("replica") as? String ?: return null
    val sequence = request.opt("sequence") as? String ?: return null
    return if (replica.isNotBlank() && sequence.isNotBlank()) "$replica:$sequence" else null
}
private fun JSONObject.nullableObject(name: String): String? = if (isNull(name)) null else getJSONObject(name).toString()
