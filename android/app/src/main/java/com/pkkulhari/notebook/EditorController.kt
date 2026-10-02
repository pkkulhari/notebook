package com.pkkulhari.notebook

import android.os.Handler
import android.os.Looper
import android.text.Editable
import android.text.TextUtils
import android.text.TextWatcher
import android.util.Log
import android.view.inputmethod.BaseInputConnection
import com.pkkulhari.notebook.core.Applied
import com.pkkulhari.notebook.core.CoreException
import com.pkkulhari.notebook.core.ListEnter
import com.pkkulhari.notebook.core.TextEdit
import com.pkkulhari.notebook.core.listEnter

/** Mirrors typing, undo and imported edits between the view and draft on the main thread. */
class EditorController(private val store: Store, private val text: NoteEditText) : TextWatcher {
    /** The note being edited, or null while the text isn't a draft's. */
    var id: String? = null
        private set

    val styler = MarkdownStyler(text)

    /** Set while the text changes from the draft, so it isn't copied back. */
    private var fromDraft = false
    private val handler = Handler(Looper.getMainLooper())
    private val tick = Runnable { schedule(store.core.tick()) }

    init {
        // The Store restores the draft and cursor. TextView restoration would
        // replace the Editable and discard the Markdown mirror attached to it.
        text.isSaveEnabled = false
        text.addTextChangedListener(this)
        text.onSelection = { if (id != null) styler.selectionChanged() }
        text.onUndo = { redo -> if (redo) redo() else undo() }
    }

    fun open(id: String, body: String, cursor: Int) {
        val at = cursor.coerceIn(0, body.length)
        this.id = null
        // Styled first, so the text is laid out once, with its styling.
        styler.style(body, at)
        text.setText(body)
        styler.attach()
        this.id = id
        text.setSelection(at)
    }

    fun close() {
        id = null
        styler.closed()
    }

    override fun beforeTextChanged(s: CharSequence, start: Int, count: Int, after: Int) {}

    override fun onTextChanged(s: CharSequence, start: Int, before: Int, count: Int) {
        val id = id ?: return
        if (fromDraft) return
        try {
            // Keyboards rewrite the word they're composing; the core records
            // only the part that changed.
            store.core.replace(id, start, before, s.subSequence(start, start + count).toString())
        } catch (error: CoreException) {
            Log.w(TAG, "The editor and the draft disagree; reloading", error)
            this.id = null
            store.reload(id)
            return
        }
        styler.changed(start)
        schedule(store.core.tick())
        if (before == 0 && count == 1 && s[start] == '\n') {
            // After the watcher returns, so the edit goes through it like typing.
            handler.post { continueList(id, start) }
        }
    }

    override fun afterTextChanged(s: Editable) {}

    /**
     * Enter at the end of a list item starts the next one; on an empty item
     * it ends the list instead. `newline` is where the typed `\n` is.
     */
    private fun continueList(id: String, newline: Int) {
        val editable = text.text
        if (id != this.id || newline >= editable.length || editable[newline] != '\n') return
        if (styler.inCodeBlock(newline)) return
        val lineStart = if (newline == 0) 0 else TextUtils.lastIndexOf(editable, '\n', newline - 1) + 1
        when (val enter = listEnter(editable.substring(lineStart, newline))) {
            is ListEnter.Continue -> if (newline - lineStart >= enter.marker) {
                val next = enter.next.removePrefix("\n")
                editable.insert(newline + 1, next)
                text.setSelection(newline + 1 + next.length)
            }
            is ListEnter.End -> editable.delete(lineStart, newline + 1)
            null -> {}
        }
    }

    private fun schedule(wait: Long) {
        handler.removeCallbacks(tick)
        if (wait >= 0) handler.postDelayed(tick, wait)
    }

    /** Writes the draft's changes into the text. Returns where an undo leaves the cursor. */
    fun apply(applied: Applied): Int? {
        val first = applied.edits.firstOrNull() ?: return null
        val editable = text.text
        fromDraft = true
        try {
            for (edit in applied.edits) {
                when (edit) {
                    is TextEdit.Insert -> editable.insert(edit.at, edit.text)
                    is TextEdit.Delete -> editable.delete(edit.at, edit.at + edit.len)
                }
            }
        } finally {
            fromDraft = false
        }
        styler.changed(
            when (first) {
                is TextEdit.Insert -> first.at
                is TextEdit.Delete -> first.at
            },
        )
        return applied.cursor
    }

    fun undo() = undoOrRedo(undo = true)

    fun redo() = undoOrRedo(undo = false)

    private fun undoOrRedo(undo: Boolean) {
        val id = id ?: return
        if (store.activeNote?.let { it.id == id && !it.deleted } != true) return
        val applied = (if (undo) store.core.undo(id) else store.core.redo(id)) ?: return
        // The keyboard's composing word is gone or changed; let it start over.
        BaseInputConnection.removeComposingSpans(text.text)
        apply(applied)?.let { cursor ->
            text.setSelection(cursor.coerceIn(0, text.length()))
            text.bringPointIntoView(text.selectionStart)
        }
        schedule(store.core.tick())
    }

    companion object {
        private const val TAG = "Notebook"
    }
}
