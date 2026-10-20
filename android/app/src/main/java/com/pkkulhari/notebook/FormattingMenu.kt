package com.pkkulhari.notebook

import android.graphics.Rect
import android.graphics.drawable.ColorDrawable
import android.content.Context
import android.view.ActionMode
import android.view.Gravity
import android.view.KeyEvent
import android.view.Menu
import android.view.MenuItem
import android.view.View
import android.view.ViewGroup
import android.widget.ArrayAdapter
import android.widget.ListView
import android.widget.PopupWindow
import android.widget.TextView
import com.pkkulhari.notebook.core.FormatCommand
import com.pkkulhari.notebook.core.SlashQuery
import com.pkkulhari.notebook.core.formatCommands
import com.pkkulhari.notebook.core.slashQuery

/** Touch selection uses Android's floating toolbar; slash commands keep IME focus. */
class FormattingMenu(
    private val text: NoteEditText,
    private val editable: () -> Boolean,
    private val apply: (FormatCommand, Boolean) -> Unit,
) {
    private val list = ListView(text.context)
    private val popup = PopupWindow(list, text.context.dp(280), text.context.dp(240), false).apply {
        setBackgroundDrawable(ColorDrawable(text.context.getColor(R.color.floating)))
        elevation = text.context.dp(8).toFloat()
        isOutsideTouchable = true
        inputMethodMode = PopupWindow.INPUT_METHOD_NEEDED
    }
    private var query: SlashQuery? = null
    private var commands = emptyList<FormatCommand>()
    private var selected = 0
    private var mayOpen = false
    private var mode: ActionMode? = null
    private val refresh = Runnable { refresh() }

    /** Symbols and Markdown hints come from the desktop's shared command catalog. */
    private class CommandAdapter(context: Context, commands: List<FormatCommand>) :
        ArrayAdapter<FormatCommand>(context, R.layout.format_command_row, R.id.command_label, commands) {
        override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
            val view = super.getView(position, convertView, parent)
            val command = getItem(position)!!
            view.findViewById<TextView>(R.id.command_icon).text = command.icon
            view.findViewById<TextView>(R.id.command_label).text = command.label
            view.findViewById<TextView>(R.id.command_hint).text = command.hint
            view.contentDescription = command.label
            return view
        }
    }

    init {
        list.choiceMode = ListView.CHOICE_MODE_SINGLE
        list.setOnItemClickListener { _, _, position, _ -> activate(position) }
        popup.setOnDismissListener { query = null }
        text.customSelectionActionModeCallback = object : ActionMode.Callback {
            override fun onCreateActionMode(mode: ActionMode, menu: Menu): Boolean {
                this@FormattingMenu.mode = mode
                return true
            }

            override fun onPrepareActionMode(mode: ActionMode, menu: Menu): Boolean {
                menu.removeGroup(FORMAT_GROUP)
                if (editable() && text.selectionStart != text.selectionEnd) {
                    for (command in formatCommands("", true)) {
                        menu.add(FORMAT_GROUP, FORMAT_BASE + command.id, Menu.NONE, command.label)
                            .setShowAsAction(MenuItem.SHOW_AS_ACTION_IF_ROOM)
                    }
                }
                return true
            }

            override fun onActionItemClicked(mode: ActionMode, item: MenuItem): Boolean {
                if (item.groupId != FORMAT_GROUP) return false
                formatCommands("", true).find { FORMAT_BASE + it.id == item.itemId }?.let { apply(it, false) }
                mode.finish()
                return true
            }

            override fun onDestroyActionMode(mode: ActionMode) { this@FormattingMenu.mode = null }
        }
    }

    fun changed(insertedSlash: Boolean = false) {
        mayOpen = mayOpen || insertedSlash
        text.removeCallbacks(refresh)
        text.post(refresh)
    }

    private fun refresh() {
        val open = mayOpen
        mayOpen = false
        if (!text.isAttachedToWindow || !editable() || text.selectionStart < 0 || text.selectionStart != text.selectionEnd) {
            dismiss()
            return
        }
        if (!open && !popup.isShowing) return
        val next = slashQuery(text.text.toString(), text.selectionStart)
        if (next == null) {
            dismiss()
            return
        }
        if (query == next && popup.isShowing) return
        query = next
        commands = formatCommands(next.text, false)
        selected = 0
        list.adapter = if (commands.isEmpty()) {
            ArrayAdapter(text.context, android.R.layout.simple_list_item_activated_1, listOf("No matching commands"))
        } else {
            CommandAdapter(text.context, commands)
        }
        list.setItemChecked(0, commands.isNotEmpty())
        val bounds = Rect()
        text.getWindowVisibleDisplayFrame(bounds)
        val location = IntArray(2)
        text.getLocationOnScreen(location)
        val layout = text.layout ?: return
        val line = layout.getLineForOffset(text.selectionEnd)
        val cursorY = location[1] + text.totalPaddingTop + layout.getLineBottom(line) - text.scrollY
        val height = minOf(text.context.dp(240), text.context.dp(48) * maxOf(1, commands.size), bounds.height())
        val width = minOf(text.context.dp(280), bounds.width())
        val x = location[0].coerceIn(bounds.left, maxOf(bounds.left, bounds.right - width))
        val y = (if (cursorY + height <= bounds.bottom) cursorY else cursorY - height - text.lineHeight)
            .coerceIn(bounds.top, maxOf(bounds.top, bounds.bottom - height))
        if (popup.isShowing) popup.update(x, y, width, height)
        else {
            popup.width = width
            popup.height = height
            popup.showAtLocation(text, Gravity.TOP or Gravity.LEFT, x, y)
        }
    }

    fun key(key: Int): Boolean {
        // IME commitText can arrive before the posted refresh.
        if (mayOpen) { text.removeCallbacks(refresh); refresh() }
        if (!popup.isShowing) return false
        when (key) {
            KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_BACK -> dismiss()
            KeyEvent.KEYCODE_DPAD_UP, KeyEvent.KEYCODE_DPAD_DOWN -> if (commands.isNotEmpty()) {
                selected = (selected + if (key == KeyEvent.KEYCODE_DPAD_UP) commands.size - 1 else 1) % commands.size
                list.setItemChecked(selected, true)
                list.smoothScrollToPosition(selected)
            }
            KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_TAB -> {
                if (commands.isEmpty()) { dismiss(); return false }
                activate(selected)
            }
            else -> return false
        }
        return true
    }

    private fun activate(position: Int) {
        val command = commands.getOrNull(position) ?: return
        val current = query
        if (current != null && editable() && text.selectionStart == text.selectionEnd &&
            slashQuery(text.text.toString(), text.selectionEnd) == current) apply(command, true)
        dismiss()
    }

    fun dismiss() {
        text.removeCallbacks(refresh)
        mayOpen = false
        query = null
        popup.dismiss()
    }

    fun close() {
        dismiss()
        mode?.finish()
    }

    companion object {
        private const val FORMAT_GROUP = 0x4e4200
        private const val FORMAT_BASE = 0x4e4300
    }
}
