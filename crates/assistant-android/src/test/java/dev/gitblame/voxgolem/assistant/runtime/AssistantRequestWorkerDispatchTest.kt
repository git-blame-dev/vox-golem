package dev.gitblame.voxgolem.assistant.runtime

import android.content.pm.ApplicationInfo
import dev.gitblame.voxgolem.assistant.AssistantTiming
import dev.gitblame.voxgolem.assistant.AssistantTiming.Kind
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import java.util.concurrent.Executor

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], manifest = Config.NONE)
class AssistantRequestWorkerDispatchTest {
    private fun timing(clock: () -> Long, events: MutableList<AssistantTiming.Event>): AssistantTiming {
        val context = RuntimeEnvironment.getApplication()
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        return AssistantTiming.begin(context, Operation.GENERATION, clock, AssistantTiming.Sink { events += it })
    }

    @Test fun dispatchIsCoalescedBeforeFirstTextUsingAndroidObservationNotProviderDuration() {
        var now = 5_000_000_000L
        val events = mutableListOf<AssistantTiming.Event>()
        val timing = timing({ now }, events)
        val durations = mutableListOf<Long>()
        val native = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                now += 10_000_000; callback.onReady()
                now += 10_000_000; callback.onRequestDispatched()
                now += 10_000_000; callback.onRequestDispatched()
                now += 10_000_000; callback.onFirstText(123)
                callback.onDelta("Synthetic answer")
                return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) {}
        }
        val worker = AssistantRequestWorker(Executor { it.run() }, native)
        assertTrue(worker.submit("synthetic-dispatch", "/synthetic/auth", "{}", object : AssistantRequestListener {
            override fun onFirstText(requestId: String, milliseconds: Long) {
                durations += milliseconds
                timing.mark(Stage.FIRST_TEXT)
            }
            override fun onDelta(requestId: String, text: String) {
                assertEquals(Stage.FIRST_TEXT, events.last().stage)
            }
            override fun onOutcome(requestId: String, outcomeJson: String) {}
        }, timing))
        val marks = events.filter { it.kind == Kind.MARK }
        assertEquals(listOf(Stage.WORKER_STARTED, Stage.NATIVE_READY, Stage.REQUEST_DISPATCHED, Stage.FIRST_TEXT), marks.map { it.stage })
        assertEquals(listOf(0L, 10_000_000L, 20_000_000L, 40_000_000L), marks.map { it.elapsedNanos })
        assertEquals(5_020_000_000L, marks.single { it.stage == Stage.REQUEST_DISPATCHED }.observedNanos)
        assertEquals(listOf(123L), durations)
        worker.close()
    }

    @Test fun cancelledRetiredAndReplacedCallbacksCannotPublishDispatchTiming() {
        lateinit var worker: AssistantRequestWorker
        lateinit var cancelledCallback: ProviderDeltaCallback
        lateinit var retiredCallback: ProviderDeltaCallback
        val cancelledEvents = mutableListOf<AssistantTiming.Event>()
        val retiredEvents = mutableListOf<AssistantTiming.Event>()
        val activeEvents = mutableListOf<AssistantTiming.Event>()
        val native = object : ProviderNativeCalls {
            override fun respond(requestId: String, authPath: String, requestJson: String, callback: ProviderDeltaCallback): String {
                callback.onReady()
                when (requestId) {
                    "synthetic-cancelled" -> {
                        cancelledCallback = callback
                        worker.cancel()
                        callback.onRequestDispatched()
                    }
                    "synthetic-retired" -> retiredCallback = callback
                    else -> {
                        cancelledCallback.onRequestDispatched()
                        retiredCallback.onRequestDispatched()
                        callback.onRequestDispatched()
                        callback.onRequestDispatched()
                    }
                }
                return """{"status":"complete"}"""
            }
            override fun cancel(requestId: String) {}
        }
        worker = AssistantRequestWorker(Executor { it.run() }, native)
        val listener = object : AssistantRequestListener {
            override fun onDelta(requestId: String, text: String) { fail("No text expected") }
            override fun onOutcome(requestId: String, outcomeJson: String) { assertNotEquals("synthetic-cancelled", requestId) }
        }
        assertTrue(worker.submit("synthetic-cancelled", "/synthetic/auth", "{}", listener, timing({ 0L }, cancelledEvents)))
        cancelledCallback.onRequestDispatched()
        assertTrue(worker.submit("synthetic-retired", "/synthetic/auth", "{}", listener, timing({ 0L }, retiredEvents)))
        retiredCallback.onRequestDispatched()
        assertTrue(worker.submit("synthetic-replacement", "/synthetic/auth", "{}", listener, timing({ 0L }, activeEvents)))
        assertFalse(cancelledEvents.any { it.stage == Stage.REQUEST_DISPATCHED })
        assertFalse(retiredEvents.any { it.stage == Stage.REQUEST_DISPATCHED })
        assertEquals(1, activeEvents.count { it.stage == Stage.REQUEST_DISPATCHED })
        worker.close()
    }
}
