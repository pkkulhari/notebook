# 9. Markdown editor

**Status:** Not started
**Needs:** step 8

## Goal

The Android editor behaves like the desktop editor:

- Markdown is styled inline, and syntax is hidden outside the block you're editing.
- Lists continue when you press Enter.
- Undo reverts only this device's edits.
- Edits from other devices appear as they arrive, without moving your cursor.
- Copying keeps the Markdown source.

## The `EditText` subclass

`NoteEditText` extends `android.widget.EditText`:

- `inputType = textMultiLine | textCapSentences | textAutoCorrect`, top gravity, no line limit.
- `onSelectionChanged` updates the hidden syntax, as described below.
- `onTextContextMenuItem`:
  - Send `android.R.id.undo` and `redo` to the core. This also covers Ctrl+Z from a hardware keyboard. The framework's own undo stack would otherwise disagree with the core's.
  - Map `android.R.id.paste` to `pasteAsPlainText`, so pasted rich text doesn't bring foreign spans.
- Trashed notes: set `keyListener = null` and keep `setTextIsSelectable(true)`.

## Input bridge

This refines step 8's watcher.

- `onTextChanged(s, start, before, count)`: unless `fromDoc` is set, call `core.replace(id, start, before, s.substring(start, start + count))`. The core trims the common prefix and suffix, so a keyboard rewriting "hel" to "hell" records a single `l`.
- **Applying edits from the core:** set `fromDoc = true`, apply each `TextEdit` in order with `editable.insert` or `editable.delete`, then clear it. Selection and composing spans shift on their own.
  - For undo and redo, first call `BaseInputConnection.removeComposingSpans(editable)`, then place the cursor at `applied.cursor` and scroll it into view.
- If the core returns `OutOfSync`: flush, reload the note, and log it. This means there's a bug to find.

## Styling

### Parsing

- Parse on one background thread with `parse_markdown(text)`, 80 ms after the last change. The desktop uses the same delay (`parse_due` in `src/ui.rs`).
- Coalesce requests: while a parse runs, keep only the newest one.
- Tag each request with a generation number, and apply a result only if the text hasn't changed since.
- Until the new result arrives, remove the hidden spans on the line being edited, as `text_changed` does on the desktop, so syntax doesn't flicker.

### Span mapping

Every span class implements a marker interface, `MdSpan`, so clearing styles removes only our spans and never the IME's or the selection's.

| Core style | GTK tag today | Android span |
| --- | --- | --- |
| `H1` / `H2` / `H3` | scale 1.875 / 1.3 / 1.12, bold | `RelativeSizeSpan` + `StyleSpan(BOLD)` |
| `Strong` | weight 700 | `StyleSpan(BOLD)` |
| `Emphasis` | italic | `StyleSpan(ITALIC)` |
| `Strike` | strikethrough | `StrikethroughSpan` |
| `Quote` | italic, 24 px inset | `StyleSpan(ITALIC)` + `LeadingMarginSpan.Standard(24dp)` |
| `Code` | monospace, scale 0.92 | `TypefaceSpan("monospace")` + `RelativeSizeSpan(0.92f)` |
| `CodeBlock` | monospace, scale 0.92, 18 px inset | as `Code` + `LeadingMarginSpan.Standard(18dp)` |
| `Link` | underline | `UnderlineSpan` + link colour |
| `Task` | monospace | `TypefaceSpan("monospace")` |
| `Checked` | monospace, strikethrough | `TypefaceSpan("monospace")` + `StrikethroughSpan` |
| `ListItemStart` | 3 px above | `LineHeightSpan` adding 3dp above the first line |
| list markers | hanging indent to the marker width | `LeadingMarginSpan.Standard(0, markerWidth)`, with the width from `Paint.measureText` on the marker text |
| hidden syntax | `invisible` | `HiddenSpan`: a `ReplacementSpan` with zero width that draws nothing |

- Diff the new spans against the applied ones, and add or remove only what changed. The desktop does this for hidden ranges (`update_hidden`, `src/ui.rs:1492`).
- Removing and re-adding every span forces a full `DynamicLayout` reflow, which is visible on long notes.

### Hidden syntax

- On each selection change, call `document.hiddenOutside(selStart, selEnd)`, diff against the applied ranges, and add or remove `HiddenSpan`s.
- The block with the cursor is always revealed, so the cursor never sits inside hidden text. Moving into another block reveals that block first.
- A `ReplacementSpan` must not cross a line break. Check that no hidden range contains `\n`, especially for setext headings and fence lines, and split any that do. Add a core test for this.
- Tapping below the last line when it contains hidden text crashed on the desktop (commit `91e0ede`). Test the same case on Android.

## List continuation

- In `afterTextChanged`, if the change was a single inserted `\n` typed by the user and the cursor isn't in a code block (`document.inCodeBlock`), take the line before the newline and call `list_enter(line)`:
  - `Continue { marker, next }`: if the cursor was past `marker`, insert `next` without its leading `\n` at the cursor.
  - `End`: delete the empty item's marker along with the newline just typed.
- Make these edits from a `post {}` rather than inside the watcher callback. They go through the watcher like typing, so they are saved and undone like typing.
- Hardware Enter keys arrive through the same path. Check that one Enter never produces two newlines.

## Links

A tap places the cursor, because that's what editing needs. When the cursor sits inside a link (`document.linkAt`), show an **Open link** action in the top bar.

- Open `http`, `https` and `mailto` targets with `Intent.ACTION_VIEW`. These are the same schemes the desktop allows (`src/ui.rs:730`).
- Ignore any other scheme.

## Word count

Show it in the editor's overflow menu. Count words split on whitespace, the same way the desktop's `split_whitespace` does.

## Performance checks

Use a 50,000-character note: `LINE_DIFF_CHARS` in `src/storage.rs` is where the core switches to line diffs.

- Typing stays smooth: no dropped frames in the GPU profiler while typing steadily.
- Parse plus span diff finishes within one frame after the 80 ms delay.
- Opening the note shows text in under 100 ms, with styling following.

## Tests (instrumented)

1. Type text containing emoji and CJK characters: `core.text` equals the `EditText` text after every keystroke.
2. An edit from another device arrives before the cursor: the cursor keeps its place relative to the text around it.
3. Undo after a remote edit reverts only local typing.
4. Enter on `- [ ] item` continues with `- [ ] `, and Enter on an empty `- ` ends the list.
5. Moving the cursor between blocks shows and hides `**` in each block as it becomes active.
6. Copying styled text gives Markdown source, and pasting HTML gives plain text.
7. Composition with Gboard and one other keyboard: no duplicated or lost characters.

## Notes

_Anything surprising goes here._
