package dev.gitblame.voxgolem.android.wear

import androidx.compose.runtime.Composable
import androidx.wear.compose.material3.AppScaffold
import androidx.wear.compose.material3.MaterialTheme

/** One Wear-specific theme/scaffold owner for the independent Assistant and Note shells. */
@Composable
internal fun WearAppTheme(content: @Composable () -> Unit) {
    MaterialTheme {
        AppScaffold { content() }
    }
}
