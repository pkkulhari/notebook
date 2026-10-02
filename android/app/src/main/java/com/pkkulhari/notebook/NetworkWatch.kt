package com.pkkulhari.notebook

import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper

/**
 * What sync needs from Android while the app is visible: word of network
 * changes, which iroh can't see on Android, and a multicast lock, without
 * which many phones drop the mDNS packets devices find each other with.
 */
class NetworkWatch(context: Context, private val onChange: () -> Unit) {
    private val connectivity = context.getSystemService(ConnectivityManager::class.java)
    private val lock = context.getSystemService(WifiManager::class.java)
        .createMulticastLock("notebook-sync")
        .apply { setReferenceCounted(false) }
    private val handler = Handler(Looper.getMainLooper())
    private val changed = Runnable { onChange() }
    private var watching = false

    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = soon()

        override fun onLost(network: Network) = soon()

        override fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) = soon()
    }

    /** Networks change in bursts; one nudge after they settle is enough. */
    private fun soon() {
        handler.removeCallbacks(changed)
        handler.postDelayed(changed, SETTLE_MS)
    }

    fun start() {
        if (watching) return
        connectivity.registerDefaultNetworkCallback(callback, handler)
        watching = true
    }

    fun stop() {
        if (!watching) return
        connectivity.unregisterNetworkCallback(callback)
        handler.removeCallbacks(changed)
        watching = false
    }

    /** Holds the multicast lock while `held`. */
    fun multicast(held: Boolean) {
        if (held && !lock.isHeld) lock.acquire()
        if (!held && lock.isHeld) lock.release()
    }

    companion object {
        private const val SETTLE_MS = 500L
    }
}
