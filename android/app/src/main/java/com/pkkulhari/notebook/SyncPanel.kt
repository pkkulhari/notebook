package com.pkkulhari.notebook

import android.view.Gravity
import android.view.View
import android.view.inputmethod.EditorInfo
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.Switch
import android.widget.TextView
import com.pkkulhari.notebook.core.Pairing
import com.pkkulhari.notebook.core.SyncControl
import com.pkkulhari.notebook.core.SyncStatus

/**
 * The sync screen, worded like the desktop's sync dialog. It shows the
 * core's status, and sends the person's choices back as controls.
 */
class SyncPanel(
    private val root: View,
    private val store: Store,
    /** Asks for local network access before sync reaches out, where Android requires it. */
    private val allowLocalNetwork: () -> Unit,
) {
    private val enabled: Switch = root.findViewById(R.id.sync_enabled)
    private val problem: TextView = root.findViewById(R.id.sync_problem)
    private val name: EditText = root.findViewById(R.id.device_name)
    private val devices: LinearLayout = root.findViewById(R.id.devices)
    private val idle: View = root.findViewById(R.id.pairing_idle)
    private val showing: View = root.findViewById(R.id.pairing_showing)
    private val searching: View = root.findViewById(R.id.pairing_searching)
    private val hint: TextView = root.findViewById(R.id.pairing_hint)
    private val code: TextView = root.findViewById(R.id.pairing_code)
    private val joinCode: EditText = root.findViewById(R.id.join_code)
    private val relay: EditText = root.findViewById(R.id.relay)

    /** Set while widgets are updated from status, so they don't send it back. */
    private var quiet = false

    /** Shown in the problem line when local network access was refused. */
    var localNetworkDenied = false
        set(value) {
            field = value
            update()
        }

    private val core get() = store.core

    init {
        enabled.setOnCheckedChangeListener { _, on ->
            if (quiet) return@setOnCheckedChangeListener
            if (on) allowLocalNetwork()
            core.sync(SyncControl.SetEnabled(on))
        }
        name.setOnEditorActionListener { _, action, _ ->
            if (action == EditorInfo.IME_ACTION_DONE) rename()
            false
        }
        name.setOnFocusChangeListener { _, focused -> if (!focused) rename() }
        root.findViewById<Button>(R.id.show_code).setOnClickListener {
            allowLocalNetwork()
            core.sync(SyncControl.StartPairing)
        }
        root.findViewById<Button>(R.id.join).setOnClickListener { join() }
        joinCode.setOnEditorActionListener { _, action, _ ->
            if (action == EditorInfo.IME_ACTION_GO) join()
            action == EditorInfo.IME_ACTION_GO
        }
        root.findViewById<Button>(R.id.cancel_code).setOnClickListener { core.sync(SyncControl.CancelPairing) }
        root.findViewById<Button>(R.id.cancel_search).setOnClickListener { core.sync(SyncControl.CancelPairing) }
        root.findViewById<Button>(R.id.apply_relay).setOnClickListener {
            core.sync(SyncControl.SetRelay(relay.text.toString().trim().ifEmpty { null }))
        }
    }

    private fun rename() {
        val wanted = name.text.toString().trim()
        if (wanted.isNotEmpty() && wanted != store.syncStatus?.deviceName) {
            core.sync(SyncControl.SetDeviceName(wanted))
        }
    }

    private fun join() {
        allowLocalNetwork()
        core.sync(SyncControl.JoinPairing(joinCode.text.toString()))
    }

    fun update() {
        val status = store.syncStatus ?: return
        quiet = true
        try {
            show(status)
        } finally {
            quiet = false
        }
    }

    private fun show(status: SyncStatus) {
        val context = root.context
        enabled.isChecked = status.enabled
        val message = status.problem
            ?: context.getString(R.string.local_network_denied).takeIf { localNetworkDenied && status.enabled }
        problem.text = message
        problem.visibility = if (message == null) View.GONE else View.VISIBLE
        // Don't overwrite a name being typed.
        if (!name.hasFocus()) name.setText(status.deviceName)
        if (!relay.hasFocus()) relay.setText(status.relayUrl ?: "")

        devices.removeAllViews()
        if (status.devices.isEmpty()) {
            devices.addView(TextView(context).apply {
                setText(R.string.no_devices)
                setTextAppearance(android.R.style.TextAppearance_DeviceDefault_Small)
            })
        }
        for (device in status.devices) {
            val state = when {
                device.connected -> context.getString(R.string.connected)
                device.lastSynced != null -> context.getString(R.string.last_synced, noteDate(device.lastSynced!!))
                else -> context.getString(R.string.not_connected)
            }
            val row = LinearLayout(context).apply {
                orientation = LinearLayout.HORIZONTAL
                gravity = Gravity.CENTER_VERTICAL
                setPadding(0, 8, 0, 8)
            }
            row.addView(LinearLayout(context).apply {
                orientation = LinearLayout.VERTICAL
                addView(TextView(context).apply {
                    text = device.name
                    setTextAppearance(android.R.style.TextAppearance_DeviceDefault_Medium)
                })
                addView(TextView(context).apply {
                    text = state
                    setTextAppearance(android.R.style.TextAppearance_DeviceDefault_Small)
                })
            }, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
            row.addView(Button(context, null, 0, android.R.style.Widget_DeviceDefault_Button_Borderless_Colored).apply {
                setText(R.string.remove)
                setOnClickListener { core.sync(SyncControl.Forget(device.id)) }
            })
            devices.addView(row)
        }

        val pairing = status.pairing
        idle.visibility = if (pairing is Pairing.Showing || pairing is Pairing.Searching) View.GONE else View.VISIBLE
        showing.visibility = if (pairing is Pairing.Showing) View.VISIBLE else View.GONE
        searching.visibility = if (pairing is Pairing.Searching) View.VISIBLE else View.GONE
        if (pairing is Pairing.Showing) code.text = pairing.code
        hint.text = when (pairing) {
            is Pairing.Paired -> context.getString(R.string.paired_with, pairing.name)
            is Pairing.Failed -> "${pairing.message}."
            else -> context.getString(R.string.pairing_intro)
        }
        // Pairing needs the endpoint running.
        for (id in listOf(R.id.show_code, R.id.join, R.id.join_code)) {
            root.findViewById<View>(id).isEnabled = status.running
        }
    }
}
