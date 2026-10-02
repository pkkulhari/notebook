package com.pkkulhari.notebook

import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.Typeface
import android.os.Handler
import android.os.Looper
import android.text.Editable
import android.text.GetChars
import android.text.Layout
import android.text.Spannable
import android.text.SpannableString
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextPaint
import android.text.TextUtils
import android.text.TextWatcher
import android.text.method.TransformationMethod
import android.text.style.ForegroundColorSpan
import android.text.style.LeadingMarginSpan
import android.text.style.LineHeightSpan
import android.text.style.MetricAffectingSpan
import android.text.style.RelativeSizeSpan
import android.text.style.ReplacementSpan
import android.text.style.StrikethroughSpan
import android.text.style.StyleSpan
import android.text.style.TypefaceSpan
import android.text.style.UnderlineSpan
import android.util.TypedValue
import android.view.View
import com.pkkulhari.notebook.core.MarkdownDocument
import com.pkkulhari.notebook.core.Style
import com.pkkulhari.notebook.core.StyledRange
import com.pkkulhari.notebook.core.TextRange
import com.pkkulhari.notebook.core.parseMarkdown
import java.io.File
import java.util.concurrent.Executors
import kotlin.math.ceil

/**
 * Styles the editor's Markdown as the desktop does: formatting spans, hanging
 * indents for list items, and syntax hidden outside the block being edited.
 * The source text never changes.
 *
 * The spans don't go on the text the keyboard edits. Android lays out a
 * span's paragraph again whenever the span moves, and every keystroke moves
 * every span after the cursor, which made typing in a long note slower the
 * more it was formatted. Instead the spans live on an overlay: a copy of the
 * text that a watcher keeps in step with every edit. A [TransformationMethod]
 * shows the text widget the text with the overlay's spans, so the widget lays
 * out and draws them while the keyboard edits plain text. When the overlay's
 * spans change, only the paragraphs they're in are laid out again.
 */
class MarkdownStyler(private val text: NoteEditText) {
    private val main = Handler(Looper.getMainLooper())
    private val parser = Executors.newSingleThreadExecutor { Thread(it, "notebook-markdown").apply { isDaemon = true } }
    private val parse = Runnable { request() }

    /** Bumped on every change, so a parse of older text is dropped. */
    private var generation = 0L

    /** The newest parse requested; the parser skips anything older. */
    @Volatile private var newest = 0L

    /** The newest parse result, and whether the text is unchanged since. */
    private var document: MarkdownDocument? = null
    private var current = false

    /** The text again, carrying the styling spans. */
    private var overlay = SpannableStringBuilder()

    /** Spans on the overlay, by what they stand for. */
    private var styles = listOf<Styled>()
    private var hidden = listOf<HiddenSpan>()

    /** The blocks whose syntax shows, as of the last hidden-syntax update. */
    private var shownBlocks: List<TextRange>? = null

    /** Runs with the link under the cursor, or null. */
    var onLink: ((String?) -> Unit)? = null
    private var link: String? = null

    /** What a span stands for. A list marker's indent is measured once, when it's added. */
    private data class Key(val style: Style?, val start: Int, val end: Int)

    private class Styled(val key: Key, val spans: List<Any>)

    /** Zero width and draws nothing: Markdown syntax outside the active block. */
    class HiddenSpan : ReplacementSpan() {
        override fun getSize(paint: Paint, text: CharSequence?, start: Int, end: Int, fm: Paint.FontMetricsInt?) = 0

        override fun draw(
            canvas: Canvas, text: CharSequence?, start: Int, end: Int, x: Float,
            top: Int, y: Int, bottom: Int, paint: Paint,
        ) {}
    }

    /** A little space above a list item's first line. */
    private class SpaceAbove(private val px: Int) : LineHeightSpan {
        override fun chooseHeight(text: CharSequence, start: Int, end: Int, spanstartv: Int, lineHeight: Int, fm: Paint.FontMetricsInt) {
            if ((text as Spanned).getSpanStart(this) in start until end) {
                fm.top -= px
                fm.ascent -= px
            }
        }
    }

    /** Set on the edited text for a moment to have a range laid out again. */
    private class Relayout : MetricAffectingSpan() {
        override fun updateMeasureState(paint: TextPaint) {}

        override fun updateDrawState(paint: TextPaint) {}
    }

