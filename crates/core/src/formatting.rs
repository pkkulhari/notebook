//! Small source edits for the rich text view. Offsets are Unicode characters,
//! matching GtkTextBuffer; mobile adapters convert UTF-16 at the boundary.
use crate::{markdown, model::Units};
use std::ops::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Bold,
    Italic,
    Strike,
    Bullets,
    Numbers,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Text,
    Heading(u8),
    Bullets,
    Numbers,
    Tasks,
    Quote,
}

/// Convert the current line, preserving its text and the caret's position.
/// Slash commands set a block type; choosing the same type is idempotent.
pub fn block(source: &str, cursor: usize, block: Block) -> Option<Edit> {
    block_numbered(source, cursor, block, 1)
}

fn block_numbered(source: &str, cursor: usize, block: Block, number: usize) -> Option<Edit> {
    let chars: Vec<_> = source.chars().collect();
    if cursor > chars.len() {
        return None;
    }
    let start = chars[..cursor]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |p| p + 1);
    let end = chars[cursor..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |p| cursor + p);
    let line = slice(&chars, start..end);
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 || markdown::ends_in_code_block(&slice(&chars, 0..end)) {
        return None;
    }
    let rest = &line[indent..];
    let hashes = rest.bytes().take_while(|ch| *ch == b'#').count();
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let heading =
        (1..=6).contains(&hashes) && (rest.len() == hashes || rest[hashes..].starts_with(' '));
    let prefix = if heading {
        hashes
    } else if rest.starts_with('>')
        || (rest.starts_with(['-', '*', '+']) && rest[1..].starts_with(' '))
    {
        1
    } else if (1..=9).contains(&digits)
        && rest[digits..].starts_with(['.', ')'])
        && rest[digits + 1..].starts_with(' ')
    {
        digits + 1
    } else {
        0
    };
    let mut content = if prefix > 0 {
        rest[prefix..].trim_start_matches(' ')
    } else {
        rest
    };
    if prefix > 0
        && !heading
        && !rest.starts_with('>')
        && ["[ ] ", "[x] ", "[X] "]
            .iter()
            .any(|marker| content.starts_with(marker))
    {
        content = &content[4..];
    }
    let old_prefix = line[..line.len() - content.len()].chars().count();
    if heading {
        let trimmed = content.trim_end();
        let closing = trimmed.bytes().rev().take_while(|ch| *ch == b'#').count();
        if closing > 0
            && closing < trimmed.len()
            && trimmed.as_bytes()[trimmed.len() - closing - 1] == b' '
        {
            content = trimmed[..trimmed.len() - closing].trim_end();
        }
    }
    let marker = match block {
        Block::Text => String::new(),
        Block::Heading(level) if (1..=6).contains(&level) => {
            format!("{} ", "#".repeat(level as usize))
        }
        Block::Heading(_) => return None,
        Block::Bullets => "- ".into(),
        Block::Numbers => format!("{number}. "),
        Block::Tasks => "- [ ] ".into(),
        Block::Quote => "> ".into(),
    };
    let at = start
        + indent
        + marker.chars().count()
        + cursor
            .saturating_sub(start + old_prefix)
            .min(content.chars().count());
    Some(Edit {
        range: start..end,
        text: format!("{}{marker}{content}", &line[..indent]),
        selection: at..at,
    })
}

/// Set the type of every touched line, keeping the original text selected.
/// A selection ending at the next line's start does not include that line.
pub fn selected_blocks(source: &str, selection: Range<usize>, kind: Block) -> Option<Edit> {
    let chars: Vec<_> = source.chars().collect();
    if selection.is_empty() || selection.end > chars.len() {
        return None;
    }
    let first = block(source, selection.start, kind)?;
    let start = first.range.start;
    let end = chars[selection.end - 1..]
        .iter()
        .position(|ch| *ch == '\n')
        .map_or(chars.len(), |at| selection.end - 1 + at);
    let document = markdown::parse(source, Units::Chars);
    if document.spans.iter().any(|span| {
        span.style == "code-block" && span.range.start < end as i32 && span.range.end > start as i32
    }) {
        return None;
    }
    let mut text = String::new();
    let mut line_start = start;
    let mut number = 1;
    let mapped_end = loop {
        let line_end = chars[line_start..end]
            .iter()
            .position(|ch| *ch == '\n')
            .map_or(end, |at| line_start + at);
        let edit = block_numbered(source, selection.end.min(line_end), kind, number)?;
        let mapped = start + text.chars().count() + edit.selection.end - line_start;
        text.push_str(&edit.text);
        if line_end == end {
            // Include a selected trailing newline without formatting the next line.
            break mapped + selection.end.saturating_sub(end);
        }
        text.push('\n');
        line_start = line_end + 1;
        number += 1;
    };
    Some(Edit {
        range: start..end,
        text,
        selection: first.selection.start..mapped_end,
    })
}

