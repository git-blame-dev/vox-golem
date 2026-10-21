package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.Executor
import java.util.concurrent.RejectedExecutionException

class CaptureProjectionPumpTest {
    @Test fun blockedSenderFloodRetainsLatestValuesWithinBothBudgetsAndOnlyOneRunnable() {
        val scheduled = mutableListOf<Runnable>()
        val sent = mutableListOf<Int>()
        val pump = CaptureProjectionPump(Executor { scheduled.add(it) }, 4, 100)
        repeat(10_000) { revision -> pump.offer("synthetic-session/text", 32, Runnable { sent.add(revision) }) }
        assertEquals(1, scheduled.size)
        assertEquals(1 to 32, pump.retained())
        repeat(100) { key -> pump.offer("synthetic-$key/status", 30, Runnable {}) }
        assertTrue(pump.retained().first <= 4)
        assertTrue(pump.retained().second <= 100)
        scheduled.single().run()
        assertEquals(0 to 0, pump.retained())
    }

    @Test fun finalReplacesPartialAndKeepsStatusBeforeFinal() {
        val scheduled = mutableListOf<Runnable>()
        val sent = mutableListOf<String>()
        val pump = CaptureProjectionPump(Executor { scheduled.add(it) })
        pump.offer("synthetic/text", 10, Runnable { sent.add("partial") })
        pump.offer("synthetic/status", 10, Runnable { sent.add("status") })
        pump.offer("synthetic/text", 10, Runnable { sent.add("final") })
        scheduled.single().run()
        assertEquals(listOf("status", "final"), sent)
    }

    @Test fun rejectedSubmissionCanBeRetriedWithoutStrandingSchedulingFlag() {
        var rejecting = true
        val scheduled = mutableListOf<Runnable>()
        val sent = mutableListOf<String>()
        val pump = CaptureProjectionPump(Executor { if (rejecting) throw RejectedExecutionException() else scheduled.add(it) })
        pump.offer("synthetic/text", 10, Runnable { sent.add("old") })
        rejecting = false
        pump.offer("synthetic/text", 10, Runnable { sent.add("latest") })
        scheduled.single().run()
        assertEquals(listOf("latest"), sent)
        pump.close()
        pump.offer("synthetic/closed", 10, Runnable { fail("closed pump delivered") })
        assertEquals(0 to 0, pump.retained())
    }
}
