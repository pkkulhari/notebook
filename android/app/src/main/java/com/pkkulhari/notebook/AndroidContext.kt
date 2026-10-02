package com.pkkulhari.notebook

import android.content.Context

/** The one hand-written JNI call: iroh reads the network's DNS servers through the app's context. */
object AndroidContext {
    init {
        // This can run before JNA has loaded the library for the bindings.
        System.loadLibrary("notebook_ffi")
    }

    /** Call once, before starting the core. Later calls do nothing. */
    @JvmStatic
    external fun install(context: Context)
}
