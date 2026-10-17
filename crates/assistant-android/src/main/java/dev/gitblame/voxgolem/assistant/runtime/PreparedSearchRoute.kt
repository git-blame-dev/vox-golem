package dev.gitblame.voxgolem.assistant.runtime

import android.os.SystemClock
import android.util.Log
import org.json.JSONObject

internal data class PreparedSearchResult(
    val evidenceContext: String?,
    val sourceFooter: String,
    val needsEscalation: Boolean,
    val route: String = "F",
    val searchMs: Long = 0L,
    val sourcesCount: Int = 0,
    val evidenceBytes: Int = 0,
)

internal object PreparedSearchRoute {
    private const val TAG = "PreparedSearchRoute"
    const val SOURCE_FOOTER_MARKER = "\n\nSources:\n"

    private val SELF_IDENTITY_QUERY = Regex(
        """^(?:what\s+model\s+are\s+you|who\s+are\s+you)\b""",
        RegexOption.IGNORE_CASE
    )
    private val LIVE_OVERRIDE_KEYWORDS = Regex(
        """\b(search|google|look up|lookup|weather|forecast|temperature|news|headline|headlines|stock|price|score|scores|standings|who won|today|tonight|yesterday|tomorrow|latest|current|currently|right now|recent|recently|2025|2026|2027|release date|latest version|current version|outage|check online|search online|official docs|officially|unofficially)\b""",
        RegexOption.IGNORE_CASE
    )
    private val CODING_DIRECT_KEYWORDS = Regex(
        """\b(rust|python|javascript|typescript|golang|compile|compiler|panic|stack trace|traceback|ring buffer|regex|algorithm|big o|database schema|refactor|derivative|integral|matrix multiplication)\b""",
        RegexOption.IGNORE_CASE
    )
    private val QUICK_SEARCH_KEYWORDS = Regex(
        """\b(who is|what is the|what are the|when is|when did|when was|where is|how much|how many|official|unofficial|community|online|internet|reddit|github|docs|documentation|specs|specifications|release|version|schedule|update|status|worth|cost|rate|winner|champion|leader|ceo of|president of|prime minister|minister|governor|mayor|population|capital of|distance|calories|recipe|ingredients|symptoms|treatment|law|policy)\b""",
        RegexOption.IGNORE_CASE
    )
    private val FILLER_PREFIX = Regex(
        "^(?:um|uh|er|ah|like)(?:[\\s\\p{Punct}]+|$)",
        RegexOption.IGNORE_CASE
    )
    private val PREFIX_COLON_SUPPLIED_TEXT = Regex(
        """^(?:can you\s+)?(?:please\s+)?(?:translate|summarize|summarise|rewrite|proofread|fix(?:\s+the)?\s+(?:typos|grammar|spelling)|extract)\b[^:]{0,60}:\s+.+""",
        setOf(RegexOption.IGNORE_CASE, RegexOption.DOT_MATCHES_ALL)
    )
    private val TAIL_SUPPLIED_TEXT = Regex(
        """\b(?:summarize|summarise|translate|rewrite|proofread|paraphrase|simplify|fix\s+the\s+(?:typos|grammar|spelling))\s+(?:the\s+)?(?:above|this|preceding|following|text|passage|article)\b""",
        RegexOption.IGNORE_CASE
    )
    private val ELLIPTICAL_FOLLOW_UP = Regex(
        """^(?:and\s+)?(?:what\s+about|how\s+about|what\s+if|and\s+what|what\s+was\s+the|are\s+there\s+any)\b""",
        RegexOption.IGNORE_CASE
    )
    private val DE_ESCALATING_COMMAND = Regex(
        """\b(summarize|summarise|translate|rewrite|shorter|bullet points|thanks|thank you)\b""",
        RegexOption.IGNORE_CASE
    )

    fun stripSourceFooter(text: String): String {
        val idx = text.indexOf(SOURCE_FOOTER_MARKER)
        return if (idx >= 0) text.substring(0, idx).trimEnd() else text
    }

    fun stripLeadingFillers(text: String): String {
        var t = text.trim()
        while (true) {
            val match = FILLER_PREFIX.find(t) ?: break
            t = t.substring(match.range.last + 1).trim()
        }
        return t
    }

    fun isolateLongPasteInstruction(text: String): String {
        if (text.length <= 260) {
            return text
        }
        if (text.contains("\n\n")) {
            val parts = text.split("\n\n")
                .map { it.trim() }
                .filter { it.isNotEmpty() }
            if (parts.size >= 2 && parts.last().length < 260) {
                return parts.last()
            }
        }
        val firstNl = text.indexOf('\n')
        if (firstNl in 15..160) {
            return text.substring(0, firstNl).trim()
        }
        val head = if (text.length > 110) text.take(110) else text
        val tail = if (text.length > 140) text.takeLast(140) else text
        return "$head ... $tail"
    }

    fun extractUserQuery(reqObj: JSONObject): String {
        val direct = reqObj.optString("prompt", "").trim()
        if (direct.isNotEmpty()) return direct
        val messages = reqObj.optJSONArray("messages") ?: return ""
        for (i in messages.length() - 1 downTo 0) {
            val msg = messages.optJSONObject(i) ?: continue
            val role = msg.optString("role", "user").trim()
            if (role.equals("user", ignoreCase = true)) {
                val content = msg.optString("content", "").trim()
                if (content.isNotEmpty()) return content
            }
        }
        return ""
    }

