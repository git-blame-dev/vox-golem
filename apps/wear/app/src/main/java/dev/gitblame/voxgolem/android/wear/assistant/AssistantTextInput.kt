package dev.gitblame.voxgolem.android.wear.assistant

import android.app.Activity
import android.app.RemoteInput
import android.content.Intent
import android.content.res.ColorStateList
import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import android.os.Bundle
import android.view.Gravity
import android.view.WindowInsets
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout

internal object AssistantTextInput {
    const val KEY = "assistant-draft"
    const val DRAFT = "assistant.initial-draft"
    fun result(text: String): Intent = Intent().also { result ->
        RemoteInput.addResultsToIntent(arrayOf(RemoteInput.Builder(KEY).build()), result, Bundle().apply { putCharSequence(KEY, text) })
    }
}

internal class AssistantImeSession(var pending: Boolean = false) {
    fun begin(): Boolean { if (pending) return false; pending = true; return true }
    fun accept(resultCode: Int, intent: Intent?): String? {
        if (!pending) return null
        pending = false
        if (resultCode != Activity.RESULT_OK || intent == null) return null
        return RemoteInput.getResultsFromIntent(intent)?.getCharSequence(AssistantTextInput.KEY)?.toString()
    }
}

/** Native keyboard entry edits a draft; confirmation never submits a provider request. */
class AssistantTextEntryActivity : Activity() {
    private lateinit var input: EditText
    private var keyboardRequested = false
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.setBackgroundDrawable(ColorDrawable(Color.BLACK))
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_VISIBLE or WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        val round = resources.configuration.isScreenRound
        fun pixels(dp: Int) = (dp * resources.displayMetrics.density).toInt()
        val horizontal = pixels(if (round) 30 else 12)
        val top = pixels(if (round) 30 else 12)
        val bottom = pixels(if (round) 30 else 8)
        input = EditText(this).apply {
            id = android.R.id.edit
            hint = "Assistant draft"
            setText(savedInstanceState?.getString(AssistantTextInput.DRAFT) ?: intent.getStringExtra(AssistantTextInput.DRAFT).orEmpty())
            inputType = android.text.InputType.TYPE_CLASS_TEXT or android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE
            imeOptions = EditorInfo.IME_ACTION_DONE or EditorInfo.IME_FLAG_NO_EXTRACT_UI
            gravity = Gravity.TOP or Gravity.START
            setTextColor(Color.WHITE)
            setHintTextColor(Color.LTGRAY)
            setTextSize(android.util.TypedValue.COMPLEX_UNIT_SP, 16f)
            backgroundTintList = ColorStateList.valueOf(Color.WHITE)
            filters = arrayOf(android.text.InputFilter.LengthFilter(32 * 1024))
            setSelection(text.length)
            setOnEditorActionListener { _, action, _ ->
                if (action == EditorInfo.IME_ACTION_DONE) { confirmDraft(); true } else false
            }
        }
        val button = Button(this).apply {
            id = android.R.id.button1
            text = "Use text"
            isAllCaps = false
            minHeight = pixels(48)
            setTextColor(Color.BLACK)
            backgroundTintList = ColorStateList.valueOf(Color.WHITE)
            setOnClickListener { confirmDraft() }
        }
        setContentView(LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.BLACK)
            setPadding(horizontal, top, horizontal, bottom)
            setOnApplyWindowInsetsListener { view, insets ->
                view.setPadding(horizontal, top, horizontal, if (insets.isVisible(WindowInsets.Type.ime())) pixels(8) else bottom)
                insets
            }
            addView(input, LinearLayout.LayoutParams(-1, 0, 1f))
            addView(button, LinearLayout.LayoutParams(-2, -2).apply { gravity = Gravity.CENTER_HORIZONTAL })
        })
        input.requestFocus()
    }
    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus && !keyboardRequested && !isFinishing) {
            keyboardRequested = true
            input.post { if (hasWindowFocus() && !isFinishing) window.insetsController?.show(WindowInsets.Type.ime()) }
        }
    }
    private fun confirmDraft() {
        if (isFinishing) return
        setResult(RESULT_OK, AssistantTextInput.result(input.text.toString()))
        finish()
    }
    override fun onSaveInstanceState(outState: Bundle) {
        outState.putString(AssistantTextInput.DRAFT, input.text.toString())
        super.onSaveInstanceState(outState)
    }
}
