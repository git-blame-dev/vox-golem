package dev.gitblame.voxgolem.android.wear.assistant

import android.app.Activity
import android.content.Intent
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import android.os.Bundle
import android.view.ViewGroup
import android.view.WindowInsets
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import dev.gitblame.voxgolem.android.wear.WearAppColors
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35], qualifiers = "w228dp-h228dp-round")
class AssistantTextInputTest {
    private fun editorIntent() = Intent(RuntimeEnvironment.getApplication(), AssistantTextEntryActivity::class.java)
        .putExtra(AssistantTextInput.DRAFT, "Synthetic old draft")

    @Test fun nativeEditorKeepsBlackBackgroundAndReadableThemeRolePairs() {
        assertEquals(Color.BLACK, WearAppColors.background)
        for ((foreground, background) in listOf(
            WearAppColors.onBackground to WearAppColors.background,
            WearAppColors.onSurfaceVariant to WearAppColors.background,
            WearAppColors.onPrimary to WearAppColors.primary,
        )) {
            val a = Color.luminance(foreground)
            val b = Color.luminance(background)
            val contrast = (maxOf(a, b) + 0.05f) / (minOf(a, b) + 0.05f)
            assertTrue("Native editor text contrast is $contrast", contrast >= 4.5f)
        }
    }

    @Test fun nativeKeyboardEditorPrefillsDraftUsesResizeAndWearThemeRoles() {
        val controller = Robolectric.buildActivity(AssistantTextEntryActivity::class.java, editorIntent()).setup()
        val activity = controller.get()
        val input = activity.findViewById<EditText>(android.R.id.edit)
        val button = activity.findViewById<Button>(android.R.id.button1)
        assertEquals("Synthetic old draft", input.text.toString())
        assertEquals(input.text.length, input.selectionStart)
        assertEquals(WearAppColors.onBackground, input.currentTextColor)
        assertEquals(WearAppColors.onSurfaceVariant, input.currentHintTextColor)
        assertEquals(WearAppColors.primary, input.backgroundTintList!!.defaultColor)
        assertEquals("Use text", button.text.toString())
        assertTrue(button.isShown)
        assertEquals(WearAppColors.onPrimary, button.currentTextColor)
        assertEquals(WearAppColors.primary, button.backgroundTintList!!.defaultColor)
        assertTrue(button.minHeight >= (48 * activity.resources.displayMetrics.density).toInt())
        val root = activity.findViewById<ViewGroup>(android.R.id.content).getChildAt(0) as LinearLayout
        assertEquals(WearAppColors.background, (root.background as ColorDrawable).color)
        assertEquals(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE,
            activity.window.attributes.softInputMode and WindowManager.LayoutParams.SOFT_INPUT_MASK_ADJUST)
        assertEquals(EditorInfo.IME_FLAG_NO_EXTRACT_UI, input.imeOptions and EditorInfo.IME_FLAG_NO_EXTRACT_UI)
        val normalPadding = root.paddingBottom
        root.dispatchApplyWindowInsets(WindowInsets.Builder().setVisible(WindowInsets.Type.ime(), true).build())
        assertTrue(root.paddingBottom < normalPadding)
        assertTrue(button.isShown)
        controller.pause().stop().destroy()
    }

    @Test fun useTextReturnsEditedDraftAndDuplicateConfirmationCannotReplaceIt() {
        val controller = Robolectric.buildActivity(AssistantTextEntryActivity::class.java, editorIntent()).setup()
        val activity = controller.get()
        val input = activity.findViewById<EditText>(android.R.id.edit)
        val button = activity.findViewById<Button>(android.R.id.button1)
        input.setText("Synthetic reviewed draft")
        button.performClick()
        val shadow = Shadows.shadowOf(activity)
        assertEquals(Activity.RESULT_OK, shadow.resultCode)
        val session = AssistantImeSession(true)
        assertEquals("Synthetic reviewed draft", session.accept(shadow.resultCode, shadow.resultIntent))
        input.setText("Synthetic late edit")
        button.performClick()
        val secondSession = AssistantImeSession(true)
        assertEquals("Synthetic reviewed draft", secondSession.accept(shadow.resultCode, shadow.resultIntent))
        assertTrue(activity.isFinishing)
        controller.pause().stop().destroy()
    }

