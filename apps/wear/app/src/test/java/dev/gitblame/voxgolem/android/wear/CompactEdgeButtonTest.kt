package dev.gitblame.voxgolem.android.wear

import org.junit.Assert.assertEquals
import org.junit.Test

class CompactEdgeButtonTest {
    @Test fun controlAndIconUseTheAcceptedEightyPercentFootprint() {
        assertEquals(41.6f, CompactEdgeButtonDefaults.Height.value, 0.001f)
        assertEquals(19.2f, CompactEdgeButtonDefaults.IconSize.value, 0.001f)
    }

    @Test fun glyphIsOpaqueAtItsOwnRestingHeightIncludingPixelRounding() {
        assertEquals(1f, CompactEdgeButtonDefaults.iconAlpha(41.6f, 41.6f), 0.001f)
        assertEquals(1f, CompactEdgeButtonDefaults.iconAlpha(84f, 83.2f), 0.001f)
    }

    @Test fun glyphFadesBeforeCollapseClipsItAndNeverExceedsValidAlpha() {
        assertEquals(0f, CompactEdgeButtonDefaults.iconAlpha(0f, 41.6f), 0.001f)
        assertEquals(0f, CompactEdgeButtonDefaults.iconAlpha(20f, 41.6f), 0.001f)
        assertEquals(0.5f, CompactEdgeButtonDefaults.iconAlpha(41.6f * 0.85f, 41.6f), 0.001f)
        assertEquals(1f, CompactEdgeButtonDefaults.iconAlpha(50f, 41.6f), 0.001f)
        assertEquals(0f, CompactEdgeButtonDefaults.iconAlpha(0f, 0f), 0.001f)
    }
}
