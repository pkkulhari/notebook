package com.pkkulhari.notebook

import androidx.test.ext.junit.runners.AndroidJUnit4
import com.pkkulhari.notebook.core.Core
import com.pkkulhari.notebook.core.CoreConfig
import com.pkkulhari.notebook.core.CoreEvent
import com.pkkulhari.notebook.core.CoreListener
import com.pkkulhari.notebook.core.SyncStatus
import java.io.File
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class CoreSmokeTest {
    private lateinit var dir: File

    private class Events : CoreListener {
        val queue = LinkedBlockingQueue<CoreEvent>()

        override fun onEvent(event: CoreEvent) {
            queue.put(event)
        }

        override fun onSyncStatus(status: SyncStatus) {}

        inline fun <reified T : CoreEvent> next(): T {
            while (true) {
                val event = queue.poll(10, TimeUnit.SECONDS) ?: error("no ${T::class.simpleName}")
                if (event is T) return event
                if (event is CoreEvent.Error) error("${event.operation}: ${event.message}")
            }
        }
    }

    @Before
    fun setUp() {
        AndroidContext.install(context)
        dir = temporaryDirectory()
    }

    @After
    fun tearDown() {
        dir.deleteRecursively()
    }

    private fun config() = CoreConfig(
        databasePath = File(dir, "notebook.db").path,
        syncConfigPath = File(dir, "sync.json").path,
        deviceName = "Smoke test",
    )

    @Test
    fun textSurvivesARestart() {
        val text = "Hello 🌿 from Android"
        val first = Events()
        val id = Core.start(config(), first).use { core ->
            core.initialize()
            val ready = first.next<CoreEvent.Ready>()
            assertEquals("", core.openDraft(ready.note, ready.snapshot))
            core.replace(ready.note.id, 0, 0, text)
            core.flush()
            first.next<CoreEvent.Flushed>()
            ready.note.id
        }

        val second = Events()
        Core.start(config(), second).use { core ->
            core.load(id, 1)
            val loaded = second.next<CoreEvent.Loaded>()
            assertEquals(text, core.openDraft(loaded.note!!, loaded.snapshot))
        }
    }
}