    @Test fun recreationRetainsUnsavedEditorTextWhileCancelReturnsNoDraft() {
        val controller = Robolectric.buildActivity(AssistantTextEntryActivity::class.java, editorIntent()).setup()
        controller.get().findViewById<EditText>(android.R.id.edit).setText("Synthetic partial edit")
        val saved = Bundle()
        controller.saveInstanceState(saved).pause().stop().destroy()
        val restored = Robolectric.buildActivity(AssistantTextEntryActivity::class.java, editorIntent()).create(saved).start().resume().visible()
        val activity = restored.get()
        assertEquals("Synthetic partial edit", activity.findViewById<EditText>(android.R.id.edit).text.toString())
        activity.finish()
        val shadow = Shadows.shadowOf(activity)
        assertEquals(Activity.RESULT_CANCELED, shadow.resultCode)
        assertNull(AssistantImeSession(true).accept(shadow.resultCode, shadow.resultIntent))
        restored.pause().stop().destroy()
    }

    @Test fun nativeImeDoneConfirmsOnlyTheDraftThroughTheSameResultContract() {
        val controller = Robolectric.buildActivity(AssistantTextEntryActivity::class.java, editorIntent()).setup()
        val activity = controller.get()
        activity.findViewById<EditText>(android.R.id.edit).apply {
            setText("Synthetic IME confirmed draft")
            onEditorAction(EditorInfo.IME_ACTION_DONE)
        }
        val shadow = Shadows.shadowOf(activity)
        assertEquals("Synthetic IME confirmed draft", AssistantImeSession(true).accept(shadow.resultCode, shadow.resultIntent))
        assertTrue(activity.isFinishing)
        controller.pause().stop().destroy()
    }

    @Test fun utf8BudgetRejectsWithoutTruncatingOrClosingAndRetainsRejectedTextAcrossRecreation() {
        assertTrue(AssistantTextInput.accepts("a".repeat(32 * 1024)))
        assertTrue(AssistantTextInput.accepts("é".repeat(16 * 1024)))
        assertTrue(AssistantTextInput.accepts("😀".repeat(8 * 1024)))
        assertFalse(AssistantTextInput.accepts("é".repeat(16 * 1024 + 1)))
        assertFalse(AssistantTextInput.accepts("😀".repeat(8 * 1024 + 1)))
        val controller = Robolectric.buildActivity(AssistantTextEntryActivity::class.java, editorIntent()).setup()
        val rejected = "Synthetic rejected UTF-8 draft " + "😀".repeat(9 * 1024)
        val input = controller.get().findViewById<EditText>(android.R.id.edit)
        try {
            input.setText(rejected)
            controller.get().findViewById<Button>(android.R.id.button1).performClick()
            assertFalse(controller.get().isFinishing)
            assertEquals(rejected, input.text.toString())
            assertNotNull(input.error)
            assertEquals(Activity.RESULT_CANCELED, Shadows.shadowOf(controller.get()).resultCode)
            val saved = Bundle()
            controller.saveInstanceState(saved)
            val restored = Robolectric.buildActivity(AssistantTextEntryActivity::class.java, editorIntent()).create(saved).start().resume().visible()
            try {
                assertEquals(rejected, restored.get().findViewById<EditText>(android.R.id.edit).text.toString())
                restored.get().findViewById<EditText>(android.R.id.edit).setText("Synthetic shortened accepted draft")
                restored.get().findViewById<Button>(android.R.id.button1).performClick()
                assertEquals(Activity.RESULT_OK, Shadows.shadowOf(restored.get()).resultCode)
            } finally { restored.pause().stop().destroy() }
        } finally { controller.pause().stop().destroy() }
    }
}
