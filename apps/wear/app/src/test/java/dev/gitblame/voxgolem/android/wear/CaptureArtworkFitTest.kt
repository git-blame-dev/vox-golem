package dev.gitblame.voxgolem.android.wear

import org.junit.Assert.*
import org.junit.Test

class CaptureArtworkFitTest {
    @Test fun ringFitsInsideTheShorterViewportWithPhysicalAntialiasMargin() {
        val inkFraction = (62.0 / 68.888889).toFloat()
        for ((width, height) in listOf(320f to 320f, 384f to 384f, 450f to 450f, 320f to 450f, 450f to 320f)) {
            val diameter = minOf(width, height)
            val paintedDiameter = diameter * inkFraction * artworkBaseFit(width, height, inkFraction)
            assertEquals(diameter - 4f, paintedDiameter, 0.001f)
            assertTrue(paintedDiameter < width && paintedDiameter < height)
        }
    }

    @Test fun fitTracksSourceGeometryInsteadOfWatchSpecificZoom() {
        assertEquals(316f / 320f, artworkBaseFit(320f, 320f, 1f), 0.0001f)
        assertEquals(316f / 160f, artworkBaseFit(320f, 320f, 0.5f), 0.0001f)
        assertEquals(1f, artworkBaseFit(0f, 0f, 0.9f), 0f)
        for (fraction in listOf(0f, -1f, 1.1f, Float.NaN)) {
            try { artworkBaseFit(320f, 320f, fraction); fail("Invalid ink fraction accepted") }
            catch (_: IllegalArgumentException) { }
        }
    }
}
