package com.pkkulhari.notebook

import android.os.Handler
import android.os.Looper
import android.text.Editable
import android.text.TextUtils
import android.text.TextWatcher
import android.util.Log
import android.view.KeyEvent
import android.view.inputmethod.BaseInputConnection
import com.pkkulhari.notebook.core.Applied
import com.pkkulhari.notebook.core.CoreException
import com.pkkulhari.notebook.core.ListEnter
import com.pkkulhari.notebook.core.TextEdit
import com.pkkulhari.notebook.core.listEnter
import com.pkkulhari.notebook.core.SourceEdit
import com.pkkulhari.notebook.core.FormatCommand
import com.pkkulhari.notebook.core.deleteVisible
import com.pkkulhari.notebook.core.formatCommands
import com.pkkulhari.notebook.core.formatText
import com.pkkulhari.notebook.core.repairRichEdit
import com.pkkulhari.notebook.core.toggleList
import com.pkkulhari.notebook.core.parseMarkdown
import com.pkkulhari.notebook.core.Style

/** Mirrors typing, undo and imported edits between the view and draft on the main thread. */
class EditorController(private val store: Store, private val text: NoteEditText) : TextWatcher {
    /** The note being edited, or null while the text isn't a draft's. */
    var id: String? = null
        private set

    val styler = MarkdownStyler(text)
    private val menu = FormattingMenu(text, ::editable, ::format)
    private val tasks = TaskCheckboxes(text, ::editable, ::toggleTask)
    private var sourceEdit = false
    private var beforeEdit: String? = null
    private var repair: SourceEdit? = null
    private var pendingCursor: Int? = null

    /** Set while the text changes from the draft, so it isn't copied back. */
    private var fromDraft = false

    /** Where a `\n` was just typed, until `afterTextChanged` continues the list. */
    private var newline = -1
    private val handler = Handler(Looper.getMainLooper())
    private val tick = Runnable { schedule(store.core.tick()) }

    init {
        // The Store restores the draft and cursor. TextView restoration would
        // replace the Editable and discard the Markdown mirror attached to it.
        text.isSaveEnabled = false
        text.addTextChangedListener(this)
        text.onSelection = {
            if (id != null && !sourceEdit && !fromDraft) {
                styler.selectionChanged()
                menu.changed()
            }
        }
        text.onUndo = { redo -> if (redo) redo() else undo() }
        text.canEdit = ::editable
        text.onEditorKey = ::key
        text.onDelete = ::delete
        text.onEnter = { menu.key(KeyEvent.KEYCODE_ENTER) }
        text.onAfterInput = {
            pendingCursor?.let { text.setSelection(it.coerceIn(0, text.length())) }
            pendingCursor = null
        }
    }

    fun open(id: String, body: String, cursor: Int) {
        menu.close()
        beforeEdit = null
        repair = null
        pendingCursor = null
        val at = cursor.coerceIn(0, body.length)
        this.id = null
        // Styled first, so the text is laid out once, with its styling.
        styler.style(body, at)
        text.setText(body)
        styler.attach()
        this.id = id
        text.setSelection(at)
        tasks.open()
    }

    fun close() {
        id = null
        menu.close()
        tasks.close()
        handler.removeCallbacks(tick)
        styler.closed()
    }

    override fun beforeTextChanged(s: CharSequence, start: Int, count: Int, after: Int) {
        if (id == null || fromDraft || sourceEdit) return
        pendingCursor = null
        beforeEdit = if (count > 0 || styler.needsRepair(start, start + count)) s.toString() else null
    }

    override fun onTextChanged(s: CharSequence, start: Int, before: Int, count: Int) {
        val id = id ?: return
        if (fromDraft) return
        try {
            // Keyboards rewrite the word they're composing; the core records
            // only the part that changed.
            store.core.replace(id, start, before, TextUtils.substring(s, start, start + count))
        } catch (error: CoreException) {
            Log.w(TAG, "The editor and the draft disagree; reloading", error)
            this.id = null
            store.reload(id)
            return
        }
        styler.changed(start)
        schedule(store.core.tick())
        if (!sourceEdit) {
            repair = beforeEdit?.let { repairRichEdit(it, s.toString()) }
            beforeEdit = null
            if (before == 0 && count == 1 && s[start] == '\n') newline = start
            menu.changed(TextUtils.substring(s, start, start + count).contains('/'))
        }
    }

    override fun afterTextChanged(s: Editable) {
        if (fromDraft || sourceEdit) return
        var at = newline
        newline = -1
        val repaired = repair
        repair = null
        if (repaired != null) {
            replaceSource(repaired)
            pendingCursor = repaired.selection.start
            if (at >= 0) at = repaired.selection.start - 1
        }
        if (at >= 0) {
            sourceEdit = true
            try { continueList(s, at) } finally { sourceEdit = false }
        }
    }

