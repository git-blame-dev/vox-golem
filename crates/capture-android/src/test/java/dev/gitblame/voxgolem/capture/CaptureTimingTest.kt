package dev.gitblame.voxgolem.capture

import android.content.pm.ApplicationInfo
import android.os.Bundle
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.shadows.ShadowLog
import org.robolectric.shadows.ShadowSystemClock
import org.robolectric.annotation.Config
import java.time.Duration

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [24, 28, 35], manifest = Config.NONE)
class CaptureTimingTest {
    private val context = RuntimeEnvironment.getApplication()

    @Before fun setUp() { ShadowLog.clear() }

    @Test fun stageDurationsAreMonotonicAndRepeatedMarkersAreCoalesced() {
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        val timing = CaptureTiming(context, "watch")
        ShadowSystemClock.advanceBy(Duration.ofMillis(125))
        timing.mark("first-audio-buffer")
        timing.mark("first-audio-buffer")
        val logs = ShadowLog.getLogsForTag(CaptureTiming.TAG)
        assertEquals(1, logs.size)
        assertTrue(logs.single().msg.contains("stage=first-audio-buffer elapsed_ms=125"))
        timing.finish("audio-saved")
        timing.finish("audio-saved")
        assertEquals(2, ShadowLog.getLogsForTag(CaptureTiming.TAG).size)
        assertTrue(timing.isFinished)
    }

    @Test fun ordinaryNonDebugBuildDoesNotEmitDiagnostics() {
        context.applicationInfo.flags = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE.inv()
        val timing = CaptureTiming(context, "watch")
        timing.mark("first-audio-buffer")
        timing.finish("audio-saved")
        assertTrue(ShadowLog.getLogsForTag(CaptureTiming.TAG).isEmpty())
        assertTrue(timing.isFinished)
    }

    @Test fun hostMetadataEnablesTimingInNonDebugBuild() {
        context.applicationInfo.flags = context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE.inv()
        context.applicationInfo.metaData = Bundle().apply { putBoolean(CaptureTiming.ENABLED_METADATA, true) }
        val timing = CaptureTiming(context, "synthetic-host")
        timing.finish("audio-saved")
        assertEquals(1, ShadowLog.getLogsForTag(CaptureTiming.TAG).size)
        assertTrue(timing.isFinished)
    }

    @Test fun captureCorrelationCannotLeakPrivateIdentityOrMatchTheNextCapture() {
        context.applicationInfo.flags = context.applicationInfo.flags or ApplicationInfo.FLAG_DEBUGGABLE
        val previous = CaptureStartupTiming.begin(context)
        CaptureStartupTiming.bind("synthetic-private-capture")
        assertSame(previous, CaptureStartupTiming.forCapture("synthetic-private-capture"))
        assertNull(CaptureStartupTiming.forCapture("synthetic-other-capture"))
        CaptureStartupTiming.begin(context)
        CaptureStartupTiming.bind("synthetic-next-capture")
        assertNull(CaptureStartupTiming.forCapture("synthetic-private-capture"))
        assertTrue(ShadowLog.getLogsForTag(CaptureTiming.TAG).none { it.msg.contains("synthetic-private") || it.msg.contains("synthetic-next") })
    }
}
