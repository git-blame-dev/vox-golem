package dev.gitblame.voxgolem.android.wear.speechtest

import android.os.Bundle
import android.os.SystemClock
import android.content.ContextWrapper
import android.content.pm.ApplicationInfo
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

/** Observer overhead only: no recognizer, speech provider, microphone or network is exercised. */
@RunWith(AndroidJUnit4::class)
class AssistantTimingRuntimeTest {
    @Test fun reportsBoundedRuntimeObserverCostsWithoutContentOrLatencyThresholds() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val samples = JSONObject()
        val iterations = 10_000
        fun measure(name: String, count: Int = iterations, operation: () -> Unit) {
            repeat(minOf(1_000, count)) { operation() }
            val batches = (1..5).map {
                val started = SystemClock.elapsedRealtimeNanos()
                repeat(count) { operation() }
                (SystemClock.elapsedRealtimeNanos() - started) / count
            }.sorted()
            samples.put(name, batches[batches.size / 2])
        }
        var control = 0
        measure("empty_control_ns") { control++ }
        var disabledEmissions = 0
        val disabledSink = AssistantTiming.Sink { disabledEmissions++ }
        val disabled = AssistantTiming.begin(null, Operation.SPEECH, sink = disabledSink)
        measure("disabled_mark_ns") { disabled.mark(Stage.REQUESTED) }
        val enabled = AssistantTiming.begin(context, Operation.SPEECH, sink = AssistantTiming.Sink {})
        enabled.mark(Stage.REQUESTED)
        measure("enabled_duplicate_mark_ns") { enabled.mark(Stage.REQUESTED) }
        measure("enabled_silent_operation_ns") {
            val timing = AssistantTiming.begin(context, Operation.SPEECH, sink = AssistantTiming.Sink {})
            timing.mark(Stage.REQUESTED); timing.finish(Stage.SETTLED)
        }
        val releaseInfo = ApplicationInfo(context.applicationInfo).apply {
            flags = flags and ApplicationInfo.FLAG_DEBUGGABLE.inv()
            metaData = null
        }
        val releaseLike = object : ContextWrapper(context) {
            override fun getApplicationInfo(): ApplicationInfo = releaseInfo
        }
        // Real PackageManager lookup in a debug APK; this is not release-build performance acceptance.
        measure("release_like_real_context_begin_ns", 200) {
            AssistantTiming.begin(releaseLike, Operation.SPEECH, sink = disabledSink).finish(Stage.SETTLED)
        }
        val logged = (1..5).map {
            val started = SystemClock.elapsedRealtimeNanos()
            repeat(10) {
                val timing = AssistantTiming.begin(context, Operation.SPEECH)
                timing.mark(Stage.REQUESTED); timing.finish(Stage.SETTLED)
            }
            (SystemClock.elapsedRealtimeNanos() - started) / 10
        }.sorted()
        samples.put("log_and_trace_operation_ns", logged[logged.size / 2])
        disabled.finish(Stage.SETTLED); enabled.finish(Stage.SETTLED)
        assertEquals(51_000, control)
        assertEquals(0, disabledEmissions)
        instrumentation.sendStatus(0, Bundle().apply {
            putString("assistant_observer_costs", samples.toString())
        })
    }
}