    /** Applies each edit to the overlay too, before the layout reacts to it. */
    private val mirror = object : TextWatcher {
        override fun beforeTextChanged(s: CharSequence, start: Int, count: Int, after: Int) {}

        override fun onTextChanged(s: CharSequence, start: Int, before: Int, count: Int) {
            overlay.replace(start, start + before, s.subSequence(start, start + count).toString())
        }

        override fun afterTextChanged(s: Editable) {}
    }

    /** What the text widget lays out and draws: its text, with the overlay's spans as well. */
    private class Display(private val text: Spanned, private val overlay: () -> Spanned) : Spanned, GetChars {
        override val length get() = text.length

        override fun get(index: Int) = text[index]

        override fun subSequence(startIndex: Int, endIndex: Int) = text.subSequence(startIndex, endIndex)

        override fun toString() = text.toString()

        override fun getChars(start: Int, end: Int, dest: CharArray, destoff: Int) =
            TextUtils.getChars(text, start, end, dest, destoff)

        override fun <T : Any?> getSpans(start: Int, end: Int, type: Class<T>): Array<T> {
            val own = text.getSpans(start, end, type)
            val styling = overlay().getSpans(start, end, type)
            if (styling.isEmpty()) return own
            if (own.isEmpty()) return styling
            @Suppress("UNCHECKED_CAST")
            val both = java.lang.reflect.Array.newInstance(type, own.size + styling.size) as Array<T>
            System.arraycopy(own, 0, both, 0, own.size)
            System.arraycopy(styling, 0, both, own.size, styling.size)
            return both
        }

        override fun getSpanStart(tag: Any) = overlay().getSpanStart(tag).let { if (it >= 0) it else text.getSpanStart(tag) }

        override fun getSpanEnd(tag: Any) = overlay().getSpanEnd(tag).let { if (it >= 0) it else text.getSpanEnd(tag) }

        override fun getSpanFlags(tag: Any): Int {
            val styling = overlay()
            return if (styling.getSpanStart(tag) >= 0) styling.getSpanFlags(tag) else text.getSpanFlags(tag)
        }

        override fun nextSpanTransition(start: Int, limit: Int, type: Class<*>?) =
            minOf(text.nextSpanTransition(start, limit, type), overlay().nextSpanTransition(start, limit, type))
    }

    init {
        text.transformationMethod = object : TransformationMethod {
            override fun getTransformation(source: CharSequence, view: View): CharSequence =
                if (source is Spanned) Display(source) { overlay } else source

            override fun onFocusChanged(view: View, sourceText: CharSequence, focused: Boolean, direction: Int, previouslyFocusedRect: Rect?) {}
        }
    }

    /**
     * Prepares the styling for `body`, which the caller then sets as the
     * widget's text, and [attach]es: the layout is built once, with the spans.
     */
    fun style(body: String, cursor: Int) {
        main.removeCallbacks(parse)
        generation += 1
        // SpannableString appends spans, where SpannableStringBuilder keeps
        // them sorted as each comes; the copy sorts them all at once.
        val styled = SpannableString(body)
        styles = emptyList()
        hidden = emptyList()
        shownBlocks = null
        val document = parseMarkdown(body)
        setDocument(document)
        place(styled, cursor, cursor, document.spans(), document.listMarkers())
        overlay = SpannableStringBuilder(styled)
    }

    /** Keeps the overlay in step with the widget's text, which was just set. */
    fun attach() {
        val editable = text.text
        editable.setSpan(mirror, 0, editable.length, Spanned.SPAN_INCLUSIVE_INCLUSIVE or (MIRROR_PRIORITY shl Spanned.SPAN_PRIORITY_SHIFT))
    }

    fun closed() {
        generation += 1
        current = false
        main.removeCallbacks(parse)
        setLink(null)
    }

    /**
     * The text changed at `at`. Parse again after a short pause, and meanwhile
     * show the syntax on the edited line, so it doesn't flicker while typing.
     */
    fun changed(at: Int) {
        generation += 1
        current = false
        val start = if (at == 0) 0 else TextUtils.lastIndexOf(overlay, '\n', at - 1) + 1
        val end = TextUtils.indexOf(overlay, '\n', at).let { if (it < 0) overlay.length else it }
        val revealed = mutableListOf<IntRange>()
        hidden = hidden.filter { span ->
            val from = overlay.getSpanStart(span)
            val to = overlay.getSpanEnd(span)
            val overlaps = from <= end && to >= start
            if (overlaps) {
                overlay.removeSpan(span)
                revealed += from until to
            }
            !overlaps
        }
        relayout(revealed)
        shownBlocks = null
        main.removeCallbacks(parse)
        main.postDelayed(parse, PARSE_DELAY_MS)
    }

