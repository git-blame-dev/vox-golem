package dev.gitblame.voxgolem.assistant

import android.content.pm.ApplicationInfo
import android.os.Bundle
import dev.gitblame.voxgolem.assistant.AssistantTiming.Kind
import dev.gitblame.voxgolem.assistant.AssistantTiming.Operation
import dev.gitblame.voxgolem.assistant.AssistantTiming.Stage
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowLog
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 28, 35], manifest = Config.NONE)
class AssistantTimingTest {
    private val context = RuntimeEnvironment.getApplication()

    @Before fun reset() {
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        context.applicationInfo.metaData = Bundle()
        ShadowLog.clear()
    }

    @Test fun monotonicStagesAreCoalescedAndFinishClosesExactlyOnce() {
        var now = 5_000_000_000L
        val events = mutableListOf<AssistantTiming.Event>()
        val timing = AssistantTiming.begin(context, Operation.SPEECH, { now }, AssistantTiming.Sink { events += it })
        now += 125_000_000
        timing.mark(Stage.QUEUE_REQUESTED); timing.mark(Stage.QUEUE_REQUESTED)
        now += 10_000_000
        timing.finish(Stage.ON_DONE); timing.finish(Stage.ON_ERROR); timing.mark(Stage.ON_START)
        assertEquals(listOf(Kind.BEGIN, Kind.MARK, Kind.MARK, Kind.END), events.map { it.kind })
        assertEquals(listOf(0L, 125_000_000L, 135_000_000L, 135_000_000L), events.map { it.elapsedNanos })
        assertEquals(listOf(Stage.QUEUE_REQUESTED, Stage.ON_DONE), events.filter { it.kind == Kind.MARK }.map { it.stage })
        assertTrue(timing.isFinished)
    }

    @Test fun fixedVocabularyBoundsRepeatedEmission() {
        val events = mutableListOf<AssistantTiming.Event>()
        val timing = AssistantTiming.begin(context, Operation.PREPARATION, sink = AssistantTiming.Sink { events += it })
        repeat(1_000) { Stage.values().forEach(timing::mark) }
        timing.finish(Stage.SETTLED)
        assertEquals(Stage.values().size + 2, events.size)
        assertEquals(1, events.count { it.kind == Kind.END })
    }

    @Test fun childrenUseTheSameObserverWithoutGlobalLatestRunOrCoupledRetirement() {
        var now = 1_000_000_000L
        val events = mutableListOf<AssistantTiming.Event>()
        val parent = AssistantTiming.begin(context, Operation.PREPARATION, { now }, AssistantTiming.Sink { events += it })
        now += 10_000_000
        val child = parent.child(Operation.TTS_INITIALIZATION)
        val unrelated = AssistantTiming.begin(context, Operation.SPEECH)
        now += 25_000_000
        parent.finish(Stage.REPLACED)
        child.finish(Stage.INIT_READY)
        unrelated.finish(Stage.CANCELLED)
        assertNotEquals(parent.run, child.run); assertNotEquals(child.run, unrelated.run)
        val childEvents = events.filter { it.run == child.run }
        assertTrue(childEvents.all { it.parentRun == parent.run })
        assertEquals(10_000_000L, childEvents.first().elapsedNanos)
        assertEquals(35_000_000L, childEvents.last().elapsedNanos)
        assertEquals(Stage.INIT_READY, childEvents.last().stage)
    }

    @Test fun ordinaryReleaseAndAbsentContextDoNotReadClockOrEmit() {
        context.applicationInfo.flags = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE.inv()
        var reads = 0; var emissions = 0
        for (host in listOf(context, null)) {
            val timing = AssistantTiming.begin(host, Operation.SPEECH, { reads++; 0L }, AssistantTiming.Sink { emissions++ })
            timing.mark(Stage.REQUESTED); timing.finish(Stage.CANCELLED)
            timing.child(Operation.TTS_UTTERANCE).finish(Stage.ON_DONE)
            assertTrue(timing.isFinished)
        }
        assertEquals(0, reads); assertEquals(0, emissions)
        assertTrue(ShadowLog.getLogsForTag(AssistantTiming.TAG).isEmpty())
    }

    @Test fun explicitAssistantMetadataEnablesReleaseButCaptureMetadataDoesNot() {
        context.applicationInfo.flags = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE.inv()
        context.applicationInfo.metaData = Bundle().apply { putBoolean("dev.gitblame.voxgolem.capture.TIMING_ENABLED", true) }
        AssistantTiming.begin(context, Operation.SPEECH).finish(Stage.SETTLED)
        assertTrue(ShadowLog.getLogsForTag(AssistantTiming.TAG).isEmpty())
        context.applicationInfo.metaData = Bundle().apply { putBoolean(AssistantTiming.ENABLED_METADATA, true) }
        val timing = AssistantTiming.begin(context, Operation.SPEECH, clock = { 0L })
        timing.finish(Stage.SETTLED)
        assertEquals("operation=SPEECH run=${timing.run} stage=SETTLED elapsed_ms=0 observed_ms=0",
            ShadowLog.getLogsForTag(AssistantTiming.TAG).single().msg)
    }

    @Test fun diagnosticSinkFailureDoesNotPreventExactlyOnceClosure() {
        var ends = 0
        val timing = AssistantTiming.begin(context, Operation.SPEECH, sink = AssistantTiming.Sink {
            if (it.kind == Kind.END) ends++
            throw IllegalStateException("Synthetic diagnostic failure")
        })
        timing.mark(Stage.REQUESTED); timing.finish(Stage.FAILED); timing.finish(Stage.CANCELLED)
        assertTrue(timing.isFinished); assertEquals(1, ends)
    }

    @Test fun concurrentCallbacksCannotEmitAfterTheSingleEnd() {
        val events = mutableListOf<AssistantTiming.Event>()
        val timing = AssistantTiming.begin(context, Operation.TTS_UTTERANCE, { 0L }, AssistantTiming.Sink { events += it })
        val start = CountDownLatch(1)
        val done = CountDownLatch(8)
        repeat(8) {
            Thread {
                try { start.await(); timing.mark(Stage.ON_START); timing.finish(Stage.ON_DONE) }
                finally { done.countDown() }
            }.start()
        }
        start.countDown()
        assertTrue(done.await(5, TimeUnit.SECONDS))
        assertEquals(1, events.count { it.kind == Kind.END })
        assertEquals(Kind.END, events.last().kind)
        assertTrue(events.count { it.stage == Stage.ON_START } <= 1)
    }
}
