package dev.gitblame.voxgolem.android.wear.assistant

import androidx.compose.animation.core.Animatable
import androidx.compose.animation.core.FastOutSlowInEasing
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.tween
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.MotionDurationScale
import androidx.compose.ui.graphics.graphicsLayer
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.repeatOnLifecycle
import dev.gitblame.voxgolem.android.wear.ARTWORK_ROTATION_MILLIS
import dev.gitblame.voxgolem.android.wear.ARTWORK_ZOOM_MILLIS
import dev.gitblame.voxgolem.android.wear.ARTWORK_PHASE_ZOOM
import dev.gitblame.voxgolem.android.wear.CaptureArtwork
import dev.gitblame.voxgolem.assistant.runtime.AssistantState
import dev.gitblame.voxgolem.assistant.recognition.DictationPhase
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

internal enum class AssistantArtworkPhase { IDLE, LISTENING, GENERATING, RETURNING }

internal fun assistantArtworkListening(voice: AssistantVoiceState): Boolean =
    voice.dictation.phase == DictationPhase.LISTENING && voice.dictation.readyForSpeech

internal class AssistantArtworkTransition {
    private var initialized = false
    private var wasBusy = false
    private var startingHead: String? = null
    private var phase = AssistantArtworkPhase.IDLE

    fun update(state: AssistantState, active: Boolean, listening: Boolean = false): AssistantArtworkPhase {
        if (!active || !state.foreground || (!listening && (state.error != null || state.interrupted))) {
            initialized = true
            phase = AssistantArtworkPhase.IDLE
        } else if (!initialized) {
            initialized = true
            phase = if (listening && !state.busy) AssistantArtworkPhase.LISTENING else AssistantArtworkPhase.IDLE
        } else if (state.busy && !wasBusy) {
            startingHead = state.selectedHead
            phase = AssistantArtworkPhase.GENERATING
        } else if (!state.busy && listening) {
            phase = AssistantArtworkPhase.LISTENING
        } else if (!state.busy && wasBusy) {
            phase = if (phase == AssistantArtworkPhase.GENERATING && state.selectedHead != null && state.selectedHead != startingHead)
                AssistantArtworkPhase.RETURNING else AssistantArtworkPhase.IDLE
        } else if (!state.busy && phase != AssistantArtworkPhase.RETURNING) {
            phase = AssistantArtworkPhase.IDLE
        }
        wasBusy = state.busy
        return phase
    }

    fun returned() { phase = AssistantArtworkPhase.IDLE }
    fun suspended(busy: Boolean) { initialized = true; wasBusy = busy; phase = AssistantArtworkPhase.IDLE }
}

@Composable
internal fun AssistantArtwork(state: AssistantState, active: Boolean, listening: Boolean = false) {
    val transition = remember { AssistantArtworkTransition() }
    var phase by remember { mutableStateOf(AssistantArtworkPhase.IDLE) }
    val rotation = remember { Animatable(0f) }
    val zoom = remember { Animatable(1f) }
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    val latestState by rememberUpdatedState(state)
    LaunchedEffect(state.busy, state.selectedHead, state.foreground, state.error, state.interrupted, active, listening) {
        phase = transition.update(state, active, listening)
    }
    LaunchedEffect(lifecycle) {
        lifecycle.repeatOnLifecycle(Lifecycle.State.RESUMED) {
            try {
                val motion = coroutineContext[MotionDurationScale]
                snapshotFlow { phase to (motion?.scaleFactor ?: 1f) }.collectLatest { (animationPhase, durationScale) ->
                    if (durationScale <= 0f) {
                        rotation.snapTo(0f); zoom.snapTo(1f)
                        if (animationPhase == AssistantArtworkPhase.RETURNING) {
                            transition.returned()
                            phase = AssistantArtworkPhase.IDLE
                        }
                    } else when (animationPhase) {
                        AssistantArtworkPhase.IDLE -> { rotation.snapTo(0f); zoom.snapTo(1f) }
                        AssistantArtworkPhase.LISTENING -> {
                            zoom.snapTo(1f)
                            rotation.animateTo(rotation.value - 360f, infiniteRepeatable(tween(ARTWORK_ROTATION_MILLIS, easing = LinearEasing)))
                        }
                        AssistantArtworkPhase.GENERATING -> coroutineScope {
                            launch { zoom.animateTo(ARTWORK_PHASE_ZOOM, tween(ARTWORK_ZOOM_MILLIS, easing = FastOutSlowInEasing)) }
                            launch { rotation.animateTo(360f, infiniteRepeatable(tween(ARTWORK_ROTATION_MILLIS, easing = LinearEasing))) }
                        }
                        AssistantArtworkPhase.RETURNING -> {
                            coroutineScope {
                                launch { rotation.animateTo(0f, tween(ARTWORK_ZOOM_MILLIS, easing = FastOutSlowInEasing)) }
                                launch { zoom.animateTo(1f, tween(ARTWORK_ZOOM_MILLIS, easing = FastOutSlowInEasing)) }
                            }
                            transition.returned()
                            phase = AssistantArtworkPhase.IDLE
                        }
                    }
                }
            } finally {
                transition.suspended(latestState.busy)
                phase = AssistantArtworkPhase.IDLE
                withContext(NonCancellable) { rotation.snapTo(0f); zoom.snapTo(1f) }
            }
        }
    }
    CaptureArtwork(Modifier.graphicsLayer { rotationZ = rotation.value; scaleX = zoom.value; scaleY = zoom.value })
}
