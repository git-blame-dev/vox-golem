package dev.gitblame.voxgolem.capture

import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class CapturePlaybackTest {
    @Test fun preparedCompletionErrorAndReplacementAreExactOwnerFenced() {
        val phases = mutableListOf<String>()
        val playback = CapturePlayback { phases += it.getString("phase") }
        val first = Any(); val second = Any()
        var releases = 0; var starts = 0
        playback.begin("synthetic-first", { first }) { releases++ }
        assertEquals("preparing", playback.snapshot().getString("phase"))
        playback.prepared(first) { starts++ }
        playback.begin("synthetic-second", { second }) { releases++ }
        playback.finish(first, "failed")
        playback.prepared(first) { fail("stale player started") }
        assertEquals("preparing", playback.snapshot().getString("phase"))
        assertEquals(1, releases)
        playback.prepared(second) { starts++ }
        playback.finish(second, "completed")
        playback.finish(second, "failed")
        assertEquals(2, starts)
        assertEquals(2, releases)
        assertEquals(listOf("preparing", "playing", "stopped", "preparing", "playing", "completed"), phases)
    }

    @Test fun startFailureAndDestructionStopReleaseExactlyOnce() {
        val playback = CapturePlayback { }
        val token = Any(); var releases = 0
        playback.begin("synthetic", { token }) { releases++ }
        playback.prepared(token) { error("synthetic start failure") }
        assertEquals("failed", playback.snapshot().getString("phase"))
        playback.stop()
        assertEquals(1, releases)
        playback.begin("synthetic", { Any() }) { releases++ }
        playback.stop(); playback.stop()
        assertEquals("stopped", playback.snapshot().getString("phase"))
        assertEquals(2, releases)
    }

    @Test fun recreatedOwnerInitialSnapshotAdvancesPastOldEvents() {
        val oldEvents = mutableListOf<app.tauri.plugin.JSObject>()
        val old = CapturePlayback { oldEvents += it }
        old.begin("synthetic-old", { Any() }) { }
        old.stop()
        val replacement = CapturePlayback { }
        val initial = replacement.snapshot()
        assertEquals("idle", initial.getString("phase"))
        for (event in oldEvents) assertTrue(initial.getLong("revision") > event.getLong("revision"))
        replacement.begin("synthetic-new", { Any() }) { }
        assertTrue(replacement.snapshot().getLong("revision") > initial.getLong("revision"))
    }

    @Test fun failedRetirementRetainsOwnerBlocksAllocationAndRetriesOnlyExplicitly() {
        val phases = mutableListOf<String>()
        val playback = CapturePlayback { phases += it.getString("phase") }
        val first = Any(); val second = Any()
        var attempts = 0
        var failRelease = true
        var allocations = 0
        playback.begin("synthetic-first", { first }) {
            attempts++
            if (failRelease) error("Synthetic release failure")
        }
        playback.finish(first, "completed")
        assertEquals("failed", playback.snapshot().getString("phase"))
        playback.finish(first, "completed")
        playback.prepared(first) { fail("cleanup-failed owner started") }
        assertEquals(1, attempts)
        assertNull(playback.begin("synthetic-second", { allocations++; second }) { })
        assertEquals(0, allocations)
        assertEquals(2, attempts)
        assertFalse(phases.contains("completed"))
        assertFalse(phases.contains("stopped"))
        failRelease = false
        assertTrue(playback.stop())
        assertEquals(3, attempts)
        playback.begin("synthetic-second", { allocations++; second }) { }
        playback.finish(first, "completed")
        assertEquals("preparing", playback.snapshot().getString("phase"))
        assertEquals(1, allocations)
        playback.finish(second, "completed")
        assertEquals("completed", playback.snapshot().getString("phase"))
    }
}
