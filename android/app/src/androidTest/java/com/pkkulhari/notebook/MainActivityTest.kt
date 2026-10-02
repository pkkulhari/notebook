package com.pkkulhari.notebook

import android.content.pm.ActivityInfo
import android.widget.EditText
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

/** The editor keeps everything across rotation and a dark mode switch. */
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
}
