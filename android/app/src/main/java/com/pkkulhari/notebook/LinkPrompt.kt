package com.pkkulhari.notebook

import android.app.AlertDialog
import android.content.DialogInterface
import android.text.InputType
import android.view.KeyEvent
import android.view.WindowManager
import android.view.inputmethod.EditorInfo
import android.widget.EditText
import android.widget.FrameLayout
import com.pkkulhari.notebook.core.SourceEdit
import com.pkkulhari.notebook.core.linkText

/**
 * Asks for a link's URL from the slash menu, selection toolbar or Ctrl+K.
 * It links the selection or caret it opened for, unless the note changed
 * while it was open: then the offsets are stale and nothing happens.
 */
class LinkPrompt(private val text: NoteEditText) {
    var dialog: AlertDialog? = null
        private set

    fun open(apply: (SourceEdit) -> Unit) {
        val context = text.context
        val source = text.text.toString()
        val start = minOf(text.selectionStart, text.selectionEnd)
        val end = maxOf(text.selectionStart, text.selectionEnd)
        if (start < 0) return
        val field = EditText(context).apply {
            id = R.id.link_url
            hint = context.getString(R.string.link_hint)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
            imeOptions = EditorInfo.IME_ACTION_DONE
            isSingleLine = true
            background = context.getDrawable(R.drawable.field)
            setPadding(context.dp(14), context.dp(12), context.dp(14), context.dp(12))
            textSize = 16f
        }
        val frame = FrameLayout(context).apply {
            setPadding(context.dp(24), context.dp(12), context.dp(24), 0)
            addView(field)
        }
        val dialog = AlertDialog.Builder(context)
            .setTitle(R.string.link)
            .setView(frame)
            .setPositiveButton(R.string.insert_link) { _, _ ->
                if (text.text.toString() == source) {
                    linkText(source, start, end, field.text.toString())?.let(apply)
                }
            }
            .setNegativeButton(android.R.string.cancel, null)
            .create()
        // The keyboard's Done action, or Enter on a hardware keyboard, inserts.
        field.setOnEditorActionListener { _, action, event ->
            val enter = event?.keyCode == KeyEvent.KEYCODE_ENTER || event?.keyCode == KeyEvent.KEYCODE_NUMPAD_ENTER
            when {
                action == EditorInfo.IME_ACTION_DONE || (enter && event?.action == KeyEvent.ACTION_DOWN) -> {
                    dialog.getButton(DialogInterface.BUTTON_POSITIVE).performClick()
                    true
                }
                enter -> true
                else -> false
            }
        }
        dialog.setOnDismissListener {
            if (this.dialog === dialog) this.dialog = null
            text.requestFocus()
        }
        dialog.window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_VISIBLE)
        this.dialog = dialog
        dialog.show()
        field.requestFocus()
    }

    fun close() {
        dialog?.dismiss()
    }
}
