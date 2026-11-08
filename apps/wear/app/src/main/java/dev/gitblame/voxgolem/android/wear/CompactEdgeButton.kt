package dev.gitblame.voxgolem.android.wear

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.wear.compose.material3.IconButton
import androidx.wear.compose.material3.IconButtonDefaults

internal object CompactEdgeButtonDefaults {
    val Height = 48.dp
    val BottomInset = 6.dp
    val OccupiedHeight = Height + BottomInset
    val IconSize = 19.2.dp
    val ViewportExclusion = OccupiedHeight + 3.dp

    fun readingBottomInset(screenHeight: Dp): Dp = maxOf(screenHeight * 0.10f, ViewportExclusion)
}

/** Persistent action override: one native semantic target, clear of the round bezel and reader. */
@Composable
internal fun CompactEdgeButton(
    onClick: () -> Unit,
    enabled: Boolean = true,
    contentDescription: String,
    content: @Composable () -> Unit,
) {
    // A scaffold EdgeButton collapses with scrolling. Use a full native 48dp target instead
    // of clipping its shape/semantics into the former 41.6dp resting height.
    IconButton(onClick = onClick, enabled = enabled,
        colors = IconButtonDefaults.filledIconButtonColors(),
        modifier = Modifier.padding(bottom = CompactEdgeButtonDefaults.BottomInset)
            .size(CompactEdgeButtonDefaults.Height).semantics {
                this.contentDescription = contentDescription
            }) {
        Box(Modifier.size(CompactEdgeButtonDefaults.IconSize).clearAndSetSemantics { },
            contentAlignment = Alignment.Center) { content() }
    }
}
