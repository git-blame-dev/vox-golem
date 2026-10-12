package dev.gitblame.voxgolem.android.wear

import android.graphics.Rect
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.compositeOver
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.wear.compose.material3.ButtonDefaults
import androidx.wear.compose.material3.LocalContentColor
import androidx.wear.compose.material3.MaterialTheme
import androidx.wear.compose.material3.ScreenScaffold
import androidx.wear.compose.material3.Text
import kotlinx.coroutines.flow.first
import kotlin.math.roundToInt

/** UI-only debug host: no stores, runtime, provider, recognizer, capture or permission requests. */
class CompactEdgeButtonFixtureActivity : ComponentActivity() {
    private var action by mutableStateOf(CompactFixtureAction.MIC)
    private var enabled by mutableStateOf(true)
    private var latest by mutableStateOf(true)
    private var clicks = 0
    private var edgeBounds = Rect()
    private var glyphBounds = Rect()
    private var textBounds = Rect()
    private var bottomPaddingPx = 0f
    private var expectedContentColor = 0
    private var expectedContainerColor = 0
    private var expectedPaintColor = 0
    private var suppliedContentColor = 0
    private var list: LazyListState? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        check(packageName == FIXTURE_PACKAGE) { "Compact edge fixture requires isolated .uxfixtures identity" }
        check(Build.HARDWARE == "ranchu" || Build.HARDWARE == "goldfish") {
            "Compact edge fixture is emulator-only"
        }
        super.onCreate(savedInstanceState)
        setContent { WearAppTheme { FixtureScreen() } }
    }

    fun show(action: CompactFixtureAction, enabled: Boolean = true) {
        this.action = action
        this.enabled = enabled
    }

    fun readBeginning() { latest = false }
    fun readLatest() { latest = true }

    fun snapshot() = CompactEdgeFixtureSnapshot(
        action, enabled, clicks, Rect(edgeBounds), Rect(glyphBounds), Rect(textBounds), bottomPaddingPx,
        expectedContentColor, expectedContainerColor, expectedPaintColor, suppliedContentColor,
        list?.canScrollForward ?: true, list?.isScrollInProgress ?: true,
    )

    @Composable
    private fun FixtureScreen() {
        val scroll = rememberLazyListState()
        list = scroll
        val density = LocalDensity.current
        val colors = ButtonDefaults.buttonColors()
        val content = if (enabled) colors.contentColor else colors.disabledContentColor
        val container = (if (enabled) colors.containerColor else colors.disabledContainerColor)
            .compositeOver(MaterialTheme.colorScheme.background)
        SideEffect {
            expectedContentColor = content.toArgb()
            expectedContainerColor = container.toArgb()
            expectedPaintColor = content.compositeOver(container).toArgb()
        }
        // Re-anchor after scaffold measurement changes so the terminal text keeps real clearance.
        LaunchedEffect(latest, scroll.layoutInfo.afterContentPadding) {
            snapshotFlow { scroll.layoutInfo.totalItemsCount }.first { it > 0 }
            scroll.scrollToItem(if (latest) scroll.layoutInfo.totalItemsCount - 1 else 0)
        }
        Box(Modifier.fillMaxSize().background(MaterialTheme.colorScheme.background)) {
            ScreenScaffold(scrollState = scroll, edgeButtonSpacing = 3.dp, edgeButton = {
                Box(Modifier.fillMaxWidth().onGloballyPositioned { edgeBounds = it.boundsInWindow().pixelRect() }) {
                    CompactEdgeButton(onClick = { clicks++ }, enabled = enabled, contentDescription = action.description) {
                        FixtureGlyph(action)
                    }
                }
            }) { padding ->
                SideEffect { bottomPaddingPx = with(density) { padding.calculateBottomPadding().toPx() } }
                LazyColumn(Modifier.fillMaxSize(), state = scroll, contentPadding = padding,
                    verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    item { Text(SYNTHETIC_ANSWER, style = wearTextStyle(MaterialTheme.typography.bodyLarge)) }
                    item {
                        Text(END_TEXT, style = wearTextStyle(MaterialTheme.typography.bodyLarge),
                            modifier = Modifier.fillMaxWidth().onGloballyPositioned { textBounds = it.boundsInWindow().pixelRect() })
                    }
                    item { Spacer(Modifier.height(0.dp)) }
                }
            }
        }
    }

    @Composable
    private fun FixtureGlyph(action: CompactFixtureAction) {
        val color = LocalContentColor.current
        SideEffect { suppliedContentColor = color.toArgb() }
        Canvas(Modifier.size(CompactEdgeButtonDefaults.IconSize)
            .onGloballyPositioned { glyphBounds = it.boundsInWindow().pixelRect() }
            // Deliberately supply child semantics: the production decorative overlay must clear it.
            .semantics { contentDescription = DECORATIVE_DESCRIPTION }) {
            val unit = size.width / 24f
            when (action) {
                CompactFixtureAction.MIC -> {
                    val stroke = Stroke(1.8f * unit)
                    drawRoundRect(color, Offset(9 * unit, 2 * unit), Size(6 * unit, 12 * unit), CornerRadius(3 * unit), style = stroke)
                    drawPath(Path().apply {
                        moveTo(6 * unit, 10 * unit); lineTo(6 * unit, 12 * unit)
                        cubicTo(6 * unit, 20 * unit, 18 * unit, 20 * unit, 18 * unit, 12 * unit)
                        lineTo(18 * unit, 10 * unit)
                    }, color, style = stroke)
                    drawLine(color, Offset(12 * unit, 18 * unit), Offset(12 * unit, 22 * unit), 1.8f * unit)
                    drawLine(color, Offset(8 * unit, 22 * unit), Offset(16 * unit, 22 * unit), 1.8f * unit)
                }
                CompactFixtureAction.STOP, CompactFixtureAction.NOTE_STOP ->
                    drawRect(color, Offset(3 * unit, 3 * unit), Size(18 * unit, 18 * unit))
                CompactFixtureAction.SEND -> drawPath(Path().apply {
                    moveTo(3 * unit, 3 * unit); lineTo(21 * unit, 12 * unit)
                    lineTo(3 * unit, 21 * unit); lineTo(7 * unit, 12 * unit); close()
                }, color)
            }
        }
    }

    companion object {
        const val FIXTURE_PACKAGE = "dev.gitblame.voxgolem.android.uxfixtures"
        const val DECORATIVE_DESCRIPTION = "Synthetic decorative compact glyph"
        const val END_TEXT = "Synthetic answer end."
        private val SYNTHETIC_ANSWER = (1..32).joinToString("\n\n") {
            "Synthetic garden paragraph $it. A paper robot waters imaginary flowers. This UI fixture has no private history or live service."
        }
    }
}

enum class CompactFixtureAction(val description: String) {
    MIC("Synthetic compact mic"),
    STOP("Synthetic compact stop"),
    SEND("Synthetic compact send"),
    NOTE_STOP("Synthetic compact note stop"),
}

data class CompactEdgeFixtureSnapshot(
    val action: CompactFixtureAction,
    val enabled: Boolean,
    val clicks: Int,
    val edgeBounds: Rect,
    val glyphBounds: Rect,
    val textBounds: Rect,
    val bottomPaddingPx: Float,
    val expectedContentColor: Int,
    val expectedContainerColor: Int,
    val expectedPaintColor: Int,
    val suppliedContentColor: Int,
    val canScrollForward: Boolean,
    val scrolling: Boolean,
)

private fun androidx.compose.ui.geometry.Rect.pixelRect() =
    Rect(left.roundToInt(), top.roundToInt(), right.roundToInt(), bottom.roundToInt())
