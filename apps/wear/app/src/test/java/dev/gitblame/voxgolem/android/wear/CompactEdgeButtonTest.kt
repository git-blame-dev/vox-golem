package dev.gitblame.voxgolem.android.wear

import androidx.compose.ui.unit.dp
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class CompactEdgeButtonTest {
    @Test fun persistentActionHasAFullFortyEightDpTargetWithAnIndependentCompactGlyph() {
        assertEquals(48f, CompactEdgeButtonDefaults.Height.value, 0f)
        assertEquals(54f, CompactEdgeButtonDefaults.OccupiedHeight.value, 0f)
        assertEquals(57f, CompactEdgeButtonDefaults.ViewportExclusion.value, 0f)
        assertTrue(CompactEdgeButtonDefaults.IconSize < CompactEdgeButtonDefaults.Height)
    }

    @Test fun entireSemanticTargetFitsInsideSmallAndLargeRoundScreens() {
        val size = CompactEdgeButtonDefaults.Height.value
        val bottomInset = CompactEdgeButtonDefaults.BottomInset.value
        for (diameter in listOf(192f, 200f, 228f, 260f)) {
            val radius = diameter / 2f
            val left = radius - size / 2f
            val top = diameter - bottomInset - size
            for (x in listOf(left, left + size)) {
                for (y in listOf(top, top + size)) {
                    val distanceSquared = (x - radius) * (x - radius) + (y - radius) * (y - radius)
                    assertTrue("Target corner clipped at ${diameter}dp", distanceSquared <= radius * radius)
                }
            }
        }
    }

    @Test fun readingViewportExcludesActionAndGapAtEveryScrollPosition() {
        // Viewport clipping, unlike final-item padding, protects middle lines of long paragraphs.
        for (height in listOf(192f, 200f, 228f, 260f, 600f)) {
            val viewportBottom = height - CompactEdgeButtonDefaults.readingBottomInset(height.dp).value
            val actionTop = height - CompactEdgeButtonDefaults.BottomInset.value - CompactEdgeButtonDefaults.Height.value
            assertTrue("Reading overlaps action at ${height}dp", viewportBottom <= actionTop - 3f)
            assertTrue(viewportBottom > height / 2f)
        }
        val captureClearance = CompactEdgeButtonDefaults.ViewportExclusion.value
        assertTrue(captureClearance >= CompactEdgeButtonDefaults.Height.value + CompactEdgeButtonDefaults.BottomInset.value + 3f)
    }
}
