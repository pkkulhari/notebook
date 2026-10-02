package com.pkkulhari.notebook

import android.os.Handler
import android.os.Looper
import android.text.Editable
import android.text.TextWatcher
import android.util.Log
import android.widget.EditText
import com.pkkulhari.notebook.core.Applied
import com.pkkulhari.notebook.core.CoreException
import com.pkkulhari.notebook.core.TextEdit

/**
 * Keeps an [EditText] and the open note's draft identical. Typing is copied
 * into the draft as it happens; the draft's undos and imported changes are
 * written into the text. Everything runs on the main thread.
 */
class EditorController(private val store: Store, private val text: EditText) : TextWatcher {
    /** The note being edited, or null while the text isn't a draft's. */
    var id: String? = null
        private set

    /** Set while the text changes from the draft, so it isn't copied back. */
    private var fromDraft = false
    private val handler = Handler(Looper.getMainLooper())
    private val tick = Runnable { schedule(store.core.tick()) }

    init {
        text.addTextChangedListener(this)
    }

    fun open(id: String, body: String, cursor: Int) {
        this.id = null
        text.setText(body)
        this.id = id
        text.setSelection(cursor.coerceIn(0, text.length()))
    }

    fun close() {
        id = null
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
        schedule(store.core.tick())
    }

    override fun afterTextChanged(s: Editable) {}

    /** Asks the core to save when it says the next save is due. */
    private fun schedule(wait: Long) {
        handler.removeCallbacks(tick)
        if (wait >= 0) handler.postDelayed(tick, wait)
    }

    /** Writes the draft's changes into the text. Returns where an undo leaves the cursor. */
    fun apply(applied: Applied): Int? {
        if (applied.edits.isEmpty()) return null
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
        return applied.cursor
    }

    fun undo() = undoOrRedo(undo = true)

    fun redo() = undoOrRedo(undo = false)

    private fun undoOrRedo(undo: Boolean) {
        val id = id ?: return
        val applied = (if (undo) store.core.undo(id) else store.core.redo(id)) ?: return
        apply(applied)?.let { text.setSelection(it.coerceIn(0, text.length())) }
        // An undo is an edit, and needs saving like one.
        schedule(store.core.tick())
    }

    companion object {
        private const val TAG = "Notebook"
    }
}
