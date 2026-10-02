package com.pkkulhari.notebook

import android.content.Context
import android.os.Handler
import android.os.Looper
import com.pkkulhari.notebook.core.Applied
import com.pkkulhari.notebook.core.Core
import com.pkkulhari.notebook.core.CoreConfig
import com.pkkulhari.notebook.core.CoreEvent
import com.pkkulhari.notebook.core.CoreListener
import com.pkkulhari.notebook.core.Filter
import com.pkkulhari.notebook.core.Mutation
import com.pkkulhari.notebook.core.NoteInfo
import com.pkkulhari.notebook.core.NoteSummary
import com.pkkulhari.notebook.core.Notebook
import com.pkkulhari.notebook.core.Operation
import com.pkkulhari.notebook.core.SyncControl
import com.pkkulhari.notebook.core.SyncStatus
import java.io.File
import java.util.UUID

/** Main-thread UI state retained across activity recreation. */
class Store(
    private val context: Context,
    database: File,
    syncConfig: File,
    deviceName: String,
) : CoreListener {
    /** The activity showing the store. Every call happens on the main thread. */
    interface Listener {
        /** The note list, its title, the notebooks or the counts changed. */
        fun listChanged()

        /** The search query was applied or reset; cancel any pending search. */
        fun queryChanged()

        fun opened(note: NoteInfo, text: String, cursor: Int, focus: Boolean)

        fun closed()

        /** The open note moved, or was trashed or restored. */
        fun activeChanged()

        /** Changes from another device or from storage, for the open note. */
        fun applied(applied: Applied)

        fun problemChanged()

        fun syncChanged()
    }

    data class Problem(val message: String, val retryable: Boolean)

    private val main = Handler(Looper.getMainLooper())
    val core: Core = Core.start(
        CoreConfig(database.path, syncConfig.path, deviceName),
        this,
    )

    var listener: Listener? = null
    var ready = false
        private set
    var defaultNotebookId = ""
        private set
    var notebooks: List<Notebook> = emptyList()
        private set
    var notes: List<NoteSummary> = emptyList()
        private set
    var counts: Map<Filter, Long> = emptyMap()
        private set
    var filter: Filter = Filter.All
        private set
    var query = ""
        private set
    var problem: Problem? = null
        private set
    var syncStatus: SyncStatus? = null
        private set

    /** The note in the editor, or null while the list shows. */
    var active: String? = null
        private set

    /** Where the cursor was when the app last paused, in UTF-16 units. */
    var cursor = 0

    private val network = NetworkWatch(context) { core.sync(SyncControl.NetworkChanged) }

    /** Whether an activity is showing. Sync runs then, and briefly after. */
    private var visible = false

    /** Stops sync networking once the app has stayed in the background. */
    private val suspend = Runnable {
        core.sync(SyncControl.Suspend(true))
        network.stop()
        network.multicast(false)
    }

    private var listGeneration = 0L
    private var loadGeneration = 0L

    /** Set when the list may be out of date while it isn't showing. */
    private var listStale = false

    init {
        core.initialize()
    }

    val activeNote: NoteInfo? get() = active?.let(core::draftNote)

    fun title(): String = when {
        query.isNotBlank() -> context.getString(R.string.search_results)
        else -> filterName(filter)
    }

    fun filterName(filter: Filter): String = when (filter) {
        is Filter.All -> context.getString(R.string.all_notes)
        is Filter.Trash -> context.getString(R.string.trash)
        is Filter.Notebook -> notebooks.find { it.id == filter.id }?.name
            ?: context.getString(R.string.default_notebook)
    }

    fun count(filter: Filter): Long = counts[filter] ?: 0

    // Core callbacks, on the core's threads. They never throw.

    override fun onEvent(event: CoreEvent) {
        main.post { handle(event) }
    }

    override fun onSyncStatus(status: SyncStatus) {
        main.post {
            syncStatus = status
            if (visible) network.multicast(status.enabled)
            listener?.syncChanged()
        }
    }

    private fun handle(event: CoreEvent) {
        when (event) {
            is CoreEvent.Ready -> {
                ready = true
                defaultNotebookId = event.defaultNotebookId
                notebooks = event.notebooks
                filter = Filter.Notebook(event.note.notebookId)
                val text = core.openDraft(event.note, event.snapshot)
                // Back where the last session ended, or straight to writing
                // on a first start.
                if (event.selectedNote == event.note.id || text.isEmpty()) {
                    show(event.note, text, event.cursor, focus = text.isEmpty())
                }
                requestList()
            }
            is CoreEvent.Listed -> {
                if (event.generation != listGeneration) return
                notes = event.notes
                counts = event.counts.associate { it.filter to it.count }
                listener?.listChanged()
            }
            is CoreEvent.Loaded -> {
                if (event.generation != loadGeneration) return
                val note = event.note ?: return refreshList()
                show(note, core.openDraft(note, event.snapshot), 0, focus = false)
            }
            is CoreEvent.Created -> refreshList()
            is CoreEvent.Saved -> {
                if (problem?.retryable == true && !core.hasFailures()) setProblem(null)
                core.trimDrafts(active)
                refreshList()
            }
            is CoreEvent.Mutated -> {
                notebooks = event.notebooks
                leaveMissingNotebook()
                listener?.activeChanged()
                listener?.listChanged()
                refreshList()
            }
            is CoreEvent.NoteDelta -> {
                // Every change for an open draft is imported, in order, even
                // the echoes of its own saves; later changes depend on them.
                val applied = core.importDelta(event.id, event.delta)
                if (event.id == active) listener?.applied(applied)
            }
            is CoreEvent.Remote -> {
                event.notebooks?.let {
                    notebooks = it
                    leaveMissingNotebook()
                }
                listener?.activeChanged()
                if (active in event.trashed && filter !is Filter.Trash) closeNote()
                listener?.listChanged()
                refreshList()
            }
            is CoreEvent.Flushed -> {}
            is CoreEvent.Error -> setProblem(
                Problem(
                    if (event.operation == Operation.CHANGE) event.message
                    else context.getString(R.string.problem, event.message),
                    event.retryable,
                ),
            )
        }
    }

    private fun leaveMissingNotebook() {
        val current = filter
        if (current is Filter.Notebook && notebooks.none { it.id == current.id }) {
            filter = Filter.Notebook(defaultNotebookId)
        }
    }

    private fun setProblem(problem: Problem?) {
        this.problem = problem
        listener?.problemChanged()
    }

    private fun requestList() {
        if (!ready) return
        listStale = false
        listGeneration += 1
        val searching = query.isNotBlank() && filter !is Filter.Trash
        core.list(if (searching) Filter.All else filter, query, listGeneration)
    }

    /** Refreshes the list now if it's showing, or when it next shows. */
    private fun refreshList() {
        if (active == null) requestList() else listStale = true
    }

    fun showFilter(filter: Filter) {
        cancelPendingLoad()
        this.filter = filter
        listener?.listChanged()
        requestList()
    }

    /** Searches every notebook, or only the trash while it's showing. */
    fun search(query: String) {
        if (query == this.query) return
        this.query = query
        listener?.queryChanged()
        listener?.listChanged()
        requestList()
    }

    /** Navigation supersedes a load even when it doesn't request another one. */
    fun cancelPendingLoad() {
        loadGeneration += 1
    }

    fun openNote(id: String) {
        cancelPendingLoad()
        core.flush()
        val note = core.draftNote(id)
        val text = core.draftText(id)
        if (note != null && text != null) {
            show(note, text, 0, focus = false)
        } else {
            core.load(id, loadGeneration)
        }
    }

    /** Creates a note in Default, regardless of the current filter. */
    fun newNote() {
        cancelPendingLoad()
        core.flush()
        val note = core.createNote()
        filter = Filter.Notebook(defaultNotebookId)
        query = ""
        // Notify even when already empty: the widget may have a pending search.
        listener?.queryChanged()
        show(note, "", 0, focus = true)
    }

    private fun show(note: NoteInfo, text: String, cursor: Int, focus: Boolean) {
        active = note.id
        this.cursor = cursor
        core.showDraft(note.id)
        core.trimDrafts(note.id)
        listener?.opened(note, text, cursor, focus)
    }

    fun closeNote() {
        cancelPendingLoad()
        if (active == null) return
        core.flush()
        active = null
        listener?.closed()
        core.trimDrafts(null)
        if (listStale) requestList()
    }

    /** Opens the note again from storage, when the editor and its draft disagree. */
    fun reload(id: String) {
        cancelPendingLoad()
        core.flush()
        core.load(id, loadGeneration)
    }

    /** Trashing returns to the list; restoring keeps the note open. */
    fun trashOrRestore() {
        val note = activeNote ?: return
        core.flush()
        if (note.deleted) {
            core.mutate(Mutation.Restore(note.id))
        } else {
            core.mutate(Mutation.Trash(note.id))
            closeNote()
        }
    }

    fun moveActive(notebookId: String) {
        val id = active ?: return
        core.flush()
        core.mutate(Mutation.Move(id, notebookId))
    }

    fun createNotebook(name: String) {
        core.mutate(Mutation.CreateNotebook(UUID.randomUUID().toString(), name))
    }

    fun renameNotebook(id: String, name: String) {
        core.mutate(Mutation.RenameNotebook(id, name))
    }

    /** Its notes move to Default. */
    fun deleteNotebook(id: String) {
        core.flush()
        core.mutate(Mutation.DeleteNotebook(id))
    }

    fun retry() {
        setProblem(null)
        core.retry()
    }

    fun dismissProblem() = setProblem(null)

    /**
     * Suspends background networking without disabling sync, after a delay
     * so a rotation or a quick trip to another app keeps the connections.
     * Resuming catches up.
     */
    fun foreground(visible: Boolean) {
        if (visible == this.visible) return
        this.visible = visible
        if (visible) {
            if (main.hasCallbacks(suspend)) {
                main.removeCallbacks(suspend)
                return
            }
            network.start()
            network.multicast(syncStatus?.enabled == true)
            core.sync(SyncControl.Suspend(false))
        } else {
            core.flush()
            main.postDelayed(suspend, SUSPEND_DELAY_MS)
        }
    }

    fun pause() {
        core.flush()
        core.savePreferences(active, cursor)
    }

    companion object {
        /**
         * How long sync keeps running in the background: long enough for a
         * rotation or a glance at another app, short because Android may
         * freeze a backgrounded app at any time.
         */
        const val SUSPEND_DELAY_MS = 5_000L
    }
}
