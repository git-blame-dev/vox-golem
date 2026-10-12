package dev.gitblame.voxgolem.android.wear

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableFloatStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import androidx.wear.compose.material3.ButtonDefaults
import androidx.wear.compose.material3.EdgeButton
import androidx.wear.compose.material3.EdgeButtonDefaults
import androidx.wear.compose.material3.EdgeButtonSize
import androidx.wear.compose.material3.LocalContentColor

internal object CompactEdgeButtonDefaults {
    private const val Scale = 0.8f
    // ExtraSmall's nominal 46dp shape includes 3dp padding above and below.
    val Height = (52f * Scale).dp
    val IconSize = EdgeButtonDefaults.ExtraSmallIconSize * Scale

    fun iconAlpha(renderedHeight: Float, expandedHeight: Float): Float {
        if (expandedHeight <= 0f) return 0f
        return ((renderedHeight / expandedHeight - 0.7f) / 0.3f).coerceIn(0f, 1f)
    }
}

@Composable
internal fun CompactEdgeButton(
    onClick: () -> Unit,
    enabled: Boolean = true,
    contentDescription: String,
    content: @Composable () -> Unit,
) {
    val colors = ButtonDefaults.buttonColors()
    val expandedHeightPx = with(LocalDensity.current) { CompactEdgeButtonDefaults.Height.toPx() }
    var renderedHeightPx by remember { mutableFloatStateOf(0f) }
    Box(Modifier.height(CompactEdgeButtonDefaults.Height).clipToBounds()
        .onSizeChanged { renderedHeightPx = it.height.toFloat() }) {
        EdgeButton(onClick = onClick, enabled = enabled, colors = colors,
            buttonSize = EdgeButtonSize.ExtraSmall, modifier = Modifier.fillMaxSize().semantics {
                this.contentDescription = contentDescription
            }) {
            Spacer(Modifier.size(CompactEdgeButtonDefaults.IconSize))
        }
        // Native content fading interprets our resting size as collapsed; overlay only the glyph.
        CompositionLocalProvider(LocalContentColor provides
            if (enabled) colors.contentColor else colors.disabledContentColor) {
            Box(Modifier.align(Alignment.Center).offset(y = (-1).dp)
                .size(CompactEdgeButtonDefaults.IconSize).graphicsLayer {
                    alpha = CompactEdgeButtonDefaults.iconAlpha(renderedHeightPx, expandedHeightPx)
                }.clearAndSetSemantics { }) {
                content()
            }
        }
    }
}
