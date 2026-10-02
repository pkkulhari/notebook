package com.pkkulhari.notebook

import android.content.Context
import android.util.AttributeSet
import android.view.View
import android.widget.EditText

/**
 * The editor's text widget. Undo and redo go to the core, whose history
 * undoes only this device's edits, and pasting drops foreign formatting.
 *
 * Notes never go to autofill or content capture services. Otherwise every
 * keystroke would send the whole note out of the app, which is both a leak
 * and, for a long note, slow enough to see (and big enough to crash).
 */
class NoteEditText(context: Context, attrs: AttributeSet?) : EditText(context, attrs) {
    init {
        importantForAutofill = View.IMPORTANT_FOR_AUTOFILL_NO_EXCLUDE_DESCENDANTS
        importantForContentCapture = View.IMPORTANT_FOR_CONTENT_CAPTURE_NO_EXCLUDE_DESCENDANTS
    }

    override fun getAutofillType() = View.AUTOFILL_TYPE_NONE

    /** Runs on every selection or cursor change. */
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
