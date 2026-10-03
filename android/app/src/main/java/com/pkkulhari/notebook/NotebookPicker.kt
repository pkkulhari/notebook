package com.pkkulhari.notebook

import android.app.Activity
import android.app.AlertDialog
import android.text.InputType
import android.util.TypedValue
import android.view.Gravity
import android.view.Menu
import android.view.MenuItem
import android.view.View
import android.view.WindowManager
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageButton
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.PopupMenu
import android.widget.ScrollView
import android.widget.TextView
import com.pkkulhari.notebook.core.Filter
import com.pkkulhari.notebook.core.Notebook

class NotebookPicker(private val activity: Activity, private val store: Store) {
    fun show() {
        val rows = LinearLayout(activity).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(activity.dp(12), activity.dp(16), activity.dp(12), activity.dp(8))
        }
        val dialog = AlertDialog.Builder(activity)
            .setView(ScrollView(activity).apply { addView(rows) })
            .setPositiveButton(R.string.new_notebook) { _, _ -> askName(null) }
            .create()
        fun add(filter: Filter, book: Notebook?) {
            rows.addView(row(filter, book) { dialog.dismiss() })
        }
        add(Filter.All, null)
        for (book in store.notebooks) add(Filter.Notebook(book.id), book)
        rows.addView(
            View(activity).apply { setBackgroundColor(activity.getColor(R.color.line)) },
            LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, activity.dp(1)).apply {
                setMargins(activity.dp(16), activity.dp(8), activity.dp(16), activity.dp(8))
            },
        )
        add(Filter.Trash, null)
        dialog.show()
    }

    private fun row(filter: Filter, book: Notebook?, dismiss: () -> Unit): View {
        val selected = filter == store.filter && store.query.isBlank()
        val row = LinearLayout(activity).apply {
            orientation = LinearLayout.HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            minimumHeight = activity.dp(52)
            setPadding(activity.dp(16), 0, activity.dp(4), 0)
            isClickable = true
            background = activity.getDrawable(if (selected) R.drawable.row_selected else R.drawable.row)
            setOnClickListener {
                dismiss()
                store.showFilter(filter)
            }
        }
        row.addView(ImageView(activity).apply {
            setImageResource(
                when (filter) {
                    Filter.All -> R.drawable.ic_notes
                    Filter.Trash -> R.drawable.ic_trash
                    is Filter.Notebook -> R.drawable.ic_folder
                },
            )
            imageTintList = activity.getColorStateList(if (selected) R.color.accent_text else R.color.toolbar_icon)
        }, LinearLayout.LayoutParams(activity.dp(24), activity.dp(24)).apply { marginEnd = activity.dp(16) })
        row.addView(TextView(activity).apply {
            text = store.filterName(filter)
            textSize = 16f
            setTextColor(activity.getColorStateList(R.color.text_primary))
            if (selected) setTypeface(typeface, android.graphics.Typeface.BOLD)
        }, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
        row.addView(TextView(activity).apply {
            text = store.count(filter).toString()
            textSize = 14f
            fontFeatureSettings = "tnum"
            setTextColor(activity.getColorStateList(R.color.text_secondary))
            setPadding(activity.dp(8), 0, activity.dp(8), 0)
        })
        // Default can't be renamed or deleted; the others get a menu.
        val menu = ImageButton(activity).apply {
            setImageResource(R.drawable.ic_more)
            imageTintList = activity.getColorStateList(R.color.toolbar_icon)
            background = activity.getDrawable(selectableBackground(borderless = true))
            contentDescription = activity.getString(R.string.notebook_actions)
            visibility = if (book != null && book.id != store.defaultNotebookId) View.VISIBLE else View.INVISIBLE
            setOnClickListener { view ->
                PopupMenu(activity, view).apply {
                    menu.add(R.string.rename).setOnMenuItemClickListener {
                        dismiss()
                        askName(book)
                        true
                    }
                    menu.add(R.string.delete).setOnMenuItemClickListener {
                        dismiss()
                        confirmDelete(book!!)
                        true
                    }
                }.show()
            }
        }
        row.addView(menu, LinearLayout.LayoutParams(activity.dp(48), activity.dp(48)))
        return row
    }

    fun askName(book: Notebook?) {
        val field = EditText(activity).apply {
            hint = activity.getString(R.string.notebook_name)
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_CAP_WORDS
            isSingleLine = true
            setText(book?.name ?: "")
            selectAll()
            background = activity.getDrawable(R.drawable.field)
            setPadding(activity.dp(14), activity.dp(12), activity.dp(14), activity.dp(12))
            textSize = 16f
        }
        val frame = FrameLayout(activity).apply {
            setPadding(activity.dp(24), activity.dp(12), activity.dp(24), 0)
            addView(field)
        }
        val dialog = AlertDialog.Builder(activity)
            .setTitle(if (book == null) R.string.new_notebook else R.string.rename_notebook)
            .setView(frame)
            .setPositiveButton(if (book == null) R.string.create else R.string.rename) { _, _ ->
                val name = field.text.toString().trim()
                if (name.isEmpty()) return@setPositiveButton
                if (book == null) store.createNotebook(name) else store.renameNotebook(book.id, name)
            }
            .setNegativeButton(android.R.string.cancel, null)
            .create()
        dialog.window?.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_STATE_VISIBLE)
        dialog.show()
        field.requestFocus()
    }

    private fun confirmDelete(book: Notebook) {
        AlertDialog.Builder(activity)
            .setTitle(activity.getString(R.string.delete_notebook, book.name))
            .setMessage(R.string.delete_notebook_detail)
            .setPositiveButton(R.string.delete) { _, _ -> store.deleteNotebook(book.id) }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }

    fun move(anchor: View) {
        val note = store.activeNote ?: return
        PopupMenu(activity, anchor).apply {
            menu.add(R.string.move_to).isEnabled = false
            var current: MenuItem? = null
            for (book in store.notebooks) {
                val item = menu.add(NOTEBOOKS, Menu.NONE, Menu.NONE, book.name)
                item.setOnMenuItemClickListener {
                    store.moveActive(book.id)
                    true
                }
                if (book.id == note.notebookId) current = item
            }
            menu.setGroupCheckable(NOTEBOOKS, true, true)
            current?.isChecked = true
        }.show()
    }

    private fun selectableBackground(borderless: Boolean = false): Int {
        val value = TypedValue()
        activity.theme.resolveAttribute(
            if (borderless) android.R.attr.selectableItemBackgroundBorderless else android.R.attr.selectableItemBackground,
            value,
            true,
        )
        return value.resourceId
    }

    companion object {
        private const val NOTEBOOKS = 1
    }
}
