package dev.gitblame.voxgolem.android.wear.assistant

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.UUID

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class AssistantReplyMetricsTest {
    private val context = RuntimeEnvironment.getApplication()
    private val name = "synthetic-reply-metrics-${UUID.randomUUID()}"
    private fun preferences() = context.getSharedPreferences(name, Context.MODE_PRIVATE)
    private fun reopen() = AssistantReplyMetrics(preferences())
    private fun entry(key: Any, milliseconds: Any) = JSONObject()
        .put("request_key", key).put("first_text_ms", milliseconds)
    private fun seed(value: String) { assertTrue(preferences().edit().putString("entries", value).commit()) }

    @Test fun contextConstructorReopensItsDedicatedSidecarWithoutUsingRuntimeSettings() {
        val runtime = context.getSharedPreferences("assistant-runtime", Context.MODE_PRIVATE)
        assertTrue(runtime.edit().putString("model", "synthetic-model").commit())
        val metrics = AssistantReplyMetrics(context)
        metrics.remember("synthetic-context-request:1", 123L)
        assertEquals(123L, AssistantReplyMetrics(context).get("synthetic-context-request:1"))
        assertEquals("synthetic-model", runtime.getString("model", null))
        assertFalse(runtime.contains("entries"))
    }

    @Test fun firstValueIsFrozenAcrossReopeningAndRequestsNeverShareMetrics() {
        val metrics = reopen()
        assertNull(metrics.get(null))
        assertNull(metrics.get("synthetic-request:1"))
        metrics.remember("synthetic-request:1", 123L)
        metrics.remember("synthetic-request:1", 456L)
        metrics.remember("synthetic-request:2", 0L)
        assertEquals(123L, metrics.get("synthetic-request:1"))
        assertEquals(0L, metrics.get("synthetic-request:2"))
        assertNull(metrics.get("synthetic-request:3"))
        val restarted = reopen()
        restarted.remember("synthetic-request:1", 789L)
        assertEquals(123L, restarted.get("synthetic-request:1"))
        assertEquals(0L, restarted.get("synthetic-request:2"))
    }

    @Test fun nullBlankAndNegativeObservationsDoNotCreateOrOverwriteEntries() {
        val metrics = reopen()
        metrics.remember(null, 1L)
        metrics.remember("", 1L)
        metrics.remember(" ", 1L)
        metrics.remember("synthetic-request:1", null)
        metrics.remember("synthetic-request:1", -1L)
        assertTrue(preferences().all.isEmpty())
        metrics.remember("synthetic-request:1", 123L)
        metrics.remember("synthetic-request:1", null)
        metrics.remember("synthetic-request:1", -1L)
        assertEquals(123L, reopen().get("synthetic-request:1"))
    }

    @Test fun malformedPreferenceTypeOrJsonIsUnknownAndNextObservationRepairsIt() {
        assertTrue(preferences().edit().putBoolean("entries", true).commit())
        assertNull(reopen().get("synthetic-request:1"))
        listOf("not synthetic json", "{}", "null").forEach { raw ->
            seed(raw)
            val metrics = reopen()
            assertNull(metrics.get("synthetic-request:1"))
            metrics.remember("synthetic-request:1", 123L)
            assertEquals(123L, reopen().get("synthetic-request:1"))
        }
    }

    @Test fun malformedRowsAreSkippedWithoutDiscardingGoodRowsOrCoercingValues() {
        seed(JSONArray(listOf(
            entry("synthetic-request:1", 123L), entry("synthetic-request:1", 456L),
            "synthetic-invalid-row", JSONObject(), entry("synthetic-negative:2", -1L),
            entry("synthetic-string:3", "123"), entry("synthetic-fraction:4", 1.5),
            entry(17, 123L), entry("", 123L), entry("synthetic-null:5", JSONObject.NULL),
            entry("synthetic-request:6", Long.MAX_VALUE),
        )).toString())
        val metrics = reopen()
        assertEquals(123L, metrics.get("synthetic-request:1"))
        assertEquals(Long.MAX_VALUE, metrics.get("synthetic-request:6"))
        listOf("synthetic-negative:2", "synthetic-string:3", "synthetic-fraction:4", "17", "", "synthetic-null:5")
            .forEach { assertNull(metrics.get(it)) }
    }

    @Test fun capEvictsOldestRecordedRequestNotMostRecentlyReadAndPersistsTheBound() {
        val metrics = reopen()
        (1..256).forEach { metrics.remember("synthetic-request:$it", it.toLong()) }
        assertEquals(1L, metrics.get("synthetic-request:1"))
        metrics.remember("synthetic-request:1", 999L)
        metrics.remember("synthetic-request:257", 257L)
        assertNull(metrics.get("synthetic-request:1"))
        assertEquals(2L, metrics.get("synthetic-request:2"))
        assertEquals(257L, reopen().get("synthetic-request:257"))
        assertNull(reopen().get("synthetic-request:1"))
        assertEquals(256, JSONArray(preferences().getString("entries", null)).length())
    }

    @Test fun oversizedStoredArrayIsAlsoBoundedOnRead() {
        seed(JSONArray((1..300).map { entry("synthetic-request:$it", it.toLong()) }).toString())
        val metrics = reopen()
        assertNull(metrics.get("synthetic-request:44"))
        assertEquals(45L, metrics.get("synthetic-request:45"))
        metrics.remember("synthetic-request:301", 301L)
        assertNull(metrics.get("synthetic-request:45"))
        assertEquals(256, JSONArray(preferences().getString("entries", null)).length())
    }

    @Test fun persistenceUsesApplyOnlyAndStoresOnlyRequestKeysAndDurations() {
        var applies = 0
        val actual = preferences()
        val asynchronous = object : SharedPreferences by actual {
            override fun edit(): SharedPreferences.Editor {
                val editor = actual.edit()
                return object : SharedPreferences.Editor by editor {
                    override fun putString(key: String?, value: String?): SharedPreferences.Editor {
                        editor.putString(key, value)
                        return this
                    }
                    override fun commit(): Boolean { fail("Metrics must not synchronously commit"); return false }
                    override fun apply() { applies++; editor.apply() }
                }
            }
        }
        val metrics = AssistantReplyMetrics(asynchronous)
        metrics.remember("synthetic-request:1", 123L)
        metrics.remember("synthetic-request:1", 456L)
        assertEquals(1, applies)
        assertEquals(setOf("entries"), actual.all.keys)
        val row = JSONArray(actual.getString("entries", null)).getJSONObject(0)
        assertEquals(setOf("request_key", "first_text_ms"), row.keys().asSequence().toSet())
        assertEquals("synthetic-request:1", row.getString("request_key"))
        assertEquals(123L, row.getLong("first_text_ms"))
    }
}
