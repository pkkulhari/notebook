package com.pkkulhari.notebook

import android.content.ClipData
import android.content.ClipboardManager
import android.os.SystemClock
import android.text.Spanned
import android.text.style.StyleSpan
import android.util.Log
import android.view.KeyEvent
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.pkkulhari.notebook.core.Applied
import com.pkkulhari.notebook.core.Filter
import com.pkkulhari.notebook.core.TextEdit
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class EditorTest {
    private lateinit var scenario: ActivityScenario<MainActivity>
    private lateinit var store: Store
    private lateinit var text: NoteEditText
    private lateinit var input: InputConnection
    private lateinit var id: String

    @Before
    fun setUp() {
        scenario = ActivityScenario.launch(MainActivity::class.java)
        scenario.onActivity { activity ->
            store = (activity.application as NotebookApp).store
            text = activity.findViewById(R.id.text)
        }
        waitUntil("the store to be ready") { store.ready }
        onMain { store.newNote() }
        id = onMain { store.active!! }
        // Detach the real keyboard so it cannot finish the test's compositions.
        input = onMain {
            context.getSystemService(InputMethodManager::class.java).hideSoftInputFromWindow(text.windowToken, 0)
            text.isFocusable = false
            text.onCreateInputConnection(EditorInfo())!!
        }
    }

    @After
    fun tearDown() {
        scenario.close()
    }

    private fun content() = onMain { text.text.toString() }

    private fun assertInStep() = assertEquals(content(), onMain { store.core.draftText(id) })

    /** The text as the widget lays it out: with the styling spans. Call on the main thread. */
    private fun shown(): Spanned = text.layout.text as Spanned

    /** Where each hidden piece of syntax starts. Call on the main thread. */
    private fun hiddenStarts(): List<Int> {
        val shown = shown()
        return shown.getSpans(0, shown.length, MarkdownStyler.HiddenSpan::class.java)
            .map(shown::getSpanStart)
            .sorted()
    }

    @Test
    fun keyboardInputKeepsTheDraftInStep() {
        val steps: List<(InputConnection) -> Unit> = listOf(
            { it.commitText("Hello ", 1) },
            { it.setComposingText("世", 1) },
            { it.setComposingText("世界", 1) },
            { it.finishComposingText() },
            { it.commitText(" 🌿", 1) },
            // A keyboard rewrites the word it's composing on each keystroke.
            { it.setComposingText("hel", 1) },
            { it.setComposingText("hell", 1) },
            { it.setComposingText("hello", 1) },
            { it.finishComposingText() },
            { it.deleteSurroundingText(5, 0) },
            // The emoji is two UTF-16 units.
            { it.deleteSurroundingText(2, 0) },
        )
        for (step in steps) {
            onMain { step(input) }
            assertInStep()
        }
        assertEquals("Hello 世界 ", content())
    }

    @Test
    fun anEditBeforeTheCursorKeepsItInPlace() {
        onMain {
            input.commitText("world", 1)
            text.setSelection(2)
            // Another device's edit, as the core hands it over.
            store.listener!!.applied(Applied(listOf(TextEdit.Insert(0, "Big ")), null))
        }
        assertEquals("Big world", content())
        assertEquals(6, onMain { text.selectionStart })
    }

    @Test
    fun enterContinuesAndEndsLists() {
        onMain { input.commitText("- [ ] item", 1) }
        onMain { input.commitText("\n", 1) }
        waitUntil("the next item") { text.text.toString() == "- [ ] item\n- [ ] " }
        onMain { input.commitText("\n", 1) }
        waitUntil("the list to end") { text.text.toString() == "- [ ] item\n" }
        assertInStep()
        onMain {
            input.commitText("- one", 1)
            text.dispatchKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER))
            text.dispatchKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_ENTER))
        }
        waitUntil("one continuation") { text.text.toString() == "- [ ] item\n- one\n- " }
        assertInStep()
    }

    @Test
    fun listsDoNotContinueInCodeBlocks() {
        onMain { input.commitText("```\n- code\n```", 1) }
        // Let the parse finish, so the editor knows where the code block is.
        Thread.sleep(300)
        onMain {
            text.setSelection("```\n- code".length)
            input.commitText("\n", 1)
        }
        Thread.sleep(300)
        assertEquals("```\n- code\n\n```", content())
    }

    @Test
    fun syntaxShowsOnlyInTheActiveBlock() {
        onMain { input.commitText("**one**\n\n**two**", 1) }
        // The cursor is in the second block, so the first one's syntax hides.
        waitUntil("the first block's syntax to hide") { hiddenStarts() == listOf(0, 5) }
        onMain { text.setSelection(1) }
        assertEquals(listOf(9, 14), onMain { hiddenStarts() })
        onMain { text.setSelection(0, text.length()) }
        assertEquals(emptyList<Int>(), onMain { hiddenStarts() })
    }

    @Test
    fun stylingKeepsFollowingEditsAfterRecreation() {
        onMain { input.commitText("Before\n\n**bold**\n\nAfter", 1) }
        waitUntil("bold styling") { shown().getSpans(0, text.length(), StyleSpan::class.java).isNotEmpty() }
        scenario.recreate()
        scenario.onActivity { text = it.findViewById(R.id.text) }
        onMain {
            val span = shown().getSpans(0, text.length(), StyleSpan::class.java).single()
            val start = shown().getSpanStart(span)
            val end = shown().getSpanEnd(span)
            text.text.insert(0, "Added ")
            // Existing spans must move with typing immediately, before parsing.
            assertEquals(start + 6, shown().getSpanStart(span))
            assertEquals(end + 6, shown().getSpanEnd(span))
            text.text.append("\n\n*new emphasis*")
        }
        waitUntil("new formatting after recreation") {
            shown().getSpans(0, text.length(), StyleSpan::class.java).size == 2
        }
        assertInStep()
    }

    @Test
    fun trashedNotesRejectUndoAndRedoFromKeyboardAndTextMenu() {
        onMain { input.commitText("Original", 1) }
        Thread.sleep(600)
        onMain {
            input.commitText(" revised", 1)
            text.onTextContextMenuItem(android.R.id.undo)
        }
        assertEquals("Original", content())
        onMain {
            store.trashOrRestore()
            store.showFilter(Filter.Trash)
        }
        waitUntil("the trashed note") { store.notes.any { it.id == id } }
        onMain { store.openNote(id) }
        waitUntil("the read-only editor") { store.activeNote?.id == id && store.activeNote?.deleted == true }
        val shortcuts = listOf(
            KeyEvent.KEYCODE_Z to KeyEvent.META_CTRL_ON,
            KeyEvent.KEYCODE_Z to (KeyEvent.META_CTRL_ON or KeyEvent.META_SHIFT_ON),
            KeyEvent.KEYCODE_Y to KeyEvent.META_CTRL_ON,
        )
        for ((key, modifiers) in shortcuts) {
            scenario.onActivity {
                assertTrue(it.dispatchKeyEvent(KeyEvent(0L, 0L, KeyEvent.ACTION_DOWN, key, 0, modifiers)))
            }
            assertEquals("Original", content())
            assertInStep()
        }
        for (action in listOf(android.R.id.undo, android.R.id.redo)) {
            onMain { text.onTextContextMenuItem(action) }
            assertEquals("Original", content())
            assertInStep()
        }
        onMain { store.trashOrRestore() }
        waitUntil("the note to be restored") { store.activeNote?.deleted == false }
        onMain { text.onTextContextMenuItem(android.R.id.redo) }
        assertEquals("Original revised", content())
        onMain { text.onTextContextMenuItem(android.R.id.undo) }
        assertEquals("Original", content())
        assertInStep()
    }

    @Test
    fun copyingKeepsMarkdownAndPastingDropsFormatting() {
        val clipboard = context.getSystemService(ClipboardManager::class.java)
        onMain { input.commitText("Plant **tomatoes**", 1) }
        waitUntil("styling") { hiddenStarts().isEmpty() && shown().getSpans(0, text.length(), StyleSpan::class.java).isNotEmpty() }
        onMain {
            text.selectAll()
            text.onTextContextMenuItem(android.R.id.copy)
        }
        assertEquals("Plant **tomatoes**", onMain { clipboard.primaryClip!!.getItemAt(0).coerceToText(context).toString() })

        onMain {
            clipboard.setPrimaryClip(ClipData.newHtmlText("rich", "bold", "<b>bold</b>"))
            text.setSelection(text.length())
            text.onTextContextMenuItem(android.R.id.paste)
        }
        assertEquals("Plant **tomatoes**bold", content())
        assertInStep()
        Thread.sleep(300)
        val pasted = onMain { shown().getSpans(text.length() - 3, text.length(), StyleSpan::class.java).toList() }
        assertTrue("pasted text kept foreign formatting: $pasted", pasted.isEmpty())
    }

    /** Typing at the top shifts every later span, exposing layout regressions. */
    @Test
    fun aLongNoteStaysResponsive() {
        val line = "- [ ] A **task** with _emphasis_ and `code` 🌿\n"
        val body = line.repeat(50_000 / line.length + 1)
        val loaded = SystemClock.uptimeMillis()
        onMain { input.commitText(body, 1) }
        val inserted = SystemClock.uptimeMillis()
        waitUntil("styling", timeoutMs = 30_000) { hiddenStarts().isNotEmpty() }
        val styled = SystemClock.uptimeMillis()
        assertTrue(onMain { text.text.getSpans(0, text.length(), MarkdownStyler.HiddenSpan::class.java).isEmpty() })

        onMain { text.setSelection(0) }
        val keystrokes = (1..20).map {
            onMain {
                val start = SystemClock.uptimeMillis()
                input.commitText("x", 1)
                SystemClock.uptimeMillis() - start
            }
        }
        Log.i("NotebookTiming", "50k note: insert ${inserted - loaded} ms, styled after ${styled - inserted} ms, keystrokes at the top max ${keystrokes.max()} ms, median ${keystrokes.sorted()[10]} ms")
        assertInStep()
        assertTrue("a keystroke took ${keystrokes.max()} ms", keystrokes.max() < 100)

        // After a pause the parse catches up: the first line, typed over, is
        // no longer a list item, so it loses its hanging indent.
        waitUntil("styling after typing") {
            shown().getSpans(0, 30, android.text.style.LeadingMarginSpan::class.java).isEmpty()
        }
    }
}