impl Format {
    pub fn style(self) -> Option<&'static str> {
        match self {
            Self::Bold => Some("strong"),
            Self::Italic => Some("emphasis"),
            Self::Strike => Some("strike"),
            _ => None,
        }
    }

    fn marker(self) -> &'static str {
        match self {
            Self::Bold => "**",
            Self::Italic => "*",
            Self::Strike => "~~",
            _ => "",
        }
    }
}

pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
    pub selection: Range<usize>,
}

fn slice(chars: &[char], range: Range<usize>) -> String {
    chars[range].iter().collect()
}

fn content(chars: &[char], span: &markdown::Span) -> Option<Range<usize>> {
    let width = match span.style {
        "strong" => 2,
        "emphasis" => 1,
        "strike" => {
            if chars.get(span.range.start as usize + 1) == Some(&'~') {
                2
            } else {
                1
            }
        }
        _ => return None,
    };
    Some(span.range.start as usize + width..span.range.end as usize - width)
}

fn wrap(text: &str, marker: &str) -> String {
    // Delimiters next to whitespace are literal Markdown, not formatting.
    let middle = text.trim();
    if middle.is_empty() {
        return text.into();
    }
    let first = text.len() - text.trim_start().len();
    let last = text.trim_end().len();
    format!(
        "{}{marker}{middle}{marker}{}",
        &text[..first],
        &text[last..]
    )
}

