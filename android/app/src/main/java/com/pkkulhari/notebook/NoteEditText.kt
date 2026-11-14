package com.pkkulhari.notebook

import android.content.Context
import android.content.ClipData
import android.content.ClipboardManager
import android.util.AttributeSet
import android.view.KeyEvent
import android.view.View
import android.view.inputmethod.BaseInputConnection
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputConnectionWrapper
import android.view.inputmethod.TextAttribute
import android.widget.EditText
import com.pkkulhari.notebook.core.visibleText

/**
 * Undo uses the core's history of local edits; paste drops foreign formatting
 * and turns a URL into a link.
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
    var onEditorKey: ((KeyEvent) -> Boolean)? = null
    var onDelete: ((Boolean) -> Boolean)? = null
    var onEnter: (() -> Boolean)? = null
    var onAfterInput: (() -> Unit)? = null
    var canEdit: (() -> Boolean)? = null

    /** Handles pasted text itself, such as linking a URL, or returns false. */
    var onPaste: ((String) -> Boolean)? = null

    override fun onSelectionChanged(selStart: Int, selEnd: Int) {
        super.onSelectionChanged(selStart, selEnd)
        onSelection?.invoke()
    }

    override fun onTextContextMenuItem(id: Int): Boolean = when (id) {
        android.R.id.undo -> true.also { onUndo?.invoke(false) }
        android.R.id.redo -> true.also { onUndo?.invoke(true) }
        android.R.id.copy, android.R.id.cut -> {
            if (id == android.R.id.cut && canEdit?.invoke() != true) true
            else {
                val start = minOf(selectionStart, selectionEnd)
                val end = maxOf(selectionStart, selectionEnd)
                if (start >= 0 && end > start) {
                    context.getSystemService(ClipboardManager::class.java).setPrimaryClip(
                        ClipData.newPlainText("Note", visibleText(text.toString(), start, end)),
                    )
                    if (id == android.R.id.cut) onDelete?.invoke(true) else setSelection(end)
                }
                true
            }
        }
        android.R.id.paste, android.R.id.pasteAsPlainText -> when {
            canEdit?.invoke() != true -> true
            clipboardText()?.let { onPaste?.invoke(it) } == true -> true
            else -> super.onTextContextMenuItem(android.R.id.pasteAsPlainText)
        }
        else -> super.onTextContextMenuItem(id)
    }

    private fun clipboardText(): String? =
        context.getSystemService(ClipboardManager::class.java).primaryClip
            ?.takeIf { it.itemCount > 0 }
            ?.getItemAt(0)?.coerceToText(context)?.toString()

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (onEditorKey?.invoke(event) == true) return true
        return super.onKeyDown(keyCode, event).also { onAfterInput?.invoke() }
    }

    override fun onCreateInputConnection(outAttrs: EditorInfo): InputConnection? {
        val target = super.onCreateInputConnection(outAttrs) ?: return null
        return object : InputConnectionWrapper(target, false) {
            private fun input(block: () -> Boolean): Boolean {
                if (canEdit?.invoke() != true) return false
                return block().also { onAfterInput?.invoke() }
            }

            private fun enter(value: CharSequence?) = value?.toString() == "\n" && onEnter?.invoke() == true

            override fun commitText(value: CharSequence?, newCursorPosition: Int) =
                input { enter(value) || super.commitText(value, newCursorPosition) }

            override fun commitText(value: CharSequence, newCursorPosition: Int, attr: TextAttribute?) =
                input { enter(value) || super.commitText(value, newCursorPosition, attr) }

            override fun setComposingText(value: CharSequence?, newCursorPosition: Int) =
                input { super.setComposingText(value, newCursorPosition) }

            override fun setComposingText(value: CharSequence, newCursorPosition: Int, attr: TextAttribute?) =
                input { super.setComposingText(value, newCursorPosition, attr) }

            private fun delete(before: Int, after: Int): Boolean {
                if (BaseInputConnection.getComposingSpanStart(text) >= 0) return false
                return when {
                    before == 1 && after == 0 -> onDelete?.invoke(true) == true
                    before == 0 && after == 1 -> onDelete?.invoke(false) == true
                    else -> false
                }
            }

            override fun deleteSurroundingText(beforeLength: Int, afterLength: Int) =
                input { delete(beforeLength, afterLength) || super.deleteSurroundingText(beforeLength, afterLength) }

            override fun deleteSurroundingTextInCodePoints(beforeLength: Int, afterLength: Int) =
                input { delete(beforeLength, afterLength) || super.deleteSurroundingTextInCodePoints(beforeLength, afterLength) }
        }
    }
}
