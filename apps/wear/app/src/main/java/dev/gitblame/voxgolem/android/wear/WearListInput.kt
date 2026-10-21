package dev.gitblame.voxgolem.android.wear

import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.input.rotary.onPreRotaryScrollEvent
import androidx.wear.compose.foundation.LocalScreenIsActive
import androidx.wear.compose.foundation.hierarchicalFocusGroup
import androidx.wear.compose.foundation.requestFocusOnHierarchyActive
import androidx.wear.compose.foundation.rotary.RotaryScrollableDefaults
import androidx.wear.compose.foundation.rotary.rotaryScrollable

@Composable
internal fun Modifier.listRotary(state: LazyListState, active: Boolean = true, scrolled: (Float) -> Unit = {}): Modifier {
    val focus = remember { FocusRequester() }
    val screenActive = active && LocalScreenIsActive.current
    return hierarchicalFocusGroup(screenActive).requestFocusOnHierarchyActive().onPreRotaryScrollEvent {
        if (screenActive) scrolled(it.verticalScrollPixels)
        false
    }.rotaryScrollable(behavior = RotaryScrollableDefaults.behavior(state), focusRequester = focus)
}
