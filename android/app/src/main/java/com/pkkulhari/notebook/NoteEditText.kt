package com.pkkulhari.notebook

import android.content.Context
import android.util.AttributeSet
import android.view.View
import android.widget.EditText

/**
 * Undo uses the core's history of local edits; paste drops foreign formatting.
 *
 * Disable autofill and content capture to keep notes private and avoid sending
 * the entire note to those services on every keystroke.
 */
class NoteEditText(context: Context, attrs: AttributeSet?) : EditText(context, attrs) {
    init {
        importantForAutofill = View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS
        importantForContentCapture = View.IMPORTANT_FOR_CONTENT_CAPTURE_NO_EXCLUDE_DESCENDANTS
    }

    override fun getAutofillType() = View.AUTOFILL_TYPE_NONE

    var onSelection: (() -> Unit)? = null

    /** Runs for undo (`false`) or redo (`true`) from the text menu or a keyboard. */
    var onUndo: ((redo: Boolean) -> Unit)? = null

    override fun onSelectionChanged(selStart: Int, selEnd: Int) {
        super.onSelectionChanged(selStart, selEnd)
        onSelection?.invoke()
    }

    override fun onTextContextMenuItem(id: Int): Boolean = when (id) {
        android.R.id.undo -> true.also { onUndo?.invoke(false) }
        android.R.id.redo -> true.also { onUndo?.invoke(true) }
        android.R.id.paste -> super.onTextContextMenuItem(android.R.id.pasteAsPlainText)
        else -> super.onTextContextMenuItem(id)
    }
}
