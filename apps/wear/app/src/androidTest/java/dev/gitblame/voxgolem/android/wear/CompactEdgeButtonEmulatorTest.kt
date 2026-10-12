package dev.gitblame.voxgolem.android.wear

import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.Rect
import android.os.Build
import android.os.SystemClock
import android.view.InputDevice
import android.view.MotionEvent
import android.view.accessibility.AccessibilityNodeInfo
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.util.concurrent.atomic.AtomicReference
import kotlin.math.abs

/** Run only the named fixture class against the isolated emulator APK, never the normal app. */
@RunWith(AndroidJUnit4::class)
class CompactEdgeButtonEmulatorTest {
    private val instrumentation get() = InstrumentationRegistry.getInstrumentation()
    private val automation get() = instrumentation.uiAutomation
    private val context get() = instrumentation.targetContext
    private val density get() = context.resources.displayMetrics.density

    @Before fun requireIsolatedWearEmulator() {
        assertEquals("Normal app identity/storage must not be used", CompactEdgeButtonFixtureActivity.FIXTURE_PACKAGE, context.packageName)
        assertTrue("Physical-device execution is forbidden", Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish")
        assertTrue("Requires a Wear emulator", context.packageManager.hasSystemFeature(PackageManager.FEATURE_WATCH))
    }

    @Test(timeout = 90_000) fun restingMicStopSendAndNoteStopAreFullyPaintedAndExposeOneNativeAction() {
        launch().use { scenario ->
            CompactFixtureAction.entries.forEachIndexed { index, action ->
                scenario.onActivity { it.show(action) }
                val value = awaitRest(scenario, action)
                assertFootprint(value)
                assertGlyphPaint(value)
                val nodes = nodes()
                assertEquals("Exactly one accessible action, not an overlay duplicate", 1,
                    nodes.count { it.description == action.description })
                assertEquals("Only the native edge target is clickable", 1, nodes.count { it.clickable })
                assertTrue("Decorative glyph semantics must be cleared", nodes.none {
                    it.description == CompactEdgeButtonFixtureActivity.DECORATIVE_DESCRIPTION
                })
                assertTrue("Native accessibility action must accept the enabled click", clickAction(action.description))
                await("One input invokes exactly one callback") { snapshot(scenario).clicks == index + 1 }
                instrumentation.waitForIdleSync()
                assertEquals("No second overlay callback", index + 1, snapshot(scenario).clicks)
            }
        }
    }

    @Test(timeout = 90_000) fun disabledControlRejectsAccessibilityAndPointerClicksAndUsesPublicDisabledColors() {
        launch().use { scenario ->
            val enabled = awaitRest(scenario, CompactFixtureAction.MIC)
            assertGlyphPaint(enabled)
            scenario.onActivity { it.show(CompactFixtureAction.MIC, enabled = false) }
            await("Native accessibility state becomes disabled") {
                nodes().singleOrNull { it.description == CompactFixtureAction.MIC.description }?.enabled == false &&
                    snapshot(scenario).suppliedContentColor == snapshot(scenario).expectedContentColor
            }
            val disabled = snapshot(scenario)
            assertFalse("Theme supplies a different disabled content color", enabled.expectedContentColor == disabled.expectedContentColor)
            assertFalse("Theme supplies a different disabled container color", enabled.expectedContainerColor == disabled.expectedContainerColor)
            assertGlyphPaint(disabled)
            // Some platform versions acknowledge ACTION_CLICK on a disabled node; callback rejection is the contract.
            clickAction(CompactFixtureAction.MIC.description)
            injectTap(disabled.glyphBounds.exactCenterX(), disabled.glyphBounds.exactCenterY())
            instrumentation.waitForIdleSync()
            SystemClock.sleep(250)
            assertEquals("Neither disabled input path may invoke the native callback", 0, snapshot(scenario).clicks)
            scenario.onActivity { it.show(CompactFixtureAction.MIC, enabled = true) }
            val restored = awaitRest(scenario, CompactFixtureAction.MIC)
            injectTap(restored.glyphBounds.exactCenterX(), restored.glyphBounds.exactCenterY())
            await("Re-enabled native pointer input invokes one callback") { snapshot(scenario).clicks == 1 }
        }
    }

    @Test(timeout = 90_000) fun scaffoldReservesCompactHeightAndLongSyntheticTextStaysClearThenEdgeCollapsesAndReturns() {
        launch().use { scenario ->
            val rest = awaitRest(scenario, CompactFixtureAction.MIC)
            assertFootprint(rest)
            assertFalse("The long synthetic answer reaches its actual end", rest.canScrollForward)
            assertTrue("Terminal text is measured, not a clipped/offscreen semantics assertion", rest.textBounds.height() > 0)
            assertTrue("Text stays above the painted wrapper rather than its expanded accessible target",
                rest.textBounds.bottom <= rest.edgeBounds.top + 1)
            assertTrue("Scaffold reservation includes the measured compact footprint",
                rest.bottomPaddingPx >= rest.edgeBounds.height() - 1)
            assertTrue("Reservation must not silently retain the old 52dp footprint",
                rest.bottomPaddingPx <= (41.6f + 3f) * density + 2)
            assertTrue("Long content is actually scrollable", nodes().any { it.scrollable })

            scenario.onActivity { it.readBeginning() }
            await("Native scaffold collapses the edge when reading away from latest") {
                val value = snapshot(scenario)
                value.canScrollForward && !value.scrolling && value.edgeBounds.height() <= 1
            }
            assertTrue("No floating glyph remains accessible while collapsed", nodes().none {
                it.description == CompactEdgeButtonFixtureActivity.DECORATIVE_DESCRIPTION
            })
            val collapsedBitmap = requireNotNull(automation.takeScreenshot())
            try {
                assertFalse("Native compact container is no longer painted at its resting location",
                    nearColor(collapsedBitmap.getPixel(rest.glyphBounds.centerX() + (12f * density).toInt(),
                        rest.glyphBounds.centerY()), rest.expectedContainerColor))
            } finally { collapsedBitmap.recycle() }
            scenario.onActivity { it.readLatest() }
            val returned = awaitRest(scenario, CompactFixtureAction.MIC)
            assertFootprint(returned)
            assertGlyphPaint(returned)
            assertEquals("Scrolling itself never invokes the button", 0, returned.clicks)
        }
    }

    private fun assertFootprint(value: CompactEdgeFixtureSnapshot) {
        assertEquals("Actual helper wrapper is 41.6dp, not graphics-only scaling", 41.6f * density,
            value.edgeBounds.height().toFloat(), 1.5f)
        assertEquals("Actual glyph is 19.2dp", 19.2f * density, value.glyphBounds.height().toFloat(), 1.5f)
        assertTrue("Compact adapter keeps the full-width scaffold constraint",
            value.edgeBounds.width() >= context.resources.displayMetrics.widthPixels - 2)
        assertEquals("Wrapper stays bottom-aligned", context.resources.displayMetrics.heightPixels.toFloat(),
            value.edgeBounds.bottom.toFloat(), 2f)
    }

    private fun assertGlyphPaint(value: CompactEdgeFixtureSnapshot) {
        assertEquals("Adapter supplies the public theme color to decorative content",
            value.expectedContentColor, value.suppliedContentColor)
        val sampleX = value.glyphBounds.centerX() + (12f * density).toInt()
        await("Native container settles to the public enabled/disabled palette") {
            val frame = requireNotNull(automation.takeScreenshot())
            try { nearColor(frame.getPixel(sampleX, value.glyphBounds.centerY()), value.expectedContainerColor) }
            finally { frame.recycle() }
        }
        val bitmap = requireNotNull(automation.takeScreenshot()) { "Actual Android screenshot is required" }
        try {
            val glyph = value.glyphBounds
            assertTrue("Measured glyph is fully inside the helper wrapper", value.edgeBounds.contains(glyph))
            assertTrue("Screenshot contains the measured fixture", Rect(0, 0, bitmap.width, bitmap.height).contains(glyph))
            val painted = Rect()
            var count = 0
            for (y in glyph.top until glyph.bottom) for (x in glyph.left until glyph.right) {
                if (nearColor(bitmap.getPixel(x, y), value.expectedPaintColor)) {
                    if (count++ == 0) painted.set(x, y, x + 1, y + 1) else painted.union(x, y, x + 1, y + 1)
                }
            }
            // Full-color extents reject the old almost-invisible native content fade and clipped top/bottom strokes.
            val minimumWidth = if (value.action == CompactFixtureAction.MIC) .43f else .65f
            val minimumHeight = if (value.action == CompactFixtureAction.MIC) .77f else .65f
            assertTrue("Full theme-colored glyph must be painted, not just present in layout (pixels=$count)", count > 8)
            assertTrue("Glyph's left/right extent remains visible", painted.width() >= glyph.width() * minimumWidth)
            assertTrue("Glyph's top/bottom extent remains visible", painted.height() >= glyph.height() * minimumHeight)
            assertTrue("Native container uses the same public enabled/disabled palette",
                nearColor(bitmap.getPixel(sampleX, glyph.centerY()), value.expectedContainerColor))
        } finally { bitmap.recycle() }
    }

    private fun nearColor(actual: Int, expected: Int) =
        abs(Color.red(actual) - Color.red(expected)) <= 8 &&
            abs(Color.green(actual) - Color.green(expected)) <= 8 &&
            abs(Color.blue(actual) - Color.blue(expected)) <= 8

    private fun launch(): ActivityScenario<CompactEdgeButtonFixtureActivity> = ActivityScenario.launch(
        Intent(context, CompactEdgeButtonFixtureActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))

    private fun snapshot(scenario: ActivityScenario<CompactEdgeButtonFixtureActivity>): CompactEdgeFixtureSnapshot {
        val value = AtomicReference<CompactEdgeFixtureSnapshot>()
        scenario.onActivity { value.set(it.snapshot()) }
        return value.get()
    }

    private fun awaitRest(scenario: ActivityScenario<CompactEdgeButtonFixtureActivity>, action: CompactFixtureAction): CompactEdgeFixtureSnapshot {
        await("Actual compact helper settles at latest with requested action and palette") {
            val value = snapshot(scenario)
            value.action == action && value.enabled && !value.canScrollForward && !value.scrolling &&
                abs(value.edgeBounds.height() - 41.6f * density) <= 1.5f &&
                value.suppliedContentColor == value.expectedContentColor &&
                nodes().any { it.description == action.description && it.enabled }
        }
        instrumentation.waitForIdleSync()
        return snapshot(scenario)
    }

    private data class Node(val description: String?, val clickable: Boolean, val enabled: Boolean, val scrollable: Boolean)

    private fun nodes(): List<Node> {
        val result = mutableListOf<Node>()
        fun visit(node: AccessibilityNodeInfo) {
            if (node.isVisibleToUser) result += Node(node.contentDescription?.toString(), node.isClickable, node.isEnabled, node.isScrollable)
            for (index in 0 until node.childCount) node.getChild(index)?.let {
                try { visit(it) } finally { it.recycle() }
            }
        }
        automation.rootInActiveWindow?.let { try { visit(it) } finally { it.recycle() } }
        return result
    }

    private fun clickAction(description: String): Boolean {
        fun visit(node: AccessibilityNodeInfo): Boolean? {
            if (node.contentDescription?.toString() == description)
                return node.performAction(AccessibilityNodeInfo.ACTION_CLICK)
            for (index in 0 until node.childCount) node.getChild(index)?.let {
                val result = try { visit(it) } finally { it.recycle() }
                if (result != null) return result
            }
            return null
        }
        val root = requireNotNull(automation.rootInActiveWindow)
        return try { requireNotNull(visit(root)) { "Missing native action: $description" } } finally { root.recycle() }
    }

    private fun injectTap(x: Float, y: Float) {
        val down = SystemClock.uptimeMillis()
        fun inject(action: Int) {
            val event = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, x, y, 0)
                .apply { source = InputDevice.SOURCE_TOUCHSCREEN }
            try { assertTrue("Platform touchscreen injection must succeed", automation.injectInputEvent(event, true)) }
            finally { event.recycle() }
        }
        inject(MotionEvent.ACTION_DOWN)
        SystemClock.sleep(50)
        inject(MotionEvent.ACTION_UP)
    }

    private fun await(message: String, condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + 15_000
        while (!condition()) {
            assertTrue(message, SystemClock.elapsedRealtime() < deadline)
            SystemClock.sleep(50)
        }
    }
}
