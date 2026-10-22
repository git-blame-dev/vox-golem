package dev.gitblame.voxgolem.android.wear.assistant

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.wear.compose.foundation.CurvedModifier
import androidx.wear.compose.foundation.clearAndSetSemantics
import androidx.wear.compose.material3.MaterialTheme
import androidx.wear.compose.material3.TimeSource
import androidx.wear.compose.material3.TimeText
import androidx.wear.compose.material3.TimeTextDefaults
import androidx.wear.compose.material3.curvedText
import dev.gitblame.voxgolem.assistant.runtime.AssistantModelPresentation
import kotlin.math.cos
import kotlin.math.sin

private object StaticHeaderTimeSource : TimeSource {
    @Composable override fun currentTime(): String = ""
}

/** Single-line header: compact family plus the full effort label after a middle dot; no mark when the model has no effort variant. */
internal fun headerLabel(model: AssistantModelPresentation?): String {
    val family = model?.compactFamilyLabel ?: "Model"
    val effort = model?.takeIf { it.effort != "none" }?.effortLabel?.takeIf { it.isNotEmpty() }
    return if (effort == null) family else "$family \u00B7 $effort"
}

@Composable
internal fun AssistantModelHeader(
    model: AssistantModelPresentation?,
    enabled: Boolean,
    openModels: () -> Unit,
    onExclusionHeight: (Dp) -> Unit,
) {
    val outerStyle = MaterialTheme.typography.arcSmall
    val foreground = MaterialTheme.colorScheme.onBackground
    val background = MaterialTheme.colorScheme.background.copy(alpha = 1f)
    val padding = TimeTextDefaults.ContentPadding
    val density = LocalDensity.current
    // CurvedLayoutInfo is internal in Wear 1.7.1; there is no public curved
    // positioning callback. Use the native fixed line-height token and sweep
    // to bound the single row, including the rounded arc caps, at the current font scale.
    val thickness = with(density) { outerStyle.lineHeight.toDp() }
    BoxWithConstraints(Modifier.fillMaxSize(), contentAlignment = Alignment.TopCenter) {
        val radius = minOf(maxWidth, maxHeight) / 2
        val halfSweep = Math.toRadians(TimeTextDefaults.MaxSweepAngle.toDouble() / 2)
        val innerRadius = maxOf(0.dp, radius - padding.calculateTopPadding() - thickness)
        val height = maxOf(48.dp, radius - innerRadius * cos(halfSweep).toFloat() + thickness / 2)
        val width = minOf(maxWidth, maxOf(48.dp, radius * (2 * sin(halfSweep)).toFloat() + thickness))
        LaunchedEffect(height) { onExclusionHeight(height) }

        TimeText(
            modifier = Modifier.fillMaxSize(),
            timeSource = remember { StaticHeaderTimeSource },
            backgroundColor = background,
            contentPadding = padding,
        ) { _ ->
            curvedText(
                headerLabel(model),
                modifier = CurvedModifier.clearAndSetSemantics {},
                style = outerStyle,
                color = foreground,
                overflow = TextOverflow.Ellipsis,
            )
        }
        // Only this bounded band handles input; the full-diameter native drawing
        // must not become a clickable overlay over conversation content.
        Box(Modifier.width(width).height(height)
            .clickable(enabled = enabled, role = Role.Button, onClick = openModels)
            .semantics {
                contentDescription = "Choose model: ${model?.fullLabel ?: "Model unavailable"}"
            })
    }
}
