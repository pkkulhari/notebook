package com.pkkulhari.notebook

import android.app.Application
import android.content.Context
import androidx.test.runner.AndroidJUnitRunner
import java.io.File

class TestRunner : AndroidJUnitRunner() {
    override fun newApplication(loader: ClassLoader, name: String, context: Context): Application =
        super.newApplication(loader, TestApp::class.java.name, context)
}

/** The app with a fresh database in its cache, so tests never touch real notes. */
class TestApp : NotebookApp() {
    private val directory by lazy { File(cacheDir, "app-under-test").apply { deleteRecursively(); mkdirs() } }

    override fun databaseFile() = File(directory, "notebook.db")

    override fun syncConfigFile() = File(directory, "sync.json")
}
