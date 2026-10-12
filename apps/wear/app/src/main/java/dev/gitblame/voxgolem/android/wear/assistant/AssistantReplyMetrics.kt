package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONObject

/** Wear-local HTTP-dispatch-to-first-forwarded-nonwhitespace-text durations, not UI/render timing. */
internal class AssistantReplyMetrics(private val preferences: SharedPreferences) {
    constructor(context: Context) : this(context.applicationContext.getSharedPreferences("assistant-reply-metrics", Context.MODE_PRIVATE))

    private val values = linkedMapOf<String, Long>()

    init {
        val entries = runCatching { JSONArray(preferences.getString("entries", null)) }.getOrNull()
        if (entries != null) for (index in 0 until entries.length()) {
            val entry = entries.optJSONObject(index) ?: continue
            val key = entry.opt("request_key") as? String ?: continue
            val milliseconds = when (val value = entry.opt("first_text_ms")) {
                is Long -> value
                is Int -> value.toLong()
                else -> continue
            }
            if (key.isNotBlank() && milliseconds >= 0 && key !in values) {
                values[key] = milliseconds
                trim()
            }
        }
    }

    fun get(requestKey: String?): Long? = requestKey?.let(values::get)

    fun remember(requestKey: String?, firstTextMs: Long?) {
        if (requestKey.isNullOrBlank() || firstTextMs == null || firstTextMs < 0 || requestKey in values) return
        values[requestKey] = firstTextMs
        trim()
        val entries = JSONArray(values.map { (key, milliseconds) ->
            JSONObject().put("request_key", key).put("first_text_ms", milliseconds)
        })
        preferences.edit().putString("entries", entries.toString()).apply()
    }

    private fun trim() { while (values.size > 256) values.remove(values.keys.first()) }
}
