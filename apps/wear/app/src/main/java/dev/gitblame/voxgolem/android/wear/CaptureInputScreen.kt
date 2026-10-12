package dev.gitblame.voxgolem.android.wear

import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.wear.compose.material3.Icon
import androidx.wear.compose.material3.MaterialTheme
import androidx.wear.compose.material3.ScreenScaffold
import androidx.wear.compose.material3.Text

@Composable
internal fun CaptureInputScreen(transcript: String, stopEnabled: Boolean, onStop: () -> Unit, fontScale: Float = 1f) {
    val scroll = rememberLazyListState()
    val follow = remember { CaptureTranscriptFollow() }
    var measuredTranscript by remember { mutableStateOf<String?>(null) }
    var measuredHeight by remember { mutableStateOf(0) }
    var following by remember { mutableStateOf(false) }
    val baseStyle = MaterialTheme.typography.bodyMedium
    val style = wearTextStyle(baseStyle.copy(
        fontSize = baseStyle.fontSize * fontScale,
        lineHeight = baseStyle.lineHeight * fontScale,
    ))
    val textHeight = scroll.layoutInfo.visibleItemsInfo.firstOrNull()?.size ?: 0
    val viewportHeight = scroll.layoutInfo.viewportSize.height
    val bottomPadding = scroll.layoutInfo.afterContentPadding
    val manualScroll = scroll.isScrollInProgress && !following
    LaunchedEffect(transcript, measuredTranscript, measuredHeight, textHeight, viewportHeight,
        bottomPadding, manualScroll) {
        follow.observe(transcript)
        val distance = follow.distance(transcript, measuredTranscript, measuredHeight, textHeight,
            viewportHeight, bottomPadding, manualScroll) ?: return@LaunchedEffect
        following = true
        try {
            scroll.scrollBy(distance)
            follow.completed(transcript)
        } finally { following = false }
    }
    ScreenScaffold(
        scrollState = scroll,
        edgeButtonSpacing = 3.dp,
        edgeButton = {
            CompactEdgeButton(onClick = onStop, enabled = stopEnabled, contentDescription = "Stop recording") {
                Icon(
                    painter = painterResource(R.drawable.ic_stop),
                    contentDescription = "Stop recording",
                    modifier = Modifier.size(CompactEdgeButtonDefaults.IconSize),
                )
            }
        },
    ) { contentPadding ->
        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            state = scroll,
            contentPadding = contentPadding,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            item(key = "capture-transcript") {
                Text(
                    text = transcript,
                    color = MaterialTheme.colorScheme.onBackground,
                    textAlign = TextAlign.Center,
                    style = style,
                    overflow = TextOverflow.Clip,
                    maxLines = Int.MAX_VALUE,
                    onTextLayout = { measuredTranscript = transcript; measuredHeight = it.size.height },
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        }
    }
}
