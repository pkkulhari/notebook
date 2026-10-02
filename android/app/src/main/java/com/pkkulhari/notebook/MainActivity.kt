package com.pkkulhari.notebook

import android.app.Activity
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.text.Editable
import android.text.TextWatcher
import android.view.KeyEvent
import android.view.View
import android.view.WindowInsets
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.EditText
import android.widget.ImageButton
import android.widget.ListView
import android.widget.TextView
import android.window.OnBackInvokedCallback
import android.window.OnBackInvokedDispatcher
import com.pkkulhari.notebook.core.Applied
import com.pkkulhari.notebook.core.NoteInfo

/** The only activity: the note list, or the editor. */
class MainActivity : Activity(), Store.Listener {
    private val store get() = (application as NotebookApp).store

    private lateinit var listScreen: View
    private lateinit var listTitle: TextView
    private lateinit var search: EditText
    private lateinit var notes: ListView
    private lateinit var listEmpty: TextView
    private lateinit var editorScreen: View
    private lateinit var location: TextView
    private lateinit var undo: ImageButton
    private lateinit var redo: ImageButton
    private lateinit var trash: ImageButton
    private lateinit var text: EditText
    private lateinit var problem: View
    private lateinit var problemText: TextView
    private lateinit var problemAction: Button

    private lateinit var editor: EditorController
    private lateinit var adapter: NoteListAdapter
    private lateinit var picker: NotebookPicker

    private val handler = Handler(Looper.getMainLooper())
    private val runSearch = Runnable { store.search(search.text.toString()) }
    private val back = OnBackInvokedCallback { store.closeNote() }
    private var backRegistered = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        listScreen = findViewById(R.id.list_screen)
        listTitle = findViewById(R.id.list_title)
        search = findViewById(R.id.search)
        notes = findViewById(R.id.notes)
        listEmpty = findViewById(R.id.list_empty)
        editorScreen = findViewById(R.id.editor_screen)
        location = findViewById(R.id.location)
        undo = findViewById(R.id.undo)
        redo = findViewById(R.id.redo)
        trash = findViewById(R.id.trash)
        text = findViewById(R.id.text)
        problem = findViewById(R.id.problem)
        problemText = findViewById(R.id.problem_text)
        problemAction = findViewById(R.id.problem_action)

        editor = EditorController(store, text)
        adapter = NoteListAdapter(layoutInflater)
        picker = NotebookPicker(this, store)
        notes.adapter = adapter
        notes.setOnItemClickListener { _, _, position, _ -> store.openNote(adapter.getItem(position).id) }
        listTitle.setOnClickListener { picker.show() }
        findViewById<View>(R.id.new_note).setOnClickListener { store.newNote() }
        search.setText(store.query)
        search.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence, start: Int, before: Int, count: Int) {}
            override fun afterTextChanged(s: Editable) {
                handler.removeCallbacks(runSearch)
                handler.postDelayed(runSearch, SEARCH_DELAY_MS)
            }
        })
        findViewById<View>(R.id.back).setOnClickListener { store.closeNote() }
        location.setOnClickListener { picker.move(it) }
        undo.setOnClickListener { editor.undo() }
        redo.setOnClickListener { editor.redo() }
        trash.setOnClickListener { store.trashOrRestore() }
        problemAction.setOnClickListener {
            if (store.problem?.retryable == true) store.retry() else store.dismissProblem()
        }
        applyInsets()

        store.listener = this
        val active = store.activeNote
        val body = active?.let { store.core.draftText(it.id) }
        if (active != null && body != null) {
            opened(active, body, store.cursor, focus = false)
        } else {
            closed()
        }
        problemChanged()
    }

    override fun onDestroy() {
        handler.removeCallbacks(runSearch)
        if (store.listener === this) store.listener = null
        super.onDestroy()
    }

    override fun onPause() {
        super.onPause()
        if (editor.id != null) store.cursor = text.selectionStart.coerceAtLeast(0)
        store.pause()
    }

    /** System bars and the keyboard never cover the content. */
    private fun applyInsets() {
        val root = findViewById<View>(R.id.root)
        root.setOnApplyWindowInsetsListener { view, insets ->
            val bars = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.displayCutout())
            val keyboard = insets.getInsets(WindowInsets.Type.ime())
            view.setPadding(bars.left, bars.top, bars.right, maxOf(bars.bottom, keyboard.bottom))
            WindowInsets.CONSUMED
        }
    }

    // Store.Listener

    override fun listChanged() {
        listTitle.text = store.title()
        adapter.notes = store.notes
        listEmpty.visibility = if (store.ready && store.notes.isEmpty()) View.VISIBLE else View.GONE
        listEmpty.setText(if (store.query.isBlank()) R.string.no_notes else R.string.no_matching_notes)
    }

    override fun opened(note: NoteInfo, text: String, cursor: Int, focus: Boolean) {
        editor.open(note.id, text, cursor)
        listScreen.visibility = View.GONE
        editorScreen.visibility = View.VISIBLE
        activeChanged()
        if (!backRegistered) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, back)
            backRegistered = true
        }
        if (focus && !note.deleted) {
            this.text.requestFocus()
            getSystemService(InputMethodManager::class.java).showSoftInput(this.text, 0)
        }
    }

    override fun closed() {
        editor.close()
        getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(text.windowToken, 0)
        text.clearFocus()
        editorScreen.visibility = View.GONE
        listScreen.visibility = View.VISIBLE
        if (backRegistered) {
            onBackInvokedDispatcher.unregisterOnBackInvokedCallback(back)
            backRegistered = false
        }
        listChanged()
    }

    override fun activeChanged() {
        val note = store.activeNote ?: return
        location.text = store.notebooks.find { it.id == note.notebookId }?.name ?: getString(R.string.default_notebook)
        location.isEnabled = !note.deleted
        // Trashed notes are read-only until restored.
        val editable = !note.deleted
        text.isFocusable = editable
        text.isFocusableInTouchMode = editable
        text.isCursorVisible = editable
        undo.isEnabled = editable
        redo.isEnabled = editable
        trash.setImageResource(if (note.deleted) R.drawable.ic_restore else R.drawable.ic_trash)
        trash.contentDescription = getString(if (note.deleted) R.string.restore else R.string.move_to_trash)
    }

    override fun applied(applied: Applied) {
        editor.apply(applied)
    }

    override fun problemChanged() {
        val current = store.problem
        problem.visibility = if (current == null) View.GONE else View.VISIBLE
        problemText.text = current?.message
        problemAction.setText(if (current?.retryable == true) R.string.retry else R.string.dismiss)
    }

    override fun syncChanged() {}

    /** Ctrl+Z and Ctrl+Shift+Z undo the draft's edits, not the text widget's own history. */
    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        if (event.action == KeyEvent.ACTION_DOWN && event.isCtrlPressed && editorScreen.visibility == View.VISIBLE) {
            when {
                event.keyCode == KeyEvent.KEYCODE_Z && event.isShiftPressed -> return true.also { editor.redo() }
                event.keyCode == KeyEvent.KEYCODE_Z -> return true.also { editor.undo() }
                event.keyCode == KeyEvent.KEYCODE_Y -> return true.also { editor.redo() }
            }
        }
        if (event.action == KeyEvent.ACTION_DOWN && event.isCtrlPressed && event.keyCode == KeyEvent.KEYCODE_N) {
            store.newNote()
            return true
        }
        return super.dispatchKeyEvent(event)
    }

    companion object {
        private const val SEARCH_DELAY_MS = 150L
    }
}
