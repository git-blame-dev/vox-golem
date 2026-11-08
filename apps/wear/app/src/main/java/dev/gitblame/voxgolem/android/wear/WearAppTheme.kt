package dev.gitblame.voxgolem.android.wear

import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.toArgb
import androidx.wear.compose.material3.AppScaffold
import androidx.wear.compose.material3.ColorScheme
import androidx.wear.compose.material3.MaterialTheme

/** Native defaults, also adapted for the View-based IME without introducing a phone palette. */
internal object WearAppColors {
    val scheme = ColorScheme()
    val background: Int get() = scheme.background.toArgb()
    val onBackground: Int get() = scheme.onBackground.toArgb()
    val onSurfaceVariant: Int get() = scheme.onSurfaceVariant.toArgb()
    val primary: Int get() = scheme.primary.toArgb()
    val onPrimary: Int get() = scheme.onPrimary.toArgb()
}

/** One Wear-specific theme/scaffold owner for the independent Assistant and Note shells. */
@Composable
internal fun WearAppTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = WearAppColors.scheme) {
        AppScaffold { content() }
    }
}
