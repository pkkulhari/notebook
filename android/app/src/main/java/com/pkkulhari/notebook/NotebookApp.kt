package com.pkkulhari.notebook

import android.app.Application
import android.provider.Settings
import android.os.Build
import java.io.File

/** Starts the core once per process; it lives as long as the process does. */
open class NotebookApp : Application() {
    lateinit var store: Store
        private set

    override fun onCreate() {
        super.onCreate()
        AndroidContext.install(this)
        store = Store(this, databaseFile(), syncConfigFile(), deviceName())
    }

    protected open fun databaseFile() = File(filesDir, "notebook.db")

    /** Never backed up: it holds this device's private sync key. */
    protected open fun syncConfigFile() = File(noBackupFilesDir, "sync.json")

    private fun deviceName(): String =
        Settings.Global.getString(contentResolver, Settings.Global.DEVICE_NAME)
            ?.takeIf { it.isNotBlank() }
            ?: Build.MODEL
}
