package com.pkkulhari.notebook

import android.content.pm.ActivityInfo
import android.view.View
import android.widget.EditText
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MainActivityTest {
    private fun ActivityScenario<MainActivity>.text(): Pair<String, Int> {
        var state = "" to 0
        onActivity {
            val text = it.findViewById<EditText>(R.id.text)
            state = text.text.toString() to text.selectionStart
        }
        return state
    }

    @Test
    fun rotationAndRecreationKeepTextCursorAndUndo() {
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            scenario.onActivity { it.findViewById<android.view.View>(R.id.new_note).performClick() }
            scenario.onActivity {
                val text = it.findViewById<EditText>(R.id.text)
                text.text.append("First thought")
            }
            // Typing after a pause is a separate undo step.
            Thread.sleep(600)
            scenario.onActivity {
                val text = it.findViewById<EditText>(R.id.text)
                text.text.append(", second thought")
                text.setSelection(5)
            }
            scenario.onActivity { it.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE }
            Thread.sleep(500)
            assertEquals("First thought, second thought" to 5, scenario.text())

            // A dark mode switch rebuilds the activity.
            scenario.recreate()
            assertEquals("First thought, second thought" to 5, scenario.text())
            scenario.onActivity { it.findViewById<android.view.View>(R.id.undo).performClick() }
            assertEquals("First thought", scenario.text().first)
            scenario.onActivity { it.requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED }
        }
    }

    private fun assertNewNoteClearsSearch(committed: Boolean) {
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            lateinit var store: Store
            lateinit var search: EditText
            scenario.onActivity {
                store = (it.application as NotebookApp).store
                search = it.findViewById(R.id.search)
            }
            waitUntil("the store to be ready") { store.ready }
            scenario.onActivity {
                store.newNote()
                it.findViewById<EditText>(R.id.text).text.append("Visible after clearing search")
                store.closeNote()
            }
            waitUntil("the saved note") { store.notes.any { it.label == "Visible after clearing search" } }
            if (committed) {
                onMain { search.setText("unmatchedquery") }
                waitUntil("the search to finish") { store.query == "unmatchedquery" && store.notes.isEmpty() }
            }
            scenario.onActivity {
                search.setText("pending query")
                it.findViewById<View>(R.id.new_note).performClick()
                assertEquals("", search.text.toString())
                assertEquals("", store.query)
                store.closeNote()
            }
            // Allow the discarded query's debounce time to pass.
            Thread.sleep(300)
            assertEquals("", onMain { search.text.toString() })
            assertEquals("", onMain { store.query })
            waitUntil("the unfiltered list") { store.notes.any { it.label == "Visible after clearing search" } }
            scenario.onActivity { assertTrue(it.findViewById<View>(R.id.list_screen).isShown) }
        }
    }

    @Test
    fun creatingANoteClearsTheSearchFieldAndPendingSearch() = assertNewNoteClearsSearch(committed = true)

    @Test
    fun creatingANoteCancelsSearchBeforeItReachesTheStore() = assertNewNoteClearsSearch(committed = false)
}
