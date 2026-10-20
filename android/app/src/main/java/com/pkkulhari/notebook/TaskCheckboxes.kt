package com.pkkulhari.notebook

import android.graphics.Rect
import android.text.Spanned
import android.view.View
import android.view.ViewGroup
import android.view.ViewTreeObserver
import android.widget.CheckBox
import android.widget.FrameLayout

/** Native, accessible controls above the reserved task spans, including after scrolling. */
class TaskCheckboxes(
    private val text: NoteEditText,
    private val editable: () -> Boolean,
    private val toggle: (Int) -> Unit,
) {
    private val buttons = mutableMapOf<MarkdownStyler.TaskSpan, CheckBox>()
    private val capitalBounds = Rect()
    private val place = ViewTreeObserver.OnPreDrawListener { place(); true }
    private var attached = false

    fun open() {
        if (!attached) text.viewTreeObserver.addOnPreDrawListener(place)
        attached = true
    }

    fun close() {
        text.viewTreeObserver.removeOnPreDrawListener(place)
        attached = false
        buttons.values.forEach { (it.parent as? ViewGroup)?.removeView(it) }
        buttons.clear()
    }

    private fun place() {
        val frame = text.parent as? FrameLayout ?: return
        val layout = text.layout ?: return
        val shown = layout.text as? Spanned ?: return
        val visible = mutableSetOf<MarkdownStyler.TaskSpan>()
        val focused = buttons.entries.firstOrNull { it.value.hasFocus() }
        val focusAt = focused?.let {
            shown.getSpanStart(it.key).takeIf { at -> at >= 0 } ?: it.value.tag as? Int
        }
        val top = text.scrollY
        val bottom = top + text.height - text.totalPaddingTop - text.totalPaddingBottom
        val first = layout.getLineStart(layout.getLineForVertical(top))
        val last = layout.getLineEnd(layout.getLineForVertical(bottom))
        // A fixed capital gives a visible text center without descender space,
        // and keeps the checkbox steady as the task's wording changes.
        text.paint.getTextBounds("H", 0, 1, capitalBounds)
        for (span in shown.getSpans(first, last, MarkdownStyler.TaskSpan::class.java)) {
            val at = shown.getSpanStart(span)
            val end = shown.getSpanEnd(span)
            if (at < 0 || end > text.length() || text.text.subSequence(at, end).toString() !in listOf("[ ]", "[x]", "[X]")) continue
            val line = layout.getLineForOffset(at)
            val y = text.top + text.totalPaddingTop + layout.getLineTop(line) - text.scrollY
            val height = layout.getLineBottom(line) - layout.getLineTop(line)
            if (y + height <= text.top || y >= text.bottom) continue
            visible += span
            val button = buttons.getOrPut(span) {
                CheckBox(text.context).apply {
                    minWidth = 0
                    minHeight = 0
                    minimumWidth = 0
                    minimumHeight = 0
                    setPadding(0, 0, 0, 0)
                    importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_YES
                    setOnClickListener {
                        // Read the moving span, never a cached source offset.
                        val current = this@TaskCheckboxes.text.layout?.text as? Spanned
                        val position = current?.getSpanStart(span) ?: -1
                        if (position >= 0 && editable()) toggle(position)
                    }
                    frame.addView(this, FrameLayout.LayoutParams(span.width, maxOf(height, this@TaskCheckboxes.text.context.dp(32))))
                }
            }
            button.tag = at
            button.isChecked = text.text[at + 1] != ' '
            button.isEnabled = editable()
            val lineEnd = text.text.indexOf('\n', end).let { if (it < 0) text.length() else it }
            button.contentDescription = text.text.subSequence(end, lineEnd).toString().trim().ifEmpty { "Task" }
            // The first and last edge also handle an RTL paragraph.
            val x = minOf(layout.getPrimaryHorizontal(at), layout.getPrimaryHorizontal(end))
            button.x = text.left + text.totalPaddingLeft + x - text.scrollX
            // Baseline positioning excludes list spacing and first-line padding.
            val centerY = text.top + text.totalPaddingTop + layout.getLineBaseline(line) -
                text.scrollY + capitalBounds.exactCenterY()
            button.y = centerY - button.layoutParams.height / 2f
        }
        val obsolete = buttons.keys.filter { it !in visible }
        for (span in obsolete) frame.removeView(buttons.remove(span))
        // A toggle changes the parsed style and replaces its span. Keep keyboard
        // focus on the replacement control, so another Space toggles it again.
        if (focused != null && focused.key !in visible) {
            buttons.values.firstOrNull { it.tag == focusAt }?.requestFocus()
        }
    }
}
