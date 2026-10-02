package com.pkkulhari.notebook

import android.view.View
import android.widget.EditText
import android.widget.Switch
import android.widget.TextView
import androidx.test.core.app.ActivityScenario
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.pkkulhari.notebook.core.Pairing
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class SyncTest {
    @Test
    fun theSyncScreenTurnsSyncOnRenamesAndShowsACode() {
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            lateinit var store: Store
            lateinit var activity: MainActivity
            scenario.onActivity {
                activity = it
                store = (it.application as NotebookApp).store
            }
            waitUntil("a sync status") { store.syncStatus != null }
            onMain {
                activity.findViewById<View>(R.id.sync_button).performClick()
                activity.findViewById<Switch>(R.id.sync_enabled).isChecked = true
            }
            waitUntil("sync to run") { store.syncStatus!!.running }

            onMain {
                val name = activity.findViewById<EditText>(R.id.device_name)
                name.setText("Test phone")
                name.onEditorAction(android.view.inputmethod.EditorInfo.IME_ACTION_DONE)
            }
            waitUntil("the new name") { store.syncStatus!!.deviceName == "Test phone" }

            onMain { activity.findViewById<View>(R.id.show_code).performClick() }
            waitUntil("a code") { store.syncStatus!!.pairing is Pairing.Showing }
            val shown = onMain { activity.findViewById<TextView>(R.id.pairing_code).text.toString() }
            assertEquals((store.syncStatus!!.pairing as Pairing.Showing).code, shown)
            assertTrue(Regex("\\d{3} \\d{3}").matches(shown))
            onMain { activity.findViewById<View>(R.id.cancel_code).performClick() }
            waitUntil("pairing to stop") { store.syncStatus!!.pairing is Pairing.Idle }

            // A quick trip away, like a rotation, keeps sync running.
            onMain {
                store.foreground(false)
                store.foreground(true)
            }
            Thread.sleep(Store.SUSPEND_DELAY_MS + 500)
            assertTrue(onMain { store.syncStatus!!.running && !store.syncStatus!!.suspended })

            // Staying away suspends sync without turning it off.
            onMain { store.foreground(false) }
            waitUntil("suspension", timeoutMs = Store.SUSPEND_DELAY_MS + 10_000) {
                store.syncStatus!!.suspended && !store.syncStatus!!.running
            }
            assertTrue(onMain { store.syncStatus!!.enabled })
            onMain { store.foreground(true) }
            waitUntil("sync to resume") { store.syncStatus!!.running && !store.syncStatus!!.suspended }

            onMain { activity.findViewById<Switch>(R.id.sync_enabled).isChecked = false }
            waitUntil("sync to stop") { !store.syncStatus!!.enabled && !store.syncStatus!!.running }
            onMain { activity.findViewById<View>(R.id.sync_back).performClick() }
        }
    }
}
