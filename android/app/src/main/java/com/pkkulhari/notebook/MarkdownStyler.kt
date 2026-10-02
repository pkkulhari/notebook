package com.pkkulhari.notebook

import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Typeface
import android.os.Handler
import android.os.Looper
import android.text.Layout
import android.text.Spannable
import android.text.SpannableString
import android.text.Spanned
import android.text.TextUtils
import android.text.style.ForegroundColorSpan
import android.text.style.LeadingMarginSpan
import android.text.style.LineHeightSpan
import android.text.style.RelativeSizeSpan
import android.text.style.ReplacementSpan
import android.text.style.StrikethroughSpan
import android.text.style.StyleSpan
import android.text.style.TypefaceSpan
import android.text.style.UnderlineSpan
import android.util.TypedValue
import com.pkkulhari.notebook.core.MarkdownDocument
import com.pkkulhari.notebook.core.Style
import com.pkkulhari.notebook.core.StyledRange
import com.pkkulhari.notebook.core.TextRange
import com.pkkulhari.notebook.core.parseMarkdown
import java.util.concurrent.Executors
import kotlin.math.ceil

/**
 * Styles the editor's Markdown as the desktop does: formatting spans, hanging
 * indents for list items, and syntax hidden outside the block being edited.
 * The source text never changes.
 *
 * Each span added to text on screen reflows its layout, so a note is styled
 * before it's shown ([style]), and later parses change only the spans that
 * differ.
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

    /** Spans this class added, by what they stand for. The keyboard's own spans are never touched. */
    private var styles = listOf<Styled>()
    private var hidden = listOf<HiddenSpan>()

    /** The blocks whose syntax shows, as of the last hidden-syntax update. */
    private var shownBlocks: List<TextRange>? = null

    /** Runs with the link under the cursor, or null. */
    var onLink: ((String?) -> Unit)? = null

    /**
     * Asks for the text to be set again, styled by [style]: cheaper than adding
     * hundreds of spans to text on screen, such as after a long paste.
     */
    var onRestyle: (() -> Unit)? = null
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

    /**
     * A note's text with its styling, for showing it: parsed and styled while
     * no layout is attached, so a long note costs one layout, not one per span.
     */
    fun style(body: String, cursor: Int): CharSequence {
        main.removeCallbacks(parse)
        generation += 1
        // SpannableString appends spans, where SpannableStringBuilder keeps
        // them sorted as they come; setText then copies them all at once.
        val styled = SpannableString(body)
        styles = emptyList()
        hidden = emptyList()
        shownBlocks = null
        val document = parseMarkdown(body)
        setDocument(document)
        place(styled, cursor, cursor, document.spans(), document.listMarkers())
        return styled
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
        val editable = text.text
        val start = if (at == 0) 0 else TextUtils.lastIndexOf(editable, '\n', at - 1) + 1
        val end = TextUtils.indexOf(editable, '\n', at).let { if (it < 0) editable.length else it }
        hidden = hidden.filter { span ->
            val overlaps = editable.getSpanStart(span) <= end && editable.getSpanEnd(span) >= start
            if (overlaps) editable.removeSpan(span)
            !overlaps
        }
        shownBlocks = null
        main.removeCallbacks(parse)
        main.postDelayed(parse, PARSE_DELAY_MS)
    }

    fun selectionChanged() {
        val start = text.selectionStart
        val end = text.selectionEnd
        if (start < 0 || document == null || !current) return
        updateHidden(text.text, minOf(start, end), maxOf(start, end))
        updateLink(start)
    }

    private fun request() {
        val generation = generation
        val source = text.text.toString()
        newest = generation
        parser.execute {
            if (generation != newest) return@execute
            val document = parseMarkdown(source)
            main.post {
                if (generation != this.generation) return@post document.close()
                setDocument(document)
                val spans = document.spans()
                val markers = document.listMarkers()
                if (spans.size + markers.size - styles.size > BULK_SPANS && onRestyle != null) {
                    onRestyle?.invoke()
                } else {
                    place(text.text, text.selectionStart, text.selectionEnd, spans, markers)
                }
            }
        }
    }

    private fun setDocument(document: MarkdownDocument) {
        this.document?.close()
        this.document = document
        current = true
    }

    /** Brings the spans in `target` up to the current document's. */
    private fun place(target: Spannable, selectionStart: Int, selectionEnd: Int, spans: List<StyledRange>, markers: List<TextRange>) {
        val start = minOf(selectionStart, selectionEnd).coerceAtLeast(0)
        val end = maxOf(selectionStart, selectionEnd).coerceAtLeast(0)
        styles = diff(target, styles, spans.map { Key(it.style, it.start, it.end) })
        // Indents depend on each marker's width in its own styling, so they're
        // measured after the other spans are in place.
        styles = diff(target, styles, markers.map { Key(null, it.start, it.end) }, indents = true)
        shownBlocks = null
        updateHidden(target, start, end)
        updateLink(start)
    }

    /**
     * Brings one kind of span (styles, or indents) to `wanted`, keeping the
     * spans that still match. Spans move with the text they cover, so after an
     * edit most do, and the layout reflows only where something changed.
     */
    private fun diff(target: Spannable, applied: List<Styled>, wanted: List<Key>, indents: Boolean = false): List<Styled> {
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
            }
        }
        for (key in missing) {
            if (key.start >= key.end || key.end > target.length) continue
            val spans = create(target, key)
            for (span in spans) target.setSpan(span, key.start, key.end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            kept += Styled(key, spans)
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
        Style.CODE -> listOf(TypefaceSpan("monospace"), RelativeSizeSpan(0.92f))
        Style.CODE_BLOCK -> listOf(TypefaceSpan("monospace"), RelativeSizeSpan(0.92f), LeadingMarginSpan.Standard(dp(18)))
        Style.LINK -> listOf(UnderlineSpan(), ForegroundColorSpan(text.linkTextColors.defaultColor))
        Style.TASK -> listOf(TypefaceSpan("monospace"))
        Style.CHECKED -> listOf(TypefaceSpan("monospace"), StrikethroughSpan())
        Style.LIST_ITEM_START -> listOf(SpaceAbove(dp(3)))
        // A list item's wrapped lines line up with its text, not its bullet.
        null -> listOf(
            LeadingMarginSpan.Standard(0, ceil(Layout.getDesiredWidth(target, key.start, key.end, text.paint)).toInt()),
        )
    }

    /**
     * Hides syntax everywhere except the blocks the selection touches. Only
     * moving into another block changes anything, so moving within one is free.
     */
    private fun updateHidden(target: Spannable, start: Int, end: Int) {
        val document = document ?: return
        val blocks = document.activeBlocks(start, end)
        if (blocks == shownBlocks) return
        shownBlocks = blocks
        val wanted = document.hiddenOutside(start, end).toMutableSet()
        val kept = ArrayList<HiddenSpan>(wanted.size)
        for (span in hidden) {
            if (wanted.remove(TextRange(target.getSpanStart(span), target.getSpanEnd(span)))) {
                kept += span
            } else {
                target.removeSpan(span)
            }
        }
        for (range in wanted) {
            if (range.end > target.length) continue
            val span = HiddenSpan()
            target.setSpan(span, range.start, range.end, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            kept += span
        }
        hidden = kept
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

        /** More new spans than this are added by setting the text again, styled. */
        private const val BULK_SPANS = 300
    }
}
