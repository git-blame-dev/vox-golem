package dev.gitblame.voxgolem.android.wear

import org.junit.Assert.*
import org.junit.Test

class CaptureTranscriptFollowTest {
    private val first = "Synthetic long transcript"
    private val next = "$first Synthetic added words"

    @Test fun geometryChangesCannotRearmCompletedContentButNewWordsCan() {
        val follow = CaptureTranscriptFollow()
        follow.observe(first)
        assertEquals(855f, follow.distance(first, first, 800, 800, 220, 55, false)!!, 0f)
        follow.completed(first)
        follow.observe(first)
        assertNull(follow.distance(first, first, 900, 900, 180, 3, false))
        follow.observe(next)
        assertEquals(903f, follow.distance(next, next, 900, 900, 180, 3, false)!!, 0f)
    }

    @Test fun aRequestWaitsForMatchingTextAndSettledLayoutWithoutCompetingWithManualScroll() {
        val follow = CaptureTranscriptFollow()
        follow.observe(next)
        assertNull(follow.distance(next, first, 800, 800, 220, 55, false))
        assertNull(follow.distance(next, next, 900, 800, 220, 55, false))
        assertNull(follow.distance(next, next, 900, 900, 0, 55, false))
        assertNull(follow.distance(next, next, 900, 900, 220, 55, true))
        assertEquals(955f, follow.distance(next, next, 900, 900, 220, 55, false)!!, 0f)
    }

    @Test fun completionOfAnOldRequestCannotDiscardNewTranscriptFollow() {
        val follow = CaptureTranscriptFollow()
        follow.observe(first)
        follow.observe(next)
        follow.completed(first)
        assertNull(follow.distance(first, first, 800, 800, 220, 55, false))
        assertNotNull(follow.distance(next, next, 900, 900, 220, 55, false))
        follow.completed(next)
        assertNull(follow.distance(next, next, 900, 900, 220, 55, false))
    }
}