    fun selectionChanged() {
        val start = text.selectionStart
        val end = text.selectionEnd
        if (start < 0 || document == null || !current) return
        relayout(updateHidden(overlay, minOf(start, end), maxOf(start, end)))
        updateLink(start)
    }

    private fun request() {
        val generation = generation
        val source = text.text.toString()
        newest = generation
        parser.execute {
            if (generation != newest) return@execute
            val document = parseMarkdown(source)
            val spans = document.spans()
            val markers = document.listMarkers()
            main.post {
                if (generation != this.generation) return@post document.close()
                setDocument(document)
                if (spans.size + markers.size - styles.size > BULK_SPANS) {
                    rebuild(spans, markers)
                } else {
                    relayout(place(overlay, text.selectionStart, text.selectionEnd, spans, markers))
                }
            }
        }
    }

    /**
     * Styles the whole text afresh, as [style] does, for when a parse brings
     * many new spans, such as after a long paste: a SpannableStringBuilder
     * sorts each span as it's added, so hundreds one by one are slow.
     */
    private fun rebuild(spans: List<StyledRange>, markers: List<TextRange>) {
        val styled = SpannableString(text.text.toString())
        styles = emptyList()
        hidden = emptyList()
        place(styled, text.selectionStart, text.selectionEnd, spans, markers)
        overlay = SpannableStringBuilder(styled)
        relayout(listOf(0 until overlay.length))
    }

    private fun setDocument(document: MarkdownDocument) {
        this.document?.close()
        this.document = document
        current = true
    }

    /**
     * Brings the spans in `target` up to the document's. Returns the ranges
     * that changed, for laying out again.
     */
    private fun place(
        target: Spannable,
        selectionStart: Int,
        selectionEnd: Int,
        spans: List<StyledRange>,
        markers: List<TextRange>,
    ): List<IntRange> {
        val start = minOf(selectionStart, selectionEnd).coerceAtLeast(0)
        val end = maxOf(selectionStart, selectionEnd).coerceAtLeast(0)
        val changed = mutableListOf<IntRange>()
        styles = diff(target, styles, spans.map { Key(it.style, it.start, it.end) }, changed)
        // Indents depend on each marker's width in its own styling, so they're
        // measured after the other spans are in place.
        styles = diff(target, styles, markers.map { Key(null, it.start, it.end) }, changed, indents = true)
        shownBlocks = null
        changed += updateHidden(target, start, end)
        updateLink(start)
        return changed
    }