    /**
     * Enter at the end of a list item starts the next one; on an empty item
     * it ends the list instead. `newline` is where the typed `\n` is. This
     * runs within the keyboard's edit, and the change goes through the
     * watcher like typing.
     */
    private fun continueList(s: Editable, newline: Int) {
        val id = id ?: return
        val lineStart = lineStart(s, newline)
        val enter = listEnter(TextUtils.substring(s, lineStart, newline)) ?: return
        if (store.core.lineInCodeBlock(id, newline)) return
        when (enter) {
            is ListEnter.Continue -> if (newline - lineStart >= enter.marker) {
                val next = enter.next.removePrefix("\n")
                s.insert(newline + 1, next)
                text.setSelection(newline + 1 + next.length)
            }
            is ListEnter.End -> s.delete(lineStart, newline + 1)
        }
    }

    private fun schedule(wait: Long) {
        handler.removeCallbacks(tick)
        if (wait >= 0) handler.postDelayed(tick, wait)
    }

    /** Writes the draft's changes into the text. Returns where an undo leaves the cursor. */
    fun apply(applied: Applied): Int? {
        // Autosave echoes import no text edits. They must not dismiss an open
        // slash menu or disturb the keyboard's pending cursor correction.
        val first = applied.edits.firstOrNull() ?: return null
        menu.dismiss()
        pendingCursor = null
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

    private fun editable() = id != null && store.activeNote?.let { it.id == id && !it.deleted } == true

    /** Commands, checkbox clicks and rich repairs all still pass through the draft watcher. */
    private fun replaceSource(edit: SourceEdit, separateUndo: Boolean = false) {
        val id = id ?: return
        if (!editable()) return
        val old = text.text.subSequence(edit.range.start, edit.range.end).toString()
        if (old == edit.text) return
        // Only replace the changed middle, retaining moving spans and IME anchors.
        var prefix = old.commonPrefixWith(edit.text).length
        var suffix = old.substring(prefix).commonSuffixWith(edit.text.substring(prefix)).length
        if (prefix > 0 && prefix < old.length && Character.isLowSurrogate(old[prefix])) prefix--
        if (suffix > 0 && Character.isLowSurrogate(old[old.length - suffix])) suffix--
        if (separateUndo) {
            BaseInputConnection.removeComposingSpans(text.text)
            store.core.undoBoundary(id)
        }
        sourceEdit = true
        try {
            text.text.replace(edit.range.start + prefix, edit.range.end - suffix,
                edit.text.substring(prefix, edit.text.length - suffix))
            text.setSelection(edit.selection.start.coerceIn(0, text.length()), edit.selection.end.coerceIn(0, text.length()))
        } finally {
            sourceEdit = false
            if (separateUndo) store.core.undoBoundary(id)
        }
    }

    private fun format(command: FormatCommand, slash: Boolean) {
        if (!editable()) return
        val edit = formatText(text.text.toString(), text.selectionStart, text.selectionEnd, command.id, slash) ?: return
        menu.dismiss()
        replaceSource(edit, separateUndo = true)
    }

    private fun delete(backwards: Boolean): Boolean {
        if (!editable()) return true
        val edit = deleteVisible(text.text.toString(), text.selectionStart, text.selectionEnd, backwards) ?: return false
        replaceSource(edit)
        return true
    }

    private fun toggleTask(at: Int) {
        if (!editable() || at < 0 || at + 3 > text.length()) return
        val marker = text.text.subSequence(at, at + 3).toString()
        if (marker !in listOf("[ ]", "[x]", "[X]")) return
        // A pending parse can still display a task that was just turned into code.
        val isTask = parseMarkdown(text.text.toString()).use { doc ->
            doc.spans().any { it.start == at && (it.style == Style.TASK || it.style == Style.CHECKED) }
        }
        if (!isTask) return
        val id = id ?: return
        menu.dismiss()
        val start = text.selectionStart
        val end = text.selectionEnd
        BaseInputConnection.removeComposingSpans(text.text)
        store.core.undoBoundary(id)
        sourceEdit = true
        try {
            text.text.replace(at + 1, at + 2, if (marker == "[ ]") "x" else " ")
            if (start >= 0 && end >= 0) text.setSelection(start, end)
        } finally {
            sourceEdit = false
            store.core.undoBoundary(id)
        }
    }

    private fun key(event: KeyEvent): Boolean {
        if (!editable()) return false
        if (!event.isCtrlPressed && !event.isAltPressed && menu.key(event.keyCode)) return true
        if (event.isCtrlPressed && !event.isAltPressed) {
            val query = when (event.keyCode) {
                KeyEvent.KEYCODE_B -> "bold"
                KeyEvent.KEYCODE_I -> "italic"
                KeyEvent.KEYCODE_X -> if (event.isShiftPressed) "strike" else null
                else -> null
            }
            if (query != null) {
                formatCommands(query, true).firstOrNull()?.let { format(it, false) }
                return true
            }
            if (event.isShiftPressed && event.keyCode in listOf(KeyEvent.KEYCODE_7, KeyEvent.KEYCODE_8)) {
                toggleList(text.text.toString(), text.selectionStart, text.selectionEnd, event.keyCode == KeyEvent.KEYCODE_8)
                    ?.let { replaceSource(it, separateUndo = true) }
                return true
            }
        }
        if (!event.isCtrlPressed && !event.isAltPressed) {
            if (event.keyCode == KeyEvent.KEYCODE_DEL) return delete(true)
            if (event.keyCode == KeyEvent.KEYCODE_FORWARD_DEL) return delete(false)
        }
        return false
    }

    companion object {
        private const val TAG = "Notebook"
    }
}
