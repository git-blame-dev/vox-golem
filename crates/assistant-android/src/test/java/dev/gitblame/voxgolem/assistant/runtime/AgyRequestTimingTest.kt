package dev.gitblame.voxgolem.assistant.runtime

import org.junit.Assert.*
import org.junit.Test

class AgyRequestTimingTest {
    @Test fun firstTextAndCompletionShareTheDispatchClockOrigin() {
        var elapsedRealtimeMs = 8_000L
        val timing = AgyRequestTiming { elapsedRealtimeMs }
        elapsedRealtimeMs += 120L // Request pipe write/flush and engine processing.
        assertEquals(120L, timing.onForwardedText("Synthetic answer"))
        elapsedRealtimeMs += 80L
        assertEquals(200L, timing.completedMs())
        assertEquals(120L, timing.firstTextMs)
    }

    @Test fun whitespaceDoesNotBecomeFirstAnswerAndLaterTextDoesNotResetIt() {
        var elapsedRealtimeMs = 20_000L
        val timing = AgyRequestTiming { elapsedRealtimeMs }
        elapsedRealtimeMs += 10L
        assertNull(timing.onForwardedText(""))
        assertNull(timing.onForwardedText(" \t\r\n"))
        assertNull(timing.firstTextMs)
        elapsedRealtimeMs += 30L
        assertEquals(40L, timing.onForwardedText(" Synthetic answer"))
        elapsedRealtimeMs += 50L
        assertNull(timing.onForwardedText(" more synthetic text"))
        assertEquals(40L, timing.firstTextMs)
    }

    @Test fun terminalWithoutNonblankForwardedTextDoesNotInventFirstText() {
        var elapsedRealtimeMs = 30_000L
        val timing = AgyRequestTiming { elapsedRealtimeMs }
        elapsedRealtimeMs += 250L
        assertEquals(250L, timing.completedMs())
        assertNull(timing.firstTextMs)
    }

    @Test fun zeroElapsedFirstTextIsObservedRatherThanMissing() {
        val timing = AgyRequestTiming { 40_000L }
        assertEquals(0L, timing.onForwardedText("Synthetic answer"))
        assertEquals(0L, timing.firstTextMs)
        assertNull(timing.onForwardedText("Synthetic continuation"))
        assertEquals(0L, timing.completedMs())
    }
}
