package dev.gitblame.voxgolem.android.wear

import org.junit.Assert.assertEquals
import org.junit.Test

class CaptureSavedExitTest {
    @Test fun exitProgressIsBoundedToTheLastQuarterSecondOfTheSavedDeadline() {
        val deadline = 14_000L
        assertEquals(0f, captureSavedExitProgress(deadline, 10_000L), 0f)
        assertEquals(0f, captureSavedExitProgress(deadline, 13_749L), 0f)
        assertEquals(0f, captureSavedExitProgress(deadline, 13_750L), 0f)
        assertEquals(0.5f, captureSavedExitProgress(deadline, 13_875L), 0f)
        assertEquals(1f, captureSavedExitProgress(deadline, 14_000L), 0f)
        assertEquals(1f, captureSavedExitProgress(deadline, 15_000L), 0f)
    }
}
