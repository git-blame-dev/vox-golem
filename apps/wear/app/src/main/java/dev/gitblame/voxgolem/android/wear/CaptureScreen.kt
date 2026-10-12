package dev.gitblame.voxgolem.android.wear

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.wear.compose.material3.Button
import androidx.wear.compose.material3.FilledTonalButton
import androidx.wear.compose.material3.MaterialTheme
import androidx.wear.compose.material3.ScreenScaffold
import androidx.wear.compose.material3.Text
import dev.gitblame.voxgolem.android.wear.capture.CaptureSnapshot
import dev.gitblame.voxgolem.capture.CaptureState

@Composable
internal fun CaptureScreen(
    snapshot: CaptureSnapshot,
    feedback: String,
    terminalSaveFailure: Boolean,
    onStart: () -> Unit,
    onStop: () -> Unit,
    stopEnabled: Boolean,
    onClose: () -> Unit,
    motionStopEnabled: Boolean,
    calibrationPrompt: String?,
    calibrationCanMeasure: Boolean,
    calibrationSampleCount: Int,
    onMotionAction: () -> Unit,
    onManualCapture: () -> Unit,
    onRetryRecording: (dev.gitblame.voxgolem.capture.CaptureRecord) -> Unit,
    preparing: Boolean = false,
    observationError: String? = null,
) {
    val visibleFeedback = listOfNotNull(captureNoteFeedbackText(feedback).takeIf { it.isNotBlank() }, observationError)
        .joinToString("\n")
    val active = snapshot.active
    if (preparing) {
        CaptureInputScreen(capturePreparationText(observationError), true, onStop)
        return
    }
    if (active != null && !terminalSaveFailure) {
        CaptureInputScreen(if (active.captureState == CaptureState.PREPARING) "Preparing microphone…"
            else if (stopEnabled) listOf(captureLiveStatusText(active), active.previewText.orEmpty()).filter(String::isNotBlank).joinToString("\n\n")
            else "Saving…", stopEnabled, onStop,
            fontScale = if (stopEnabled) 1f else 2f)
        return
    }
    val scroll = rememberLazyListState()
    ScreenScaffold(scrollState = scroll) { contentPadding ->
        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            state = scroll,
            contentPadding = contentPadding,
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            if (active != null && terminalSaveFailure) {
                item(key = "save-failure") { CaptureNoteText(visibleFeedback) }
                item(key = "close") {
                    Button(onClick = onClose, modifier = Modifier.fillMaxWidth(), label = { Text("Close") })
                }
            } else {
                item(key = "title") {
                    Text("VoxGolem", color = MaterialTheme.colorScheme.onBackground,
                        style = wearTextStyle(MaterialTheme.typography.titleMedium), textAlign = TextAlign.Center)
                }
                item(key = "feedback") { CaptureNoteText(visibleFeedback.ifBlank { "Ready for voice capture" }) }
                if (calibrationPrompt != null) {
                    item(key = "calibration-prompt") { CaptureNoteText(calibrationPrompt) }
                    item(key = "calibration-samples") { CaptureNoteText("$calibrationSampleCount / 30 samples") }
                    if (calibrationCanMeasure) {
                        item(key = "measure-posture") {
                            Button(onClick = onMotionAction, modifier = Modifier.fillMaxWidth(),
                                label = { Text("Measure posture") }, secondaryLabel = { Text("Starts after 2 seconds") })
                        }
                    }
                    item(key = "manual-capture") {
                        FilledTonalButton(onClick = onManualCapture, modifier = Modifier.fillMaxWidth(),
                            label = { Text("Record manually") }, secondaryLabel = { Text("Stop button only") })
                    }
                } else {
                    item(key = "start-capture") {
                        Button(onClick = onStart, modifier = Modifier.fillMaxWidth(), label = { Text("Capture") })
                    }
                    item(key = "motion-stop") {
                        FilledTonalButton(onClick = onMotionAction, modifier = Modifier.fillMaxWidth(),
                            label = { Text(if (motionStopEnabled) "Disable motion stop" else "Calibrate arm-down stop") })
                    }
                    if (feedback.contains("saved", ignoreCase = true)) {
                        item(key = "done") {
                            FilledTonalButton(onClick = onClose, modifier = Modifier.fillMaxWidth(), label = { Text("Done") })
                        }
                    }
                    item(key = "recent-heading") { CaptureNoteText("Recent captures") }
                    items(snapshot.recent.take(5), key = { it.captureId }) { item ->
                        Column(Modifier.fillMaxWidth(), horizontalAlignment = Alignment.CenterHorizontally,
                            verticalArrangement = Arrangement.spacedBy(4.dp)) {
                            CaptureNoteText("${item.finishedLocalDate ?: item.startedAtUtc?.take(10) ?: item.createdAtUtc.take(10)} · ${item.captureState.name.lowercase()} · ${item.transferState.name.lowercase()}")
                            if (item.finalText == null) CaptureNoteText("Phone transcription: ${item.liveTranscriptionState.name.lowercase()}")
                            item.liveErrorCode?.let { CaptureNoteText("Audio retained · ${dev.gitblame.voxgolem.capture.CaptureErrorCodes.safe(it)}") }
                            (item.finalText ?: item.previewText)?.takeIf { it.isNotBlank() }?.let { transcript ->
                                if (item.finalText == null) CaptureNoteText("Draft retained · not finalized")
                                CaptureNoteText(transcript)
                            }
                            captureNoteErrorText(item.lastErrorCode)?.let { CaptureNoteText(it) }
                            if (item.captureState == CaptureState.INTERRUPTED && item.finalizedByteLength == null) {
                                FilledTonalButton(onClick = { onRetryRecording(item) }, modifier = Modifier.fillMaxWidth(),
                                    label = { Text("Retry recording repair") })
                            }
                        }
                    }
                }
            }
        }
    }
}

internal fun capturePreparationText(observationError: String?): String =
    "Preparing microphone…" + (observationError?.let { "\n$it" } ?: "")

@Composable
private fun CaptureNoteText(text: String) {
    Text(
        text = text,
        color = MaterialTheme.colorScheme.onBackground,
        modifier = Modifier.fillMaxWidth(),
        textAlign = TextAlign.Center,
        style = wearTextStyle(MaterialTheme.typography.bodyMedium),
        overflow = TextOverflow.Clip,
        maxLines = Int.MAX_VALUE,
    )
}