    fun extractPreviousUserQuery(reqObj: JSONObject, currentQuery: String): String? {
        val messages = reqObj.optJSONArray("messages") ?: return null
        val trimmedCurrent = currentQuery.trim()
        var skippedCurrent = false
        for (i in messages.length() - 1 downTo 0) {
            val msg = messages.optJSONObject(i) ?: continue
            val role = msg.optString("role", "user").trim()
            if (role.equals("user", ignoreCase = true)) {
                val rawContent = msg.optString("content", "").trim()
                val content = stripSourceFooter(rawContent).trim()
                if (content.isEmpty()) continue
                if (!skippedCurrent && (rawContent == trimmedCurrent || content == trimmedCurrent)) {
                    skippedCurrent = true
                    continue
                }
                return content
            }
        }
        return null
    }

    fun shouldRunWebSearch(
        webEnabled: Boolean,
        searchMode: String,
        query: String,
        prevUserQuery: String? = null
    ): Boolean {
        if (!webEnabled || query.isBlank()) return false
        return when (searchMode.trim().lowercase()) {
            "off", "disabled", "none" -> false
            "verify", "research" -> true
            "quick", "" -> {
                var candidate = stripLeadingFillers(query)
                if (candidate.isBlank()) return false

                if (PREFIX_COLON_SUPPLIED_TEXT.containsMatchIn(candidate)) {
                    return false
                }
                if (candidate.length > 180 && TAIL_SUPPLIED_TEXT.containsMatchIn(candidate.takeLast(240))) {
                    return false
                }

                if (!prevUserQuery.isNullOrBlank()
                    && ELLIPTICAL_FOLLOW_UP.containsMatchIn(candidate)
                    && !DE_ESCALATING_COMMAND.containsMatchIn(candidate)
                ) {
                    candidate = "${prevUserQuery.trim()} $candidate"
                }

                val isolated = isolateLongPasteInstruction(candidate)
                if (SELF_IDENTITY_QUERY.containsMatchIn(isolated)) {
                    return false
                }
                if (LIVE_OVERRIDE_KEYWORDS.containsMatchIn(isolated)) {
                    return true
                }
                if (CODING_DIRECT_KEYWORDS.containsMatchIn(isolated)) {
                    return false
                }
                QUICK_SEARCH_KEYWORDS.containsMatchIn(isolated)
            }
            else -> false
        }
    }

    fun prepare(
        requestId: String,
        reqObj: JSONObject,
        searchNative: ProviderNativeCalls?,
        callback: ProviderDeltaCallback,
        isCancelled: () -> Boolean,
    ): PreparedSearchResult {
        val webEnabled = reqObj.optBoolean("webEnabled", reqObj.optBoolean("web_enabled", false))
        val searchMode = reqObj.optString("searchMode", reqObj.optString("web_search_mode", "quick"))
        val userQuery = extractUserQuery(reqObj)
        val prevUserQuery = extractPreviousUserQuery(reqObj, userQuery)

        if (!shouldRunWebSearch(webEnabled, searchMode, userQuery, prevUserQuery) || searchNative == null || isCancelled()) {
            return PreparedSearchResult(null, "", false)
        }

        val prepareReq = JSONObject(reqObj.toString()).put("requestId", requestId).put("enforceQuickRouter", true)
        val startNanos = runCatching { SystemClock.elapsedRealtimeNanos() }.getOrDefault(0L)
        val prepJson = try {
            searchNative.prepareSearch(prepareReq.toString(), callback)
        } catch (_: Throwable) {
            Log.w(TAG, "stage=search_prepare_failed")
            null
        }
        val endNanos = runCatching { SystemClock.elapsedRealtimeNanos() }.getOrDefault(startNanos)
        val searchMs = ((endNanos - startNanos).coerceAtLeast(0L)) / 1_000_000L

        if (isCancelled() || prepJson.isNullOrBlank()) {
            return PreparedSearchResult(null, "", false, searchMs = searchMs)
        }

        return try {
            val resObj = JSONObject(prepJson)
            val rawContext = if (!resObj.isNull("evidenceContext")) resObj.optString("evidenceContext", "").trim() else null
            val evidenceContext = if (rawContext.isNullOrBlank() || rawContext == "null") null else rawContext
            val sourceFooter = resObj.optString("sourceFooter", "")
            val needsEscalation = resObj.optBoolean("needsEscalation", false)
            val route = resObj.optString("route", "S").ifBlank { "S" }
            val sourcesCount = resObj.optJSONArray("sources")?.length() ?: 0
            val evidenceBytes = evidenceContext?.toByteArray(Charsets.UTF_8)?.size ?: 0
            PreparedSearchResult(evidenceContext, sourceFooter, needsEscalation, route, searchMs, sourcesCount, evidenceBytes)
        } catch (_: Throwable) {
            Log.w(TAG, "stage=search_result_invalid")
            PreparedSearchResult(null, "", false, searchMs = searchMs)
        }
    }
}
