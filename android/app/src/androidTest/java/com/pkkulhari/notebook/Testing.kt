package com.pkkulhari.notebook

import android.os.SystemClock
import androidx.test.platform.app.InstrumentationRegistry
import com.pkkulhari.notebook.core.Applied
import com.pkkulhari.notebook.core.NoteInfo
import java.io.File
import org.junit.Assert.fail

/** Runs [block] on the main thread, where the store lives, and returns its result. */
fun <T> onMain(block: () -> T): T {
    var result: Result<T>? = null
    InstrumentationRegistry.getInstrumentation().runOnMainSync { result = runCatching(block) }
    return result!!.getOrThrow()
}

fun waitUntil(what: String, timeoutMs: Long = 10_000, condition: () -> Boolean) {
    val deadline = SystemClock.uptimeMillis() + timeoutMs
    while (!onMain(condition)) {
        if (SystemClock.uptimeMillis() > deadline) fail("Timed out waiting for $what")
        Thread.sleep(20)
    }
}

val context get() = InstrumentationRegistry.getInstrumentation().targetContext

fun temporaryDirectory(): File = File(context.cacheDir, "test-${System.nanoTime()}").apply { mkdirs() }

class Recorder : Store.Listener {
    var opened: NoteInfo? = null
    var text = ""
    var closed = 0

    override fun listChanged() {}

    override fun queryChanged() {}

    override fun opened(note: NoteInfo, text: String, cursor: Int, focus: Boolean) {
        opened = note
        this.text = text
    }

    override fun closed() {
        opened = null
        closed += 1
    }

    override fun activeChanged() {}

    override fun applied(applied: Applied) {}

    override fun problemChanged() {}

    override fun syncChanged() {}
}
