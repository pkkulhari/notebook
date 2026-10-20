package com.pkkulhari.notebook

import android.content.ClipData
import android.content.ClipboardManager
import android.os.SystemClock
import android.text.Spanned
import android.text.style.StyleSpan
import android.util.Log
import android.widget.CheckBox
import android.widget.FrameLayout
import android.view.KeyEvent
import android.view.ActionMode
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
        // Within the keyboard's own edit: the result is there when it returns.
        onMain { input.commitText("- [ ] item", 1) }
        onMain { input.commitText("\n", 1) }
        assertEquals("- [ ] item\n- [ ] " to "- [ ] item\n- [ ] ".length, onMain { text.text.toString() to text.selectionStart })
        onMain { input.commitText("\n", 1) }
        assertEquals("- [ ] item\n", content())
        assertInStep()
        onMain {
            input.commitText("- one", 1)
            text.dispatchKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ENTER))
            text.dispatchKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_ENTER))
        }
        assertEquals("- [ ] item\n- one\n- ", content())
        assertInStep()
    }

    @Test
    fun listsDoNotContinueInCodeBlocks() {
        onMain {
            input.commitText("```\n- code\n```", 1)
            text.setSelection("```\n- code".length)
            input.commitText("\n", 1)
        }
        assertEquals("```\n- code\n\n```", content())
        // A fence typed just now counts, before any parse.
        onMain {
            text.setSelection(text.length())
            input.commitText("\n\n~~~\n- also code", 1)
            input.commitText("\n", 1)
        }
        assertEquals("```\n- code\n\n```\n\n~~~\n- also code\n", content())
        assertInStep()
    }

    @Test
    fun syntaxStaysHiddenWhileEditingAndSelecting() {
        onMain { input.commitText("**one**\n\n**two**", 1) }
        // Both blocks stay rendered, including the active selection.
        waitUntil("the first block's syntax to hide") { hiddenStarts() == listOf(0, 5, 9, 14) }
        onMain { text.setSelection(1) }
        assertEquals(listOf(0, 5, 9, 14), onMain { hiddenStarts() })
        onMain { text.setSelection(0, text.length()) }
        assertEquals(listOf(0, 5, 9, 14), onMain { hiddenStarts() })
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
    fun copyingUsesVisibleTextAndPastingDropsFormatting() {
        val clipboard = context.getSystemService(ClipboardManager::class.java)
        onMain { input.commitText("Plant **tomatoes**", 1) }
        waitUntil("styling") { hiddenStarts().isNotEmpty() && shown().getSpans(0, text.length(), StyleSpan::class.java).isNotEmpty() }
        onMain {
            text.selectAll()
            text.onTextContextMenuItem(android.R.id.copy)
        }
        assertEquals("Plant tomatoes", onMain { clipboard.primaryClip!!.getItemAt(0).coerceToText(context).toString() })

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

    private fun shortcut(code: Int, shift: Boolean = false) {
        text.onKeyDown(code, KeyEvent(0, 0, KeyEvent.ACTION_DOWN, code, 0,
            KeyEvent.META_CTRL_ON or if (shift) KeyEvent.META_SHIFT_ON else 0))
    }

    @Test
    fun formattingSelectionAndUndoHaveSeparateSteps() {
        onMain {
            input.commitText("🌿 hello\nsecond\nlast", 1)
            text.setSelection(3, 8)
            shortcut(KeyEvent.KEYCODE_B)
        }
        assertEquals("🌿 **hello**\nsecond\nlast", content())
        assertInStep()
        onMain { text.onTextContextMenuItem(android.R.id.undo) }
        assertEquals("🌿 hello\nsecond\nlast", content())
        onMain { text.onTextContextMenuItem(android.R.id.redo) }
        assertEquals("🌿 **hello**\nsecond\nlast", content())
        onMain {
            text.setSelection(5, 10)
            shortcut(KeyEvent.KEYCODE_I)
        }
        assertEquals("🌿 ***hello***\nsecond\nlast", content())
        onMain {
            text.setSelection(0, text.text.indexOf("last"))
            shortcut(KeyEvent.KEYCODE_8, shift = true)
        }
        assertEquals("1. 🌿 ***hello***\n2. second\nlast", content())
        assertInStep()
    }

    @Test
    fun selectionToolbarFormatsEverySelectedLine() {
        onMain {
            input.commitText("one\ntwo\nlast", 1)
            text.isFocusableInTouchMode = true
            text.requestFocus()
            text.setSelection(0, 8)
            val callback = text.customSelectionActionModeCallback!!
            val mode = text.startActionMode(callback, ActionMode.TYPE_FLOATING)!!
            mode.invalidate()
            val item = (0 until mode.menu.size()).map(mode.menu::getItem).single { it.title == "Heading 4" }
            assertTrue(callback.onActionItemClicked(mode, item))
        }
        assertEquals("#### one\n#### two\nlast", content())
        assertInStep()
        onMain { text.onTextContextMenuItem(android.R.id.undo) }
        assertEquals("one\ntwo\nlast", content())
    }

    @Test
    fun slashCommandsWorkWithImeAndHardwareKeys() {
        onMain { input.commitText("🌿 /h6", 1) }
        // Let the menu's posted update run, as between actual keystrokes.
        onMain { input.commitText("\n", 1) }
        assertEquals("###### 🌿 ", content())
        onMain {
            text.setSelection(text.length())
            input.commitText("\n/todo", 1)
        }
        onMain { text.onKeyDown(KeyEvent.KEYCODE_TAB, KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_TAB)) }
        assertEquals("###### 🌿 \n- [ ] ", content())
        assertInStep()
        onMain {
            input.commitText("task\n", 1)
            input.commitText("/quote", 1)
        }
        onMain { text.onKeyDown(KeyEvent.KEYCODE_ESCAPE, KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_ESCAPE)) }
        onMain { input.commitText("!", 1) }
        assertTrue(content().endsWith("/quote!"))
        assertInStep()
    }

    @Test
    fun slashMenuSurvivesAutosaveWhileFiltering() {
        var echoes = 0
        val listener = onMain { store.listener!! }
        onMain {
            store.listener = object : Store.Listener by listener {
                override fun applied(applied: Applied) {
                    listener.applied(applied)
                    if (applied.edits.isEmpty()) echoes++
                }
            }
        }
        try {
            onMain { input.commitText("/", 1) }
            waitUntil("the slash to autosave and echo back without text changes") { echoes > 0 }
            val saved = onMain { echoes }
            onMain { input.commitText("h2", 1) }
            waitUntil("the filtered command to autosave") { echoes > saved }
            onMain { input.commitText("\n", 1) }
            assertEquals("## ", content())
            assertInStep()
        } finally {
            onMain { store.listener = listener }
        }
    }

    @Test
    fun remoteTextEditsStillDismissTheSlashMenu() {
        onMain { input.commitText("/h2", 1) }
        onMain {
            // The draft already contains incoming text when it notifies the view.
            store.core.replace(id, 0, 0, "Remote ")
            store.listener!!.applied(Applied(listOf(TextEdit.Insert(0, "Remote ")), null))
        }
        onMain { input.commitText("\n", 1) }
        assertEquals("Remote /h2\n", content())
        assertInStep()
    }

    @Test
    fun hiddenDelimitersSurviveCompositionSpaceAndVisibleDeletion() {
        onMain { input.commitText("**hello**", 1) }
        waitUntil("hidden delimiters") { hiddenStarts() == listOf(0, 7) }
        onMain {
            text.setSelection(7)
            input.setComposingRegion(2, 7)
            input.setComposingText("hello!", 1)
            input.finishComposingText()
        }
        assertEquals("**hello!**", content())
        onMain { input.commitText(" ", 1) }
        assertEquals("**hello!** ", content())
        assertInStep()
        onMain {
            text.setSelection(10)
            input.deleteSurroundingText(1, 0)
        }
        assertEquals("**hello** ", content())
        onMain {
            text.setSelection(2, 7)
            input.commitText("🌿", 1)
        }
        assertEquals("🌿 ", content())
        assertInStep()
    }

    private fun checkboxes(): List<CheckBox> {
        val frame = text.parent as FrameLayout
        return (0 until frame.childCount).map(frame::getChildAt).filterIsInstance<CheckBox>()
    }

    @Test
    fun checkboxesToggleUndoAndTrackEdits() {
        onMain { input.commitText("- [ ] first\n  - [X] nested\n\n```\n- [ ] code\n```", 1) }
        waitUntil("two native checkboxes") { checkboxes().size == 2 }
        val original = content()
        val cursor = onMain { text.selectionStart }
        onMain {
            val button = checkboxes().first { it.contentDescription == "first" }
            assertTrue(button.isFocusable)
            button.performClick()
        }
        assertEquals(original.replaceFirst("[ ]", "[x]"), content())
        assertEquals(cursor, onMain { text.selectionStart })
        assertInStep()
        onMain { text.onTextContextMenuItem(android.R.id.undo) }
        assertEquals(original, content())
        onMain { text.onTextContextMenuItem(android.R.id.redo) }
        assertTrue(content().startsWith("- [x]"))
        onMain { text.text.insert(0, "🌿\n\n") }
        waitUntil("checkboxes after an insertion") { checkboxes().size == 2 && checkboxes().any { it.contentDescription == "nested" } }
        onMain { checkboxes().first { it.contentDescription == "nested" }.performClick() }
        assertTrue(content().contains("  - [ ] nested"))
        assertInStep()
        waitUntil("the updated first checkbox") {
            checkboxes().any { it.contentDescription == "first" && it.isChecked } &&
                shown().getSpans(0, shown().length, MarkdownStyler.TaskSpan::class.java)
                    .any { it.checked && shown().getSpanStart(it) == text.text.indexOf("[x] first") }
        }
        val toggledButton = onMain {
            val button = checkboxes().first { it.contentDescription == "first" }
            assertTrue(button.requestFocusFromTouch())
            button.dispatchKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_SPACE))
            button.dispatchKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_SPACE))
            button
        }
        assertTrue(content().contains("- [ ] first"))
        waitUntil("the unchecked task span") {
            shown().getSpans(0, shown().length, MarkdownStyler.TaskSpan::class.java).all { !it.checked } &&
                checkboxes().any { it.contentDescription == "first" && it !== toggledButton }
        }
        onMain {
            val button = checkboxes().first { it.contentDescription == "first" }
            assertTrue("toggling must preserve checkbox focus after parsing", button.hasFocus())
            button.dispatchKeyEvent(KeyEvent(KeyEvent.ACTION_DOWN, KeyEvent.KEYCODE_SPACE))
            button.dispatchKeyEvent(KeyEvent(KeyEvent.ACTION_UP, KeyEvent.KEYCODE_SPACE))
        }
        assertTrue(content().contains("- [x] first"))
        val beforeTrash = content()
        onMain {
            store.trashOrRestore()
            store.showFilter(Filter.Trash)
        }
        waitUntil("the trashed task note") { store.notes.any { it.id == id } }
        onMain { store.openNote(id) }
        waitUntil("disabled checkboxes") { checkboxes().size == 2 && checkboxes().all { !it.isEnabled } }
        onMain { checkboxes().first().performClick() }
        assertEquals(beforeTrash, content())
        assertInStep()
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
