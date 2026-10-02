package com.pkkulhari.notebook


import androidx.test.ext.junit.runners.AndroidJUnit4
import com.pkkulhari.notebook.core.Filter
import java.io.File
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/** The app shell's behaviour, through the store and a real EditText. */
@RunWith(AndroidJUnit4::class)
class StoreTest {
    private lateinit var dir: File

    @Before
    fun setUp() {
        AndroidContext.install(context)
        dir = temporaryDirectory()
    }

    // The cores stay open: like the app's, they live as long as the process,
    // and an editor's scheduled save may still run after a test ends.
    @After
    fun tearDown() {
        dir.deleteRecursively()
    }

    private fun start(): Pair<Store, Recorder> {
        val view = Recorder()
        val store = onMain {
            Store(context, File(dir, "notebook.db"), File(dir, "sync.json"), "Test phone").also { it.listener = view }
        }
        waitUntil("the store to be ready") { store.ready }
        return store to view
    }

    /** Writes a note the way a person does: new note, type, back. */
    private fun write(store: Store, view: Recorder, body: String): String {
        val text = onMain { NoteEditText(context, null) }
        onMain { store.newNote() }
        val id = onMain { view.opened!!.id }
        onMain {
            EditorController(store, text).open(id, "", 0)
            text.text.append(body)
            store.closeNote()
        }
        val label = body.lineSequence().first()
        waitUntil("“$label” in the list") { store.notes.any { it.label == label } }
        return id
    }

    @Test
    fun textSurvivesTheProcessGoingAway() {
        val (first, view) = start()
        // A fresh database opens its first note for writing.
        waitUntil("the editor") { view.opened != null }
        val id = onMain { view.opened!!.id }
        val text = onMain { NoteEditText(context, null) }
        onMain {
            EditorController(first, text).open(id, view.text, 0)
            text.text.append("Written on a phone 🌿")
            first.cursor = text.length()
            // The activity pauses, and Android later kills the process.
            first.pause()
            first.closeNote()
        }
        waitUntil("the save") { first.notes.any { it.label == "Written on a phone 🌿" } }

        val (second, again) = start()
        waitUntil("the editor to reopen") { again.opened != null }
        assertEquals(id, onMain { again.opened!!.id })
        assertEquals("Written on a phone 🌿", onMain { again.text })
        assertEquals("Written on a phone 🌿".length, onMain { second.cursor })
    }

    @Test
    fun searchFindsWordPrefixesAndTrashSearchesOnlyTrash() {
        val (store, view) = start()
        val rhubarb = write(store, view, "Rhubarb crumble\nWith custard")
        write(store, view, "Rhyme scheme")
        write(store, view, "Shopping")
        onMain { store.showFilter(Filter.All) }
        onMain { store.search("rh") }
        waitUntil("two matches") { store.notes.size == 2 }
        assertEquals("Search results", onMain { store.title() })
        onMain { store.search("rhu cru") }
        waitUntil("one match") { store.notes.map { it.id } == listOf(rhubarb) }

        onMain { store.search("") }
        onMain { store.openNote(rhubarb) }
        waitUntil("the note to open") { view.opened?.id == rhubarb }
        onMain { store.trashOrRestore() }
        onMain { store.showFilter(Filter.Trash) }
        onMain { store.search("rh") }
        waitUntil("only the trashed note") { store.notes.map { it.id } == listOf(rhubarb) }
    }

    @Test
    fun trashingTheOpenNoteReturnsToTheListAndItCanBeRestored() {
        val (store, view) = start()
        val id = write(store, view, "Old idea")
        onMain { store.openNote(id) }
        waitUntil("the note to open") { view.opened?.id == id }
        val closedBefore = onMain { view.closed }
        onMain { store.trashOrRestore() }
        assertNull(onMain { store.active })
        assertEquals(closedBefore + 1, onMain { view.closed })
        waitUntil("the note to leave the list") { store.notes.none { it.id == id } }

        onMain { store.showFilter(Filter.Trash) }
        waitUntil("the trash to list it") { store.notes.any { it.id == id } }
        onMain { store.openNote(id) }
        waitUntil("the trashed note to open") { view.opened?.id == id }
        assertTrue(onMain { store.activeNote!!.deleted })
        onMain { store.trashOrRestore() }
        waitUntil("the restore") { store.activeNote?.deleted == false }
        onMain { store.closeNote() }
        waitUntil("the trash to empty") { store.notes.none { it.id == id } }
    }

    @Test
    fun notebooksAreCreatedRenamedAndDeletedWithTheirNotesMovingToDefault() {
        val (store, view) = start()
        val id = write(store, view, "Quarterly plan")
        onMain { store.createNotebook("Work") }
        waitUntil("the notebook") { store.notebooks.any { it.name == "Work" } }
        val work = onMain { store.notebooks.first { it.name == "Work" }.id }
        onMain { store.openNote(id) }
        waitUntil("the note to open") { view.opened?.id == id }
        onMain { store.moveActive(work) }
        waitUntil("the move") { store.activeNote?.notebookId == work }

        onMain { store.renameNotebook(work, "Projects") }
        waitUntil("the rename") { store.notebooks.any { it.id == work && it.name == "Projects" } }
        onMain { store.createNotebook("Projects") }
        waitUntil("the duplicate name to be refused") { store.problem != null }
        onMain { store.dismissProblem() }

        onMain { store.deleteNotebook(work) }
        waitUntil("the deletion") { store.notebooks.none { it.id == work } }
        assertEquals(onMain { store.defaultNotebookId }, onMain { store.activeNote!!.notebookId })
    }
}
