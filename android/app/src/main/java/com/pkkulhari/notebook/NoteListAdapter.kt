package com.pkkulhari.notebook

import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.BaseAdapter
import android.widget.TextView
import com.pkkulhari.notebook.core.NoteSummary

class NoteListAdapter(private val inflater: LayoutInflater) : BaseAdapter() {
    var notes: List<NoteSummary> = emptyList()
        set(value) {
            field = value
            notifyDataSetChanged()
        }

    private class Row(view: View) {
        val label: TextView = view.findViewById(R.id.label)
        val date: TextView = view.findViewById(R.id.date)
        val preview: TextView = view.findViewById(R.id.preview)
    }

    override fun getCount() = notes.size

    override fun getItem(position: Int) = notes[position]

    override fun getItemId(position: Int) = position.toLong()

    override fun getView(position: Int, convertView: View?, parent: ViewGroup): View {
        val view = convertView ?: inflater.inflate(R.layout.note_row, parent, false).also { it.tag = Row(it) }
        val row = view.tag as Row
        val note = notes[position]
        row.label.text = note.label
        row.date.text = noteDate(note.updatedAt)
        row.preview.text = note.preview
        row.preview.visibility = if (note.preview.isEmpty()) View.GONE else View.VISIBLE
        return view
    }
}
