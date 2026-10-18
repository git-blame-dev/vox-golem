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
    val fallbackUsed: Boolean = false,
    val searchStatus: String = "skipped",
)

internal object PreparedSearchRoute {
    private const val TAG = "PreparedSearchRoute"
    const val SOURCE_FOOTER_MARKER = "\n\nSources:\n"

    fun stripSourceFooter(text: String): String {
        val idx = text.indexOf(SOURCE_FOOTER_MARKER)
        return if (idx >= 0) text.substring(0, idx).trimEnd() else text
    }

    fun shouldRunWebSearch(
        webEnabled: Boolean,
        searchMode: String,
        query: String,
        prevUserQuery: String? = null
    ): Boolean {
        if (!webEnabled || query.isBlank()) return false
        val systemLoader = ClassLoader.getSystemClassLoader()
        if (systemLoader != null && PreparedSearchRoute::class.java.classLoader != systemLoader && System.getProperty("assistant.native.library") != null) {
            return runCatching {
                val cls = systemLoader.loadClass("dev.gitblame.voxgolem.assistant.runtime.AssistantProviderNative")
                val instance = cls.getField("INSTANCE").get(null)
                val method = cls.getMethod(
                    "shouldRunWebSearch",
                    Boolean::class.javaPrimitiveType,
                    String::class.java,
                    String::class.java,
                    String::class.java,
                )
                method.invoke(instance, webEnabled, searchMode, query, prevUserQuery) as Boolean
            }.getOrElse {
                AssistantProviderNative.shouldRunWebSearch(webEnabled, searchMode, query, prevUserQuery)
            }
        }
        return AssistantProviderNative.shouldRunWebSearch(webEnabled, searchMode, query, prevUserQuery)
    }

    fun routePrompt2d(
        query: String,
        prevUserQuery: String? = null,
    ): String {
        if (query.isBlank()) return "F"
        val systemLoader = ClassLoader.getSystemClassLoader()
        if (systemLoader != null && PreparedSearchRoute::class.java.classLoader != systemLoader && System.getProperty("assistant.native.library") != null) {
            return runCatching {
                val cls = systemLoader.loadClass("dev.gitblame.voxgolem.assistant.runtime.AssistantProviderNative")
                val instance = cls.getField("INSTANCE").get(null)
                val method = cls.getMethod(
                    "routePrompt2d",
                    String::class.java,
                    String::class.java,
                )
                method.invoke(instance, query, prevUserQuery) as String
            }.getOrElse {
                AssistantProviderNative.routePrompt2d(query, prevUserQuery)
            }
        }
        return AssistantProviderNative.routePrompt2d(query, prevUserQuery)
    }

    fun prepare(
        requestId: String,
        reqObj: JSONObject,
        searchNative: ProviderNativeCalls?,
        callback: ProviderDeltaCallback,
        isCancelled: () -> Boolean,
    ): PreparedSearchResult {
        if (searchNative == null || isCancelled()) {
            return PreparedSearchResult(null, "", false, searchStatus = if (isCancelled()) "cancelled" else "skipped")
        }
        val webEnabled = reqObj.optBoolean("webEnabled", reqObj.optBoolean("web_enabled", false))
        if (!webEnabled) {
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
            return PreparedSearchResult(
                null,
                "",
                false,
                searchMs = searchMs,
                searchStatus = if (isCancelled()) "cancelled" else "failed",
            )
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
            val fallbackUsed = resObj.optBoolean("fallbackUsed", false)
            val searchStatus = resObj.optString("statusCode", if (sourcesCount > 0) "ready" else "empty")
            PreparedSearchResult(
                evidenceContext = evidenceContext,
                sourceFooter = sourceFooter,
                needsEscalation = needsEscalation,
                route = route,
                searchMs = searchMs,
                sourcesCount = sourcesCount,
                evidenceBytes = evidenceBytes,
                fallbackUsed = fallbackUsed,
                searchStatus = searchStatus,
            )
        } catch (_: Throwable) {
            Log.w(TAG, "stage=search_result_invalid")
            PreparedSearchResult(null, "", false, searchMs = searchMs, searchStatus = "failed")
        }
    }
}
