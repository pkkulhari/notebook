package com.pkkulhari.notebook

import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.ColorStateList
import android.net.Uri
import android.os.Build
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
import android.widget.PopupMenu
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
    private lateinit var openLink: ImageButton
    private lateinit var text: NoteEditText
    private lateinit var problem: View
    private lateinit var problemText: TextView
    private lateinit var problemAction: Button
    private lateinit var syncScreen: View
    private lateinit var syncButton: ImageButton
    private lateinit var sync: SyncPanel

    private lateinit var editor: EditorController
    private lateinit var adapter: NoteListAdapter
    private lateinit var picker: NotebookPicker

    private val handler = Handler(Looper.getMainLooper())
    private val runSearch = Runnable { store.search(search.text.toString()) }
    private val back = OnBackInvokedCallback { goBack() }
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
        openLink = findViewById(R.id.open_link)
        text = findViewById(R.id.text)
        problem = findViewById(R.id.problem)
        problemText = findViewById(R.id.problem_text)
        problemAction = findViewById(R.id.problem_action)
        syncScreen = findViewById(R.id.sync_screen)
        syncButton = findViewById(R.id.sync_button)
        sync = SyncPanel(syncScreen, store, ::allowLocalNetwork)
        syncButton.setOnClickListener { showSync(true) }
        findViewById<View>(R.id.sync_back).setOnClickListener { showSync(false) }

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
        findViewById<View>(R.id.more).setOnClickListener { showMore(it) }
        editor.styler.onLink = { url ->
            openLink.visibility = if (url == null) View.GONE else View.VISIBLE
            openLink.tag = url
        }
        openLink.setOnClickListener { (it.tag as? String)?.let(::open) }
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
        syncChanged()
    }

    override fun onStart() {
        super.onStart()
        store.foreground(true)
    }

    override fun onStop() {
        store.foreground(false)
        super.onStop()
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
        syncScreen.visibility = View.GONE
        updateBack()
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
        updateBack()
        listChanged()
    }

    /** Back closes the sync screen or the editor; from the list it leaves the app. */
    private fun goBack() {
        if (syncScreen.visibility == View.VISIBLE) showSync(false) else store.closeNote()
    }

    private fun updateBack() {
        val wanted = syncScreen.visibility == View.VISIBLE || editorScreen.visibility == View.VISIBLE
        if (wanted && !backRegistered) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(OnBackInvokedDispatcher.PRIORITY_DEFAULT, back)
        } else if (!wanted && backRegistered) {
            onBackInvokedDispatcher.unregisterOnBackInvokedCallback(back)
        }
        backRegistered = wanted
    }

    private fun showSync(show: Boolean) {
        syncScreen.visibility = if (show) View.VISIBLE else View.GONE
        listScreen.visibility = if (show || editorScreen.visibility == View.VISIBLE) View.GONE else View.VISIBLE
        if (show) sync.update() else getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(syncScreen.windowToken, 0)
        updateBack()
    }

    /**
     * Android 17 asks before an app reaches devices on the local network.
     * Asked when sync is turned on or pairing starts, never at launch.
     */
    private fun allowLocalNetwork() {
        if (Build.VERSION.SDK_INT < 37 || checkSelfPermission(LOCAL_NETWORK) == PackageManager.PERMISSION_GRANTED) return
        requestPermissions(arrayOf(LOCAL_NETWORK), LOCAL_NETWORK_REQUEST)
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == LOCAL_NETWORK_REQUEST) {
            sync.localNetworkDenied = grantResults.firstOrNull() != PackageManager.PERMISSION_GRANTED
        }
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

    override fun syncChanged() {
        val status = store.syncStatus ?: return
        if (syncScreen.visibility == View.VISIBLE) sync.update()
        val connected = status.devices.filter { it.connected }.map { it.name }
        syncButton.contentDescription = when {
            status.problem != null -> status.problem
            !status.enabled -> getString(R.string.sync_off)
            connected.isEmpty() -> getString(R.string.sync_idle)
            connected.size == 1 -> getString(R.string.syncing_with, connected[0])
            else -> getString(R.string.syncing_with_many, connected.size)
        }
        syncButton.tooltipText = syncButton.contentDescription
        // Lit while syncing with at least one device, like the desktop's button.
        syncButton.imageTintList = ColorStateList.valueOf(
            getColor(if (connected.isNotEmpty()) android.R.color.system_accent1_500 else android.R.color.system_neutral1_500),
        )
    }

    /** Links are opened deliberately, from the top bar; a tap only places the cursor. */
    private fun open(url: String) {
        try {
            startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
        } catch (_: ActivityNotFoundException) {
            // Nothing on this device opens it.
        }
    }

    private fun showMore(anchor: View) {
        // Words as the desktop counts them: runs of non-whitespace.
        val words = text.text.split(WHITESPACE).count { it.isNotEmpty() }
        PopupMenu(this, anchor).apply {
            menu.add(resources.getQuantityString(R.plurals.words, words, words)).isEnabled = false
        }.show()
    }

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
        private const val LOCAL_NETWORK = "android.permission.ACCESS_LOCAL_NETWORK"
        private const val LOCAL_NETWORK_REQUEST = 1
        private val WHITESPACE = Regex("\\s+")
    }
}