pub fn toggle(source: &str, mut selection: Range<usize>, format: Format) -> Option<Edit> {
    let chars: Vec<_> = source.chars().collect();
    if selection.end > chars.len() {
        return None;
    }
    if format.style().is_none() {
        return list(&chars, selection, format);
    }
    let doc = markdown::parse(source, Units::Chars);
    let hidden = |at: usize| doc.hidden.iter().any(|r| r.contains(&(at as i32)));
    while selection.start < selection.end
        && (hidden(selection.start) || chars[selection.start].is_whitespace())
    {
        selection.start += 1;
    }
    while selection.end > selection.start
        && (hidden(selection.end - 1) || chars[selection.end - 1].is_whitespace())
    {
        selection.end -= 1;
    }
    if selection.is_empty() {
        return None;
    }
    let spans: Vec<_> = doc
        .spans
        .iter()
        .filter(|span| content(&chars, span).is_some())
        .collect();
    let remove = selection.clone().filter(|at| !hidden(*at)).all(|at| {
        spans.iter().any(|span| {
            Some(span.style) == format.style() && content(&chars, span).unwrap().contains(&at)
        })
    });
    // Split intersecting runs into balanced fragments. This preserves nested
    // italic/bold when a selection starts or ends in the middle of a run.
    let mut region = selection.clone();
    for span in &spans {
        if span.range.start < selection.end as i32 && span.range.end > selection.start as i32 {
            region.start = region.start.min(span.range.start as usize);
            region.end = region.end.max(span.range.end as usize);
        }
    }
    let left = fragment(&chars, &spans, region.start..selection.start, None);
    let middle = fragment(&chars, &spans, selection.clone(), format.style());
    let middle = if remove {
        middle
    } else {
        middle
            .split('\n')
            .map(|line| wrap(line, format.marker()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let right = fragment(&chars, &spans, selection.end..region.end, None);
    let start = region.start + left.chars().count();
    let end = start + middle.chars().count();
    Some(Edit {
        range: region,
        text: left + &middle + &right,
        selection: start..end,
    })
}

fn fragment(
    chars: &[char],
    spans: &[&markdown::Span],
    range: Range<usize>,
    skip: Option<&str>,
) -> String {
    let mut out = String::new();
    let mut at = range.start;
    for (i, span) in spans.iter().enumerate() {
        let start = span.range.start as usize;
        let end = span.range.end as usize;
        if end <= at || start >= range.end {
            continue;
        }
        if start > at {
            out += &slice(chars, at..start);
        }
        let inner = content(chars, span).unwrap();
        let from = inner.start.max(at);
        let to = inner.end.min(range.end);
        if from < to {
            let text = fragment(chars, &spans[i + 1..], from..to, skip);
            if skip == Some(span.style) {
                out += &text;
            } else {
                // Preserve existing delimiters for whole runs. Use asterisks
                // for split emphasis, since underscores cannot flank letters.
                let marker = if from == inner.start && to == inner.end {
                    slice(chars, start..inner.start)
                } else {
                    match span.style {
                        "strong" => "**",
                        "emphasis" => "*",
                        _ => "~~",
                    }
                    .into()
                };
                out += &wrap(&text, &marker);
            }
        }
        at = end.min(range.end);
    }
    if at < range.end {
        out += &slice(chars, at..range.end);
    }
    out
}

fn list(chars: &[char], selection: Range<usize>, format: Format) -> Option<Edit> {
    let start = chars[..selection.start]
        .iter()
        .rposition(|c| *c == '\n')
        .map_or(0, |p| p + 1);
    // A selection ending at the next line's start doesn't include that line.
    let end_at = if selection.end > selection.start && chars.get(selection.end - 1) == Some(&'\n') {
        selection.end - 1
    } else {
        selection.end
    };
    let end = chars[end_at..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(chars.len(), |p| end_at + p);
    let selected = slice(chars, start..end);
    let lines: Vec<_> = selected.split('\n').collect();
    let prefix = |line: &str| -> (usize, usize, bool) {
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let rest = &line[indent..];
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        let number = digits > 0 && rest[digits..].starts_with(['.', ')']);
        let marker = if number {
            digits + 1
        } else if rest.starts_with(['-', '*', '+']) {
            1
        } else {
            0
        };
        if marker > 0 && rest[marker..].starts_with([' ', '\t']) {
            let gap = rest[marker..].len() - rest[marker..].trim_start_matches([' ', '\t']).len();
            (indent, marker + gap, number)
        } else {
            (indent, 0, false)
        }
    };
    let remove = lines.iter().all(|line| {
        let (_, width, number) = prefix(line);
        width > 0 && number == (format == Format::Numbers)
    });
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let (indent, width, _) = prefix(line);
        let marker = if remove {
            String::new()
        } else if format == Format::Numbers {
            format!("{}. ", i + 1)
        } else {
            "- ".into()
        };
        out.push(format!(
            "{}{}{}",
            &line[..indent],
            marker,
            &line[indent + width..]
        ));
    }
    let text = out.join("\n");
    let finish = start + text.chars().count();
    Some(Edit {
        range: start..end,
        text,
        selection: if selection.is_empty() {
            finish..finish
        } else {
            start..finish
        },
    })
}

/// Delete what the user selected without leaving half of a checkbox or an invisible pair.
/// Partly selected runs keep their delimiters; entirely selected ones lose
/// both delimiters, including ones just outside the native selection.
pub fn delete(source: &str, selection: Range<usize>) -> Option<Edit> {
    let chars: Vec<_> = source.chars().collect();
    if selection.is_empty() || selection.end > chars.len() {
        return None;
    }
    let doc = markdown::parse(source, Units::Chars);
    let mut hidden = vec![false; chars.len()];
    for range in &doc.hidden {
        hidden[range.start as usize..range.end as usize].fill(true);
    }
    let mut remove = vec![false; chars.len()];
    for at in selection.clone() {
        remove[at] = !hidden[at];
    }
    // A checkbox replaces the list prefix in the rich text view. Deleting
    // any part of that prefix (including the gap before the task text) must
    // remove the whole prefix, not expose a leftover "- [" or "[x".
    // Item starts exclude nesting indentation and blockquote markers.
    let mut item_start = 0;
    for span in &doc.spans {
        if span.style == "list-item-start" {
            item_start = span.range.start as usize;
        } else if matches!(span.style, "task" | "checked") {
            let mut end = span.range.end as usize;
            while matches!(chars.get(end), Some(' ' | '\t')) {
                end += 1;
            }
            if selection.start < end && item_start < selection.end {
                remove[item_start..end].fill(true);
            }
        }
    }
    for span in &doc.spans {
        let range = span.range.start as usize..span.range.end as usize;
        let visible: Vec<_> = range.clone().filter(|p| !hidden[*p]).collect();
        if !visible.is_empty() && visible.iter().all(|p| remove[*p]) {
            for at in range {
                if hidden[at] {
                    remove[at] = true;
                }
            }
        }
    }
    remove.iter().position(|v| *v)?;
    let mut positions = vec![0; chars.len() + 1];
    let mut result = Vec::new();
    for (at, ch) in chars.iter().enumerate() {
        positions[at] = result.len();
        if !remove[at] {
            result.push(*ch);
        }
    }
    positions[chars.len()] = result.len();
    let mut cursor = positions[selection.start];
    // Removing a word can leave whitespace immediately inside emphasis.
    // Move that whitespace outside the pair so Markdown still renders it.
    // The length doesn't change, so the original position map stays valid.
    for span in doc.spans.iter().rev() {
        let Some(inner) = content(&chars, span) else {
            continue;
        };
        if !remove[inner.clone()].iter().any(|v| *v) {
            continue;
        }
        let start = positions[span.range.start as usize];
        let end = positions[span.range.end as usize];
        let width = inner.start - span.range.start as usize;
        if end <= start + 2 * width {
            continue;
        }
        let middle = slice(&result, start + width..end - width);
        let leading = middle.chars().take_while(|c| c.is_whitespace()).count();
        let trailing = middle
            .chars()
            .rev()
            .take_while(|c| c.is_whitespace())
            .count();
        if leading + trailing == 0 {
            continue;
        }
        let marker = slice(&chars, span.range.start as usize..inner.start);
        let wrapped = wrap(&middle, &marker);
        if wrapped.chars().count() != end - start {
            continue;
        }
        if leading > 0 && (start + width..=start + width + leading).contains(&cursor) {
            cursor -= width;
        } else if trailing > 0 && (end - width - trailing..=end - width).contains(&cursor) {
            cursor += width;
        }
        result.splice(start..end, wrapped.chars());
    }
    Some(Edit {
        range: 0..chars.len(),
        text: result.iter().collect(),
        selection: cursor..cursor,
    })
}

/// Spaces/newlines typed at the edge of an inline run belong outside its
/// delimiters. Otherwise a perfectly ordinary keystroke exposes raw syntax.
pub fn insert(source: &str, at: usize, text: &str) -> Edit {
    let chars: Vec<_> = source.chars().collect();
    let mut result = chars.clone();
    result.splice(at..at, text.chars());
    let count = text.chars().count();
    let mut cursor = at + count;
    let doc = markdown::parse(source, Units::Chars);
    for span in doc.spans.iter().rev() {
        let Some(inner) = content(&chars, span) else {
            continue;
        };
        if at < inner.start || at > inner.end {
            continue;
        }
        let width = inner.start - span.range.start as usize;
        let start = span.range.start as usize;
        let end = span.range.end as usize + count;
        let middle = slice(&result, start + width..end - width);
        let leading = middle.chars().take_while(|c| c.is_whitespace()).count();
        let trailing = middle
            .chars()
            .rev()
            .take_while(|c| c.is_whitespace())
            .count();
        if leading + trailing == 0 {
            continue;
        }
        let marker = slice(&chars, start..inner.start);
        let wrapped = wrap(&middle, &marker);
        if wrapped.chars().count() != end - start {
            continue;
        }
        if leading > 0 && (start + width..=start + width + leading).contains(&cursor) {
            cursor -= width;
        } else if trailing > 0 && (end - width - trailing..=end - width).contains(&cursor) {
            cursor += width;
        }
        result.splice(start..end, wrapped.chars());
    }
    Edit {
        range: 0..chars.len(),
        text: result.iter().collect(),
        selection: cursor..cursor,
    }
}

/// A URL pasted on its own: http(s), mailto, or a `www.` address, which
/// gets https. Anything with spaces in it is prose, not a URL.
pub fn pasted_url(text: &str) -> Option<String> {
    let text = text.trim();
    if text
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '<' || c == '>')
    {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    let rest = ["https://", "http://", "mailto:"]
        .iter()
        .find_map(|scheme| lower.strip_prefix(scheme));
    match rest {
        Some("") => None,
        Some(_) => Some(text.into()),
        None if lower.starts_with("www.") && text[4..].contains('.') && !text.ends_with('.') => {
            Some(format!("https://{text}"))
        }
        None => None,
    }
}

/// What someone typed into a link field. A bare address gets https, and an
/// email address gets mailto. Characters a Markdown link can't hold are
/// percent-encoded.
pub fn typed_url(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let scheme = text.split_once(':').is_some_and(|(scheme, rest)| {
        let mut letters = scheme.chars();
        letters.next().is_some_and(|c| c.is_ascii_alphabetic())
            && letters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
            && (rest.starts_with("//")
                || ["mailto", "tel", "sms"].contains(&scheme.to_ascii_lowercase().as_str()))
    });
    let url = if scheme {
        text.to_string()
    } else if text.contains('@') && !text.contains('/') {
        format!("mailto:{text}")
    } else {
        format!("https://{text}")
    };
    let mut encoded = String::with_capacity(url.len());
    for ch in url.chars() {
        match ch {
            ' ' => encoded.push_str("%20"),
            '<' => encoded.push_str("%3C"),
            '>' => encoded.push_str("%3E"),
            c if c.is_whitespace() || c.is_control() => {}
            c => encoded.push(c),
        }
    }
    Some(encoded)
}

/// Markdown for a link showing `label`. A link that shows its own URL is an
/// autolink, which keeps the source readable. Whitespace around the label
/// stays outside the brackets, so a double-clicked word links cleanly.
fn link_markdown(label: &str, url: &str) -> Option<String> {
    let middle = label.trim();
    if middle.contains('\n') {
        return None;
    }
    let mut depth = 0i32;
    let mut escaped = false;
    for ch in middle.chars() {
        match ch {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return None;
        }
    }
    if depth != 0 {
        return None;
    }
    let first = label.len() - label.trim_start().len();
    let last = label.trim_end().len();
    let link = if middle.is_empty() || middle == url {
        format!("<{url}>")
    } else if url.contains(['(', ')']) {
        format!("[{middle}](<{url}>)")
    } else {
        format!("[{middle}]({url})")
    };
    Some(format!("{}{link}{}", &label[..first], &label[last..]))
}

/// Link the selected text to `url`, or insert `url` as a link at a caret.
/// The caret ends up after the link.
pub fn link(source: &str, selection: Range<usize>, url: &str) -> Option<Edit> {
    let chars: Vec<_> = source.chars().collect();
    if selection.end > chars.len() {
        return None;
    }
    let label = slice(&chars, selection.clone());
    let text = link_markdown(&label, url)?;
    let end = selection.start + text.chars().count();
    Some(Edit {
        range: selection,
        text,
        selection: end..end,
    })
}

/// Pasting a URL makes it a link, and pasting one over selected text links
/// that text. `None` means paste plain text: inside code or an existing
/// link, over another URL, or while someone writes link syntax by hand.
pub fn paste_link(source: &str, selection: Range<usize>, pasted: &str) -> Option<Edit> {
    let url = pasted_url(pasted)?;
    let chars: Vec<_> = source.chars().collect();
    if selection.end > chars.len() {
        return None;
    }
    let label = slice(&chars, selection.clone());
    if pasted_url(&label).is_some() {
        return None;
    }
    let at = selection.start;
    let before = |n: usize| at.checked_sub(n).and_then(|i| chars.get(i)).copied();
    if matches!(before(1), Some('<' | '[' | '`' | '\\'))
        || (before(1) == Some('(') && before(2) == Some(']'))
    {
        return None;
    }
    let touches = |range: &Range<i32>| {
        let (start, end) = (range.start as usize, range.end as usize);
        if selection.is_empty() {
            start < at && at < end
        } else {
            start < selection.end && selection.start < end
        }
    };
    let doc = markdown::parse(source, Units::Chars);
    // A character typed here is code if the whole line is. This also covers
    // an empty line at the end of an unclosed fence.
    let line = format!("{}x", slice(&chars, 0..at));
    if markdown::ends_in_code_block(&line)
        || doc.links.iter().any(|(range, _)| touches(range))
        || doc
            .spans
            .iter()
            .any(|span| matches!(span.style, "code" | "code-block") && touches(&span.range))
    {
        return None;
    }
    let label = if label.is_empty() {
        pasted.trim().to_string()
    } else {
        label
    };
    let text = link_markdown(&label, &url)?;
    let end = at + text.chars().count();
    Some(Edit {
        range: selection,
        text,
        selection: end..end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(source: &str, edit: Edit) -> String {
        let mut chars: Vec<_> = source.chars().collect();
        chars.splice(edit.range, edit.text.chars());
        chars.iter().collect()
    }

    #[test]
    fn block_commands_convert_headings_and_preserve_caret() {
        for level in 1..=6 {
            let edit = block("🌿 Title", 3, Block::Heading(level)).unwrap();
            assert_eq!(edit.selection.start, 3 + level as usize + 1);
            let source = apply("🌿 Title", edit);
            assert_eq!(source, format!("{} 🌿 Title", "#".repeat(level as usize)));
            let doc = markdown::parse(&source, Units::Chars);
            assert!(
                doc.spans
                    .iter()
                    .any(|span| span.style == format!("h{level}"))
            );
            assert_eq!(
                apply(
                    &source,
                    block(&source, source.chars().count(), Block::Text).unwrap()
                ),
                "🌿 Title"
            );
            assert_eq!(
                apply(
                    &source,
                    block(&source, source.chars().count(), Block::Heading(level)).unwrap()
                ),
                source
            );
        }
        assert_eq!(
            apply("## Title ##", block("## Title ##", 8, Block::Text).unwrap()),
            "Title"
        );
        assert_eq!(block("", 0, Block::Heading(2)).unwrap().text, "## ");
    }

    #[test]
    fn block_commands_replace_existing_markers() {
        for (source, kind, expected) in [
            ("- [x] Done", Block::Heading(3), "### Done"),
            ("### Title", Block::Tasks, "- [ ] Title"),
            ("1. First", Block::Bullets, "- First"),
            ("> Quote", Block::Text, "Quote"),
            ("Text", Block::Quote, "> Text"),
        ] {
            assert_eq!(
                apply(source, block(source, source.chars().count(), kind).unwrap()),
                expected
            );
        }
        assert!(block("```\ncode", 8, Block::Heading(1)).is_none());
        assert!(block("    code", 8, Block::Heading(1)).is_none());
        assert!(block("text", 4, Block::Heading(7)).is_none());
    }

    #[test]
    fn selected_blocks_preserve_unicode_selection_and_exclude_next_line() {
        let source = "🌿 one\ntwo\nthree";
        let edit = selected_blocks(source, 2..10, Block::Heading(2)).unwrap();
        let selection = edit.selection.clone();
        let edited = apply(source, edit);
        assert_eq!(edited, "## 🌿 one\n## two\nthree");
        assert_eq!(selection, 5..16);
        assert_eq!(
            apply(
                &edited,
                selected_blocks(&edited, selection, Block::Text).unwrap()
            ),
            source
        );
        let edit = selected_blocks(source, 2..7, Block::Numbers).unwrap();
        assert_eq!(edit.selection, 5..13);
        assert_eq!(apply(source, edit), "1. 🌿 one\n2. two\nthree");
        let source = "# First\n- Second\n> Third";
        assert_eq!(
            apply(
                source,
                selected_blocks(source, 2..source.chars().count(), Block::Tasks).unwrap()
            ),
            "- [ ] First\n- [ ] Second\n- [ ] Third"
        );
    }

    #[test]
    fn selected_blocks_reject_code_without_partial_changes() {
        for source in ["text\n```\ncode\n```", "    code", "```rust\nx\n```\ntext"] {
            assert!(
                selected_blocks(source, 0..source.chars().count(), Block::Heading(1)).is_none()
            );
        }
        assert!(selected_blocks("text", 1..1, Block::Heading(2)).is_none());
        assert!(selected_blocks("text", 0..5, Block::Heading(2)).is_none());
    }

    #[test]
    fn selection_formats_unicode_and_toggles_off() {
        let bold = toggle("a 🌿世界 z", 2..5, Format::Bold).unwrap();
        let selected = bold.selection.clone();
        let source = apply("a 🌿世界 z", bold);
        assert_eq!(source, "a **🌿世界** z");
        assert_eq!(
            apply(&source, toggle(&source, selected, Format::Bold).unwrap()),
            "a 🌿世界 z"
        );
    }

    #[test]
    fn partial_toggle_preserves_surrounding_text_and_other_styles() {
        assert_eq!(
            apply(
                "**one two three**",
                toggle("**one two three**", 6..9, Format::Bold).unwrap()
            ),
            "**one** two **three**"
        );
        assert_eq!(
            apply("_hello_", toggle("_hello_", 1..6, Format::Bold).unwrap()),
            "**_hello_**"
        );
        assert_eq!(
            apply(
                "one\n\ntwo",
                toggle("one\n\ntwo", 0..8, Format::Italic).unwrap()
            ),
            "*one*\n\n*two*"
        );
    }

    #[test]
    fn mixed_selection_merges_runs_without_exposing_markers() {
        let source = "**one** two **three**";
        assert_eq!(
            apply(
                source,
                toggle(source, 0..source.chars().count(), Format::Bold).unwrap()
            ),
            "**one two three**"
        );
        let source = "**_hello_**";
        assert_eq!(
            apply(
                source,
                toggle(source, 0..source.chars().count(), Format::Bold).unwrap()
            ),
            "_hello_"
        );
    }

    #[test]
    fn partial_nested_formatting_preserves_other_styles() {
        let source = "**one _two three_**";
        let edited = apply(source, toggle(source, 7..10, Format::Bold).unwrap());
        let doc = markdown::parse(&edited, Units::Chars);
        let visible: String = edited
            .chars()
            .enumerate()
            .filter(|(i, _)| !doc.hidden.iter().any(|r| r.contains(&(*i as i32))))
            .map(|(_, ch)| ch)
            .collect();
        assert_eq!(visible, "one two three");
        let two = edited.find("two").unwrap() as i32;
        assert!(
            doc.spans
                .iter()
                .any(|s| s.style == "emphasis" && s.range.contains(&two))
        );
        assert!(
            !doc.spans
                .iter()
                .any(|s| s.style == "strong" && s.range.contains(&two))
        );

        let source = "_left right_ more";
        let edited = apply(source, toggle(source, 6..17, Format::Bold).unwrap());
        let doc = markdown::parse(&edited, Units::Chars);
        let visible: String = edited
            .chars()
            .enumerate()
            .filter(|(i, _)| !doc.hidden.iter().any(|r| r.contains(&(*i as i32))))
            .map(|(_, ch)| ch)
            .collect();
        assert_eq!(visible, "left right more");
    }

    #[test]
    fn list_toggle_converts_and_excludes_unselected_line() {
        assert_eq!(
            apply(
                "one\ntwo\nthree",
                toggle("one\ntwo\nthree", 0..8, Format::Bullets).unwrap()
            ),
            "- one\n- two\nthree"
        );
        assert_eq!(
            apply(
                "- one\n- two",
                toggle("- one\n- two", 0..11, Format::Numbers).unwrap()
            ),
            "1. one\n2. two"
        );
        assert_eq!(
            apply(
                "1. one\n2. two",
                toggle("1. one\n2. two", 0..13, Format::Numbers).unwrap()
            ),
            "one\ntwo"
        );
        assert_eq!(apply("", toggle("", 0..0, Format::Bullets).unwrap()), "- ");
    }

    #[test]
    fn deletion_keeps_partial_styles_and_removes_empty_pairs() {
        assert_eq!(apply("**abc**", delete("**abc**", 3..7).unwrap()), "**a**");
        assert_eq!(apply("**abc**", delete("**abc**", 2..5).unwrap()), "");
        assert_eq!(
            apply("a **bc** d", delete("a **bc** d", 0..5).unwrap()),
            "**c** d"
        );
        assert_eq!(apply("**a _🌿_**", delete("**a _🌿_**", 2..9).unwrap()), "");
        assert_eq!(
            apply(
                "[site](https://example.org)",
                delete("[site](https://example.org)", 1..5).unwrap()
            ),
            ""
        );
        assert_eq!(
            apply(
                "**Hello 🌿** world",
                delete("**Hello 🌿** world", 0..7).unwrap()
            ),
            " **🌿** world"
        );
    }

    #[test]
    fn deleting_any_part_of_a_checkbox_removes_the_complete_prefix() {
        for marker in ["- [ ] ", "* [x] ", "+ [X] ", "12. [ ] ", "3) [x]\t"] {
            for at in 0..marker.chars().count() {
                let source = format!("{marker}Keep **🌿 text**");
                let edit = delete(&source, at..at + 1).unwrap();
                assert_eq!(edit.selection, 0..0);
                assert_eq!(
                    apply(&source, edit),
                    "Keep **🌿 text**",
                    "{marker:?} at {at}"
                );
            }
        }
        for source in ["- [ ] ", "- [x] ", "- [X]\t"] {
            let end = source.chars().count();
            assert_eq!(apply(source, delete(source, end - 1..end).unwrap()), "");
        }
        let source = "- [x] 🌿 Done\n- [ ] ";
        let end = source.chars().count();
        let edit = delete(source, end - 1..end).unwrap();
        assert_eq!(edit.selection, end - 6..end - 6);
        assert_eq!(apply(source, edit), "- [x] 🌿 Done\n");
    }

    #[test]
    fn checkbox_deletion_preserves_context_and_normal_text_editing() {
        for (source, selection, expected) in [
            ("- Parent\n  - [ ] Child", 15..16, "- Parent\n  Child"),
            ("> - [x] Quoted", 7..8, "> Quoted"),
            ("- [ ] Text", 6..7, "- [ ] ext"),
            ("- [x] 🌿", 6..7, "- [x] "),
            ("- [ ] Text", 4..8, "xt"),
            ("[ ] Text", 2..3, "[  Text"),
            ("`- [ ] example`", 5..6, "`- [  example`"),
            ("```\n- [ ] example\n```", 8..9, "```\n- [  example\n```"),
        ] {
            assert_eq!(
                apply(source, delete(source, selection).unwrap()),
                expected,
                "{source:?}"
            );
        }
        assert!(delete("- [ ] **Text**", 6..8).is_none());
    }

    #[test]
    fn typing_space_and_enter_keeps_inline_formatting_rendered() {
        for space in [" ", "\n", "  "] {
            let edit = insert("**hello**", 7, space);
            assert_eq!(edit.text, format!("**hello**{space}"));
            assert_eq!(edit.selection.start, 9 + space.len());
        }
        assert_eq!(insert("**hello**", 2, " ").text, " **hello**");
        assert_eq!(insert("**hello**", 4, " 🌿 ").text, "**he 🌿 llo**");
    }

    #[test]
    fn pasted_urls_become_links() {
        for (pasted, expected) in [
            ("https://example.org", "<https://example.org>"),
            (
                " https://example.org/a?b=1#c\n",
                "<https://example.org/a?b=1#c>",
            ),
            ("mailto:me@example.org", "<mailto:me@example.org>"),
            (
                "www.example.org",
                "[www.example.org](https://www.example.org)",
            ),
            (
                "https://en.wikipedia.org/wiki/Rust_(language)",
                "<https://en.wikipedia.org/wiki/Rust_(language)>",
            ),
        ] {
            let edit = paste_link("See ", 4..4, pasted).unwrap();
            let source = apply("See ", edit);
            assert_eq!(source, format!("See {expected}"), "{pasted:?}");
            let doc = markdown::parse(&source, Units::Chars);
            assert_eq!(doc.link_at(5), pasted_url(pasted).as_deref(), "{pasted:?}");
        }
        for pasted in [
            "example.org",
            "https://",
            "see https://example.org",
            "two\nlines",
            "ftp://example.org",
            "www.",
        ] {
            assert!(paste_link("", 0..0, pasted).is_none(), "{pasted:?}");
        }
    }

    #[test]
    fn pasting_a_url_over_a_selection_links_the_text() {
        let edit = paste_link("Read this 🌿 now", 5..12, "https://example.org").unwrap();
        assert_eq!(edit.selection.start, 35);
        let source = apply("Read this 🌿 now", edit);
        assert_eq!(source, "Read [this 🌿](https://example.org) now");
        assert_eq!(
            apply(
                "a word b",
                paste_link("a word b", 1..7, "https://x.org/(y)").unwrap()
            ),
            "a [word](<https://x.org/(y)>) b"
        );
        // Replacing one URL with another stays a plain paste.
        assert!(paste_link("https://old.org", 0..15, "https://new.org").is_none());
        assert!(paste_link("one\ntwo", 0..7, "https://example.org").is_none());
        assert!(paste_link("a ] b", 0..5, "https://example.org").is_none());
    }

    #[test]
    fn pasting_a_url_into_syntax_or_code_stays_plain() {
        for (source, at) in [
            ("[text](", 7),
            ("<", 1),
            ("[", 1),
            ("`", 1),
            ("`code here`", 5),
            ("```\ncode\n", 9),
            ("    indented", 12),
            ("[link](https://a.org) and", 3),
            ("<https://a.org>", 5),
        ] {
            assert!(
                paste_link(source, at..at, "https://example.org").is_none(),
                "{source:?}"
            );
        }
        assert!(paste_link("`code` ", 7..7, "https://example.org").is_some());
        assert!(paste_link("```\ncode\n```\n", 13..13, "https://example.org").is_some());
    }

    #[test]
    fn typed_urls_are_normalized() {
        for (typed, expected) in [
            ("example.org", "https://example.org"),
            (" https://example.org/a b ", "https://example.org/a%20b"),
            ("me@example.org", "mailto:me@example.org"),
            ("mailto:me@example.org", "mailto:me@example.org"),
            ("localhost:8080/x", "https://localhost:8080/x"),
            ("ftp://files.org", "ftp://files.org"),
            ("a.org/<x>", "https://a.org/%3Cx%3E"),
        ] {
            assert_eq!(typed_url(typed).as_deref(), Some(expected), "{typed:?}");
        }
        assert!(typed_url("  ").is_none());
        let edit = link("x word y", 2..7, "https://a.org").unwrap();
        assert_eq!(apply("x word y", edit), "x [word](https://a.org) y");
        let edit = link("ab", 1..1, "https://a.org").unwrap();
        assert_eq!(edit.selection, 16..16);
        assert_eq!(apply("ab", edit), "a<https://a.org>b");
    }
}
