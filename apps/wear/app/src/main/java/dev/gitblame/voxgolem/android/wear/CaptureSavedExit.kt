package dev.gitblame.voxgolem.android.wear

import android.os.SystemClock
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.tween
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.TransformOrigin
import androidx.compose.ui.graphics.graphicsLayer

internal const val CAPTURE_SAVED_EXIT_MILLIS = 250L

internal fun captureSavedExitProgress(deadlineMs: Long, nowMs: Long): Float =
    1f - (deadlineMs - nowMs).coerceIn(0, CAPTURE_SAVED_EXIT_MILLIS).toFloat() / CAPTURE_SAVED_EXIT_MILLIS

@Composable
internal fun CaptureSavedExit(deadlineMs: Long?, content: @Composable () -> Unit) {
    val progress = remember { Animatable(0f) }
    LaunchedEffect(deadlineMs) {
        if (deadlineMs == null) progress.snapTo(0f)
        else {
            val nowMs = SystemClock.elapsedRealtime()
            progress.snapTo(captureSavedExitProgress(deadlineMs, nowMs))
            val remainingMs = (deadlineMs - nowMs).coerceIn(0, CAPTURE_SAVED_EXIT_MILLIS)
            progress.animateTo(1f, tween(remainingMs.toInt(), easing = LinearEasing))
        }
    }
    Box(Modifier.fillMaxSize().graphicsLayer {
        val visibility = if (deadlineMs == null) 1f else 1f - FastOutSlowInEasing.transform(progress.value)
        transformOrigin = TransformOrigin.Center
        scaleX = visibility
        scaleY = visibility
        alpha = visibility
        clip = true
    }) { content() }
}
