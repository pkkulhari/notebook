package com.pkkulhari.notebook

import android.content.ClipData
import android.content.ClipboardManager
import android.os.SystemClock
import android.text.style.StyleSpan
import android.util.Log
import android.view.KeyEvent
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputConnection
import android.view.inputmethod.InputMethodManager
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.pkkulhari.notebook.core.Applied
import com.pkkulhari.notebook.core.TextEdit
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/** The Markdown editor, driven the way a keyboard drives it. */
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
        // The test plays the keyboard. A real one stays attached to a focused
        // field and finishes compositions it didn't start, so it's sent away.
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

    /** The text widget and the core's draft hold the same text. */
    private fun assertInStep() = assertEquals(content(), onMain { store.core.draftText(id) })

    /** Where each hidden piece of syntax starts. Call on the main thread. */
    private fun hiddenStarts(): List<Int> {
        val editable = text.text
        return editable.getSpans(0, editable.length, MarkdownStyler.HiddenSpan::class.java)
            .map(editable::getSpanStart)
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
        // A hardware Enter key gives one newline and one continuation.
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
    fun copyingKeepsMarkdownAndPastingDropsFormatting() {
        val clipboard = context.getSystemService(ClipboardManager::class.java)
        onMain { input.commitText("Plant **tomatoes**", 1) }
        waitUntil("styling") { hiddenStarts().isEmpty() && text.text.getSpans(0, text.length(), StyleSpan::class.java).isNotEmpty() }
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
        val pasted = onMain { text.text.getSpans(text.length() - 3, text.length(), StyleSpan::class.java).toList() }
        assertTrue("pasted text kept foreign formatting: $pasted", pasted.isEmpty())
    }

    /** Logs timings for a 50,000-character note; the bounds only catch something badly wrong. */
    @Test
    fun aLongNoteStaysResponsive() {
        val line = "- [ ] A **task** with _emphasis_ and `code` 🌿\n"
        val body = line.repeat(50_000 / line.length + 1)
        val loaded = SystemClock.uptimeMillis()
        onMain { input.commitText(body, 1) }
        val inserted = SystemClock.uptimeMillis()
        waitUntil("styling", timeoutMs = 30_000) { hiddenStarts().isNotEmpty() }
        val styled = SystemClock.uptimeMillis()
        val keystrokes = (1..20).map {
            onMain {
                val start = SystemClock.uptimeMillis()
                input.commitText("x", 1)
                SystemClock.uptimeMillis() - start
            }
        }
        Log.i("NotebookTiming", "50k note: insert ${inserted - loaded} ms, styled after ${styled - inserted} ms, keystrokes max ${keystrokes.max()} ms, median ${keystrokes.sorted()[10]} ms")
        assertInStep()
        assertTrue("a keystroke took ${keystrokes.max()} ms", keystrokes.max() < 100)
    }
}
