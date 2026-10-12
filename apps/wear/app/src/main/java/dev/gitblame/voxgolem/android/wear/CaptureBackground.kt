package dev.gitblame.voxgolem.android.wear

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.MotionDurationScale
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Shadow
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import kotlin.math.min
import kotlin.math.sqrt

internal const val CAPTURE_SAVED_FEEDBACK_MILLIS = 4_000L
internal const val ARTWORK_ZOOM_MILLIS = 1_500
internal const val ARTWORK_ROTATION_MILLIS = 8_533
internal const val ARTWORK_PHASE_ZOOM = 2f
internal const val WEAR_CONTROL_TARGET_DP = 48
internal const val WEAR_CONTROL_VISUAL_DP = 26.8f

internal fun wearPolarControlInset(width: Float, height: Float, round: Boolean): Float {
    if (!round) return 4f
    val radius = min(width, height) / 2f
    val halfTarget = WEAR_CONTROL_TARGET_DP / 2f
    val safeRadius = (radius - 2f).coerceAtLeast(halfTarget)
    return height / 2f - sqrt(safeRadius * safeRadius - halfTarget * halfTarget)
}

@Composable
internal fun wearTextStyle(base: TextStyle): TextStyle {
    val density = LocalDensity.current
    return base.copy(shadow = Shadow(Color.Black, Offset(0f, with(density) { 1.dp.toPx() }), with(density) { 3.dp.toPx() }))
}

internal fun artworkBaseFit(widthPx: Float, heightPx: Float, inkFraction: Float): Float {
    require(inkFraction > 0f && inkFraction <= 1f)
    val diameter = min(widthPx, heightPx)
    return if (diameter <= 4f) 1f else (diameter - 4f) / (inkFraction * diameter)
}

@Composable
internal fun CaptureBackground(listening: Boolean, saved: Boolean = false) {
    val rotation = remember { Animatable(0f) }
    val zoom = remember { Animatable(1f) }
    LaunchedEffect(listening, saved) {
        val motion = coroutineContext[MotionDurationScale]
        snapshotFlow { motion?.scaleFactor ?: 1f }.collectLatest { durationScale ->
            if (durationScale <= 0f) { rotation.snapTo(0f); zoom.snapTo(1f) }
            else if (saved) coroutineScope {
                launch { rotation.animateTo(rotation.value + 360f * CAPTURE_SAVED_FEEDBACK_MILLIS / ARTWORK_ROTATION_MILLIS,
                    tween(CAPTURE_SAVED_FEEDBACK_MILLIS.toInt(), easing = LinearEasing)) }
                launch { zoom.animateTo(ARTWORK_PHASE_ZOOM, tween(ARTWORK_ZOOM_MILLIS, easing = FastOutSlowInEasing)) }
            } else {
                zoom.snapTo(1f)
                if (listening) rotation.animateTo(
                    targetValue = rotation.value - 360f,
                    animationSpec = infiniteRepeatable(tween(ARTWORK_ROTATION_MILLIS, easing = LinearEasing)),
                )
                else rotation.snapTo(0f)
            }
        }
    }
    CaptureArtwork(Modifier.graphicsLayer { rotationZ = rotation.value; scaleX = zoom.value; scaleY = zoom.value })
}

@Composable
internal fun CaptureArtwork(modifier: Modifier = Modifier) {
    val context = LocalContext.current
    val density = LocalDensity.current
    val inkFraction = context.resources.getFraction(R.fraction.artwork_ink_fraction, 1, 1)
    BoxWithConstraints(Modifier.fillMaxSize().clipToBounds(), contentAlignment = Alignment.Center) {
        val diameter = minOf(maxWidth, maxHeight)
        val fit = artworkBaseFit(with(density) { maxWidth.toPx() }, with(density) { maxHeight.toPx() }, inkFraction)
        Image(
            painter = painterResource(R.drawable.capture_background),
            contentDescription = null,
            contentScale = ContentScale.Fit,
            modifier = Modifier.size(diameter).graphicsLayer { scaleX = fit; scaleY = fit }.then(modifier),
        )
    }
}