    /**
     * Brings one kind of span (styles, or indents) to `wanted`, keeping the
     * spans that still match. Spans move with the text they cover, so after an
     * edit most do.
     */
    private fun diff(
        target: Spannable,
        applied: List<Styled>,
        wanted: List<Key>,
        changed: MutableList<IntRange>,
        indents: Boolean = false,
    ): List<Styled> {
        val missing = wanted.toMutableSet()
        val kept = ArrayList<Styled>(applied.size)
        for (styled in applied) {
            if ((styled.key.style == null) != indents) {
                kept += styled
                continue
            }
            val first = styled.spans.first()
            val now = styled.key.copy(start = target.getSpanStart(first), end = target.getSpanEnd(first))
            if (missing.remove(now)) {
                kept += Styled(now, styled.spans)
            } else {
                styled.spans.forEach(target::removeSpan)
                if (now.start >= 0) changed += now.start until now.end
            }
        }
        for (key in missing) {
            if (key.start >= key.end || key.end > target.length) continue
            val spans = create(target, key)
            for (span in spans) target.setSpan(span, key.start, key.end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            kept += Styled(key, spans)
            changed += key.start until key.end
        }
        return kept
    }

    private fun create(target: Spannable, key: Key): List<Any> = when (key.style) {
        Style.H1 -> listOf(RelativeSizeSpan(1.875f), StyleSpan(Typeface.BOLD))
        Style.H2 -> listOf(RelativeSizeSpan(1.3f), StyleSpan(Typeface.BOLD))
        Style.H3 -> listOf(RelativeSizeSpan(1.12f), StyleSpan(Typeface.BOLD))
        Style.STRONG -> listOf(StyleSpan(Typeface.BOLD))
        Style.EMPHASIS -> listOf(StyleSpan(Typeface.ITALIC))
        Style.STRIKE -> listOf(StrikethroughSpan())
        Style.QUOTE -> listOf(StyleSpan(Typeface.ITALIC), LeadingMarginSpan.Standard(dp(24)))
        Style.CODE -> listOf(TypefaceSpan(MONOSPACE), RelativeSizeSpan(0.92f))
        Style.CODE_BLOCK -> listOf(TypefaceSpan(MONOSPACE), RelativeSizeSpan(0.92f), LeadingMarginSpan.Standard(dp(18)))
        Style.LINK -> listOf(UnderlineSpan(), ForegroundColorSpan(text.linkTextColors.defaultColor))
        Style.TASK -> listOf(TypefaceSpan(MONOSPACE))
        Style.CHECKED -> listOf(TypefaceSpan(MONOSPACE), StrikethroughSpan())
        Style.LIST_ITEM_START -> listOf(SpaceAbove(dp(3)))
        // A list item's wrapped lines line up with its text, not its bullet.
        null -> listOf(
            LeadingMarginSpan.Standard(0, ceil(Layout.getDesiredWidth(target, key.start, key.end, text.paint)).toInt()),
        )
    }

    /**
     * Hides syntax everywhere except the blocks the selection touches. Only
     * moving into another block changes anything, so moving within one is
     * free. Returns the ranges that changed.
     */
    private fun updateHidden(target: Spannable, start: Int, end: Int): List<IntRange> {
        val document = document ?: return emptyList()
        val blocks = document.activeBlocks(start, end)
        if (blocks == shownBlocks) return emptyList()
        shownBlocks = blocks
        val wanted = document.hiddenOutside(start, end, 0, target.length + 1).toMutableSet()
        val changed = mutableListOf<IntRange>()
        val kept = ArrayList<HiddenSpan>(wanted.size)
        for (span in hidden) {
            val range = TextRange(target.getSpanStart(span), target.getSpanEnd(span))
            if (wanted.remove(range)) {
                kept += span
            } else {
                target.removeSpan(span)
                if (range.start >= 0) changed += range.start until range.end
            }
        }
        for (range in wanted) {
            if (range.end > target.length) continue
            val span = HiddenSpan()
            target.setSpan(span, range.start, range.end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            kept += span
            changed += range.start until range.end
        }
        hidden = kept
        return changed
    }

    /**
     * The layout watches only the edited text, so it learns of overlay
     * changes from a span set there for a moment over each changed stretch.
     * Nearby changes share one, so each paragraph is laid out about once.
     */
    private fun relayout(ranges: List<IntRange>) {
        if (ranges.isEmpty()) return
        val editable = text.text
        val sorted = ranges.sortedBy { it.first }
        var from = sorted.first().first
        var to = sorted.first().last + 1
        fun flush() {
            val start = from.coerceIn(0, editable.length)
            val end = to.coerceIn(start, editable.length)
            if (end <= start) return
            val probe = Relayout()
            editable.setSpan(probe, start, end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            editable.removeSpan(probe)
        }
        for (range in sorted.drop(1)) {
            if (range.first <= to + NEARBY_CHARS) {
                to = maxOf(to, range.last + 1)
            } else {
                flush()
                from = range.first
                to = range.last + 1
            }
        }
        flush()
    }

    private fun updateLink(position: Int) {
        setLink(document?.linkAt(position))
    }

    private fun setLink(url: String?) {
        if (url == link) return
        link = url
        onLink?.invoke(url)
    }

    /** Whether `position` is in a code block, by the newest parse. */
    fun inCodeBlock(position: Int) = document?.inCodeBlock(position) ?: false

    private fun dp(value: Int) = TypedValue.applyDimension(
        TypedValue.COMPLEX_UNIT_DIP, value.toFloat(), text.resources.displayMetrics,
    ).toInt()

    companion object {
        /** As on the desktop: parse once typing pauses this long. */
        const val PARSE_DELAY_MS = 80L

        /**
         * Ahead of the layout's own watchers (100 and 128), so the overlay has
         * each edit before the layout lays it out.
         */
        private const val MIRROR_PRIORITY = 200

        /** Changes closer than this are laid out together. */
        private const val NEARBY_CHARS = 80

        /** More new spans than this, and the overlay is built afresh. */
        private const val BULK_SPANS = 500

        /**
         * Code's font. A phone's font setting can replace every family,
         * monospace included, with one proportional font, in which a backtick
         * may even have no width; AOSP's Droid Sans Mono is still among the
         * system fonts.
         */
        private val MONOSPACE: Typeface by lazy {
            runCatching { Typeface.Builder(File("/system/fonts/DroidSansMono.ttf")).build() }.getOrNull()
                ?: Typeface.MONOSPACE
        }
    }
}
