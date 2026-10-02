//! Pure Markdown presentation: source is never modified, and all UI offsets
//! are in the caller's `Units`.
use crate::model::Units;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct Span {
    pub range: Range<i32>,
    pub style: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct Document {
    pub spans: Vec<Span>,
    pub hidden: Vec<Range<i32>>,
    pub blocks: Vec<Range<i32>>,
    pub list_markers: Vec<Range<i32>>,
    pub links: Vec<(Range<i32>, String)>,
}

impl Document {
    pub fn visible_blocks(&self, selection: Range<i32>) -> Vec<Range<i32>> {
        self.blocks
            .iter()
            .filter(|b| {
                if selection.start == selection.end {
                    b.start <= selection.start && selection.start < b.end
                } else {
                    b.start < selection.end && selection.start < b.end
                }
            })
            .cloned()
            .collect()
    }

    pub fn hidden_outside(&self, selection: Range<i32>) -> Vec<Range<i32>> {
        let active = self.visible_blocks(selection);
        self.hidden
            .iter()
            .filter(|h| !active.iter().any(|b| h.start < b.end && b.start < h.end))
            .cloned()
            .collect()
    }

    /// The URL of the link at `position`, if it's one the app may open:
    /// http, https or mailto.
    pub fn link_at(&self, position: i32) -> Option<&str> {
        self.links
            .iter()
            .find(|(range, _)| range.contains(&position))
            .map(|(_, url)| url.as_str())
            .filter(|url| {
                ["https://", "http://", "mailto:"]
                    .iter()
                    .any(|scheme| url.starts_with(scheme))
            })
    }
}

pub fn parse(source: &str, units: Units) -> Document {
    let mut doc = Document::default();
    // Each byte's position in `units`, from the start of its character.
    let mut offsets = vec![0i32; source.len() + 1];
    let mut at = 0;
    for (byte, ch) in source.char_indices() {
        offsets[byte..byte + ch.len_utf8()].fill(at);
        at += units.width(ch) as i32;
        offsets[byte + ch.len_utf8()] = at;
    }
    let chars = |r: Range<usize>| offsets[r.start]..offsets[r.end];
    let mut image_depth = 0;
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
        if matches!(event, Event::Start(Tag::Image { .. })) {
            image_depth += 1;
        }
        if image_depth > 0 {
            if matches!(event, Event::End(pulldown_cmark::TagEnd::Image)) {
                image_depth -= 1;
            }
            continue;
        }
        let text = &source[range.clone()];
        let mut style = None;
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                style = Some(match level {
                    pulldown_cmark::HeadingLevel::H1 => "h1",
                    pulldown_cmark::HeadingLevel::H2 => "h2",
                    _ => "h3",
                });
                doc.blocks.push(chars(range.clone()));
                let hashes = text.bytes().take_while(|b| *b == b'#').count();
                if hashes > 0 {
                    let prefix = hashes + text[hashes..].bytes().take_while(|b| *b == b' ').count();
                    doc.hidden.push(chars(range.start..range.start + prefix));
                    let trimmed = text.trim_end();
                    let trailing = trimmed.bytes().rev().take_while(|b| *b == b'#').count();
                    if trailing > 0
                        && trimmed.len() > prefix + trailing
                        && trimmed.as_bytes()[trimmed.len() - trailing - 1].is_ascii_whitespace()
                    {
                        doc.hidden.push(chars(
                            range.start + trimmed.len() - trailing..range.start + trimmed.len(),
                        ));
                    }
                } else if let Some(newline) = text.trim_end().rfind('\n') {
                    doc.hidden.push(chars(range.start + newline + 1..range.end));
                }
            }
            Event::Start(Tag::Paragraph) => {
                doc.blocks.push(chars(range.clone()));
            }
            Event::Start(Tag::Emphasis) => {
                style = Some("emphasis");
                hide_pair(&mut doc, &chars, &range, 1);
            }
            Event::Start(Tag::Strong) => {
                style = Some("strong");
                hide_pair(&mut doc, &chars, &range, 2);
            }
            Event::Start(Tag::Strikethrough) => {
                style = Some("strike");
                let width = text.bytes().take_while(|b| *b == b'~').count().min(2);
                hide_pair(&mut doc, &chars, &range, width);
            }
            Event::Start(Tag::BlockQuote(_)) => {
                style = Some("quote");
            }
            Event::Start(Tag::Item) => {
                // Tight lists have no Paragraph event; each item is an editing block.
                doc.blocks.push(chars(range.clone()));
                // Space item starts without spreading out continuation lines or child blocks.
                let first_line_end = range.start + text.find('\n').unwrap_or(text.len());
                doc.spans.push(Span {
                    range: chars(range.start..first_line_end),
                    style: "list-item-start",
                });
                let line_start = source[..range.start].rfind('\n').map_or(0, |i| i + 1);
                let bullet = text.find(char::is_whitespace).unwrap_or(text.len());
                let content = text.len() - text[bullet..].trim_start_matches([' ', '\t']).len();
                doc.list_markers
                    .push(chars(line_start..range.start + content));
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                style = Some("code-block");
                doc.blocks.push(chars(range.clone()));
                if let CodeBlockKind::Fenced(_) = kind {
                    if let Some(end) = text.find('\n') {
                        doc.hidden.push(chars(range.start..range.start + end));
                    }
                    let trimmed = text.trim_end();
                    if let Some(start) = trimmed.rfind('\n') {
                        let last = trimmed[start + 1..].trim_start();
                        if last.starts_with("```") || last.starts_with("~~~") {
                            doc.hidden
                                .push(chars(range.start + start + 1..range.start + trimmed.len()));
                        }
                    }
                }
            }
            Event::Code(_) => {
                style = Some("code");
                let count = text.bytes().take_while(|b| *b == b'`').count();
                hide_pair(&mut doc, &chars, &range, count);
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                style = Some("link");
                doc.links.push((chars(range.clone()), dest_url.to_string()));
                if text.starts_with('[')
                    && let Some(end) = text.rfind("](").or_else(|| text.rfind("]["))
                {
                    doc.hidden.push(chars(range.start..range.start + 1));
                    doc.hidden.push(chars(range.start + end..range.end));
                }
            }
            Event::TaskListMarker(checked) => {
                style = Some(if checked { "checked" } else { "task" });
                let content = range.end + source[range.end..].len()
                    - source[range.end..].trim_start_matches([' ', '\t']).len();
                if let Some(marker) = doc.list_markers.last_mut() {
                    marker.end = chars(range.start..content).end;
                }
            }
            _ => {}
        }
        if let Some(style) = style {
            doc.spans.push(Span {
                range: chars(range),
                style,
            });
        }
    }
    // Include the end-of-document cursor in the final block.
    let end = offsets[source.len()];
    for block in &mut doc.blocks {
        if block.end == end {
            block.end += 1;
        }
    }
    doc
}

fn hide_pair(
    doc: &mut Document,
    chars: &impl Fn(Range<usize>) -> Range<i32>,
    range: &Range<usize>,
    width: usize,
) {
    if width > 0 && range.len() >= 2 * width {
        doc.hidden.push(chars(range.start..range.start + width));
        doc.hidden.push(chars(range.end - width..range.end));
    }
}

#[derive(Debug, PartialEq)]
pub enum ListEnter {
    Continue { marker: usize, next: String },
    End,
}

/// What Enter does at the end of a list item. `marker` is where the item's
/// content starts. Everything before it (indent, bullet or number, task box) is
/// ASCII, so it's the same count in every `Units`.
pub fn list_enter(line: &str) -> Option<ListEnter> {
    let spaces = |s: &str| s.len() - s.trim_start_matches([' ', '\t']).len();
    let indent = spaces(line);
    let rest = &line[indent..];
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let (bullet, number) = if rest.starts_with(['-', '*', '+']) {
        (rest[..1].to_string(), None)
    } else if (1..=9).contains(&digits) && rest[digits..].starts_with(['.', ')']) {
        let number: u64 = rest[..digits].parse().ok()?;
        (rest[digits..digits + 1].to_string(), Some(number + 1))
    } else {
        return None;
    };
    let bullet_len = digits + 1;
    let gap = spaces(&rest[bullet_len..]);
    let after = &rest[bullet_len + gap..];
    if gap == 0 {
        return None;
    }
    let task = ["[ ]", "[x]", "[X]"].iter().any(|b| after.starts_with(b))
        && (after.len() == 3 || after[3..].starts_with([' ', '\t']));
    let content = if task {
        &after[3 + spaces(&after[3..])..]
    } else {
        after
    };
    if content.trim().is_empty() {
        return Some(ListEnter::End);
    }
    let marker = line.len() - content.len();
    let mut next = format!("\n{}", &line[..indent]);
    if let Some(number) = number {
        next.push_str(&number.to_string());
    }
    next.push_str(&bullet);
    next.push_str(&rest[bullet_len..bullet_len + gap]);
    if task {
        next.push_str("[ ]");
        next.push_str(&after[3..after.len() - content.len()]);
    }
    Some(ListEnter::Continue {
        marker: line[..marker].chars().count(),
        next,
    })
}

/// Whether the last line of `source` is in a code block, where Enter doesn't
/// continue a list. Only what comes before a line decides that, so `source`
/// can end with the line, and a parse of it is never stale.
pub fn ends_in_code_block(source: &str) -> bool {
    let line_start = source.rfind('\n').map_or(0, |i| i + 1);
    let options = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    Parser::new_ext(source, options)
        .into_offset_iter()
        .any(|(event, range)| {
            matches!(event, Event::Start(Tag::CodeBlock(_)))
                && range.start < source.len()
                && range.end > line_start
        })
}

pub fn summary(body: &str) -> (String, String) {
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    let label = lines
        .next()
        .map(plain)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Untitled".into());
    let preview = lines.take(3).map(plain).collect::<Vec<_>>().join(" ");
    (
        label.chars().take(100).collect(),
        preview.chars().take(160).collect(),
    )
}

fn plain(line: &str) -> String {
    let mut result = String::new();
    for event in Parser::new_ext(
        line,
        Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS,
    ) {
        match event {
            Event::Text(text) | Event::Code(text) => result.push_str(&text),
            Event::SoftBreak | Event::HardBreak => result.push(' '),
            _ => {}
        }
    }
    if result.trim().is_empty() {
        line.trim().to_string()
    } else {
        result.trim().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rng;

    /// Checks that every range `parse` finds in UTF-16 units is the matching
    /// character range, converted.
    fn assert_units_agree(source: &str) {
        fn tagged<'a>(
            tag: &'static str,
            ranges: &'a [Range<i32>],
        ) -> impl Iterator<Item = (&'static str, Range<i32>)> + 'a {
            ranges.iter().map(move |r| (tag, r.clone()))
        }
        fn ranges(doc: &Document) -> Vec<(&'static str, Range<i32>)> {
            doc.spans
                .iter()
                .map(|s| (s.style, s.range.clone()))
                .chain(tagged("hidden", &doc.hidden))
                .chain(tagged("block", &doc.blocks))
                .chain(tagged("marker", &doc.list_markers))
                .chain(doc.links.iter().map(|(r, _)| ("link", r.clone())))
                .collect()
        }
        // The UTF-16 position of each character position, and one past the
        // end for the end-of-document cursor.
        let mut utf16 = vec![0];
        for ch in source.chars() {
            utf16.push(utf16.last().unwrap() + ch.len_utf16() as i32);
        }
        utf16.push(utf16.last().unwrap() + 1);
        let expected: Vec<_> = ranges(&parse(source, Units::Chars))
            .into_iter()
            .map(|(tag, r)| (tag, utf16[r.start as usize]..utf16[r.end as usize]))
            .collect();
        assert_eq!(ranges(&parse(source, Units::Utf16)), expected, "{source:?}");
    }

    #[test]
    fn utf16_ranges_cover_the_same_text() {
        let source = "# नमस्ते 🌿\n\n**hello _世界_** and `λ` 🌿🌿 ~~x~~";
        assert_units_agree(source);
        let units: Vec<u16> = source.encode_utf16().collect();
        let doc = parse(source, Units::Utf16);
        let text =
            |r: &Range<i32>| String::from_utf16(&units[r.start as usize..r.end as usize]).unwrap();
        let styled =
            |style: &str| text(&doc.spans.iter().find(|s| s.style == style).unwrap().range);
        assert_eq!(styled("h1").trim_end(), "# नमस्ते 🌿");
        assert_eq!(styled("emphasis"), "_世界_");
        assert_eq!(styled("code"), "`λ`");
        assert_eq!(styled("strike"), "~~x~~");
        // The last block reaches past the end, so a cursor there is inside it.
        assert_eq!(doc.blocks.last().unwrap().end as usize, units.len() + 1);
    }

    #[test]
    fn utf16_ranges_match_character_ranges_for_any_text() {
        let pieces = [
            "a",
            "word ",
            "世界",
            "🌿",
            "e\u{301}",
            "👩‍💻",
            "\n",
            "\n\n",
            "  ",
            "#",
            "# ",
            "## ",
            "**",
            "_",
            "`",
            "~~",
            "> ",
            "- ",
            "1. ",
            "- [ ] ",
            "[🌿](https://example.org)",
            "```\n",
        ];
        for seed in 1..=500u64 {
            let mut rng = Rng(seed);
            let source: String = (0..1 + rng.below(40))
                .map(|_| pieces[rng.below(pieces.len())])
                .collect();
            assert_units_agree(&source);
        }
    }

    #[test]
    fn list_spacing_only_applies_to_item_starts() {
        let source = "- 世界\n  continuation\n  - nested\n- [ ] task\n\n1. ordered\n2. next\n";
        let chars: Vec<char> = source.chars().collect();
        let starts: Vec<String> = parse(source, Units::Chars)
            .spans
            .iter()
            .filter(|span| span.style == "list-item-start")
            .map(|span| {
                chars[span.range.start as usize..span.range.end as usize]
                    .iter()
                    .collect()
            })
            .collect();
        assert_eq!(
            starts,
            ["- 世界", "- nested", "- [ ] task", "1. ordered", "2. next"]
        );
    }

    #[test]
    fn list_markers_cover_indent_bullet_and_task_box() {
        let source = "- one\n  - [ ] nested task\n10. ten\n";
        let chars: Vec<char> = source.chars().collect();
        let markers: Vec<String> = parse(source, Units::Chars)
            .list_markers
            .iter()
            .map(|r| chars[r.start as usize..r.end as usize].iter().collect())
            .collect();
        assert_eq!(markers, ["- ", "  - [ ] ", "10. "]);
    }

    #[test]
    fn enter_continues_or_ends_lists() {
        let next = |line| match list_enter(line) {
            Some(ListEnter::Continue { next, .. }) => Some(next),
            _ => None,
        };
        assert_eq!(next("- item").as_deref(), Some("\n- "));
        assert_eq!(next("  * nested").as_deref(), Some("\n  * "));
        assert_eq!(next("- [x] done").as_deref(), Some("\n- [ ] "));
        assert_eq!(next("9. nine").as_deref(), Some("\n10. "));
        assert_eq!(next("3) three").as_deref(), Some("\n4) "));
        assert_eq!(
            list_enter("- [ ] 世界"),
            Some(ListEnter::Continue {
                marker: 6,
                next: "\n- [ ] ".into()
            })
        );
        // Markers are ASCII, so `marker` is the same in characters and UTF-16.
        assert_eq!(
            list_enter("- [ ] 🌿 x"),
            Some(ListEnter::Continue {
                marker: 6,
                next: "\n- [ ] ".into()
            })
        );
        for line in ["- ", "1. ", "- [ ] ", "  - [x]"] {
            assert_eq!(list_enter(line), Some(ListEnter::End), "{line:?}");
        }
        for line in ["plain", "-", "-dash", "***", "1.5 kg", "1234567890. x"] {
            assert_eq!(list_enter(line), None, "{line:?}");
        }
    }

    #[test]
    fn unicode_ranges_and_nested_formatting() {
        let source = "# नमस्ते 🌿\n\n**hello _世界_** and `λ`\n";
        let doc = parse(source, Units::Chars);
        let chars: Vec<char> = source.chars().collect();
        for span in &doc.spans {
            assert!(span.range.end as usize <= chars.len());
        }
        let emphasis = doc.spans.iter().find(|s| s.style == "emphasis").unwrap();
        assert_eq!(
            chars[emphasis.range.start as usize..emphasis.range.end as usize]
                .iter()
                .collect::<String>(),
            "_世界_"
        );
        assert_eq!(summary(source).0, "नमस्ते 🌿");
    }

    #[test]
    fn active_block_and_selection_reveal_syntax() {
        let doc = parse("**one**\n\n*two*", Units::Chars);
        assert_eq!(doc.hidden_outside(2..2).len(), 2);
        assert!(doc.hidden_outside(2..12).is_empty());
        assert_eq!(doc.hidden_outside(14..14).len(), 2);
    }

    #[test]
    fn incomplete_syntax_is_preserved() {
        let source = "hello **unfinished [link](\n\n![image](pic.png)";
        let doc = parse(source, Units::Chars);
        assert!(doc.hidden.is_empty());
        assert!(doc.links.is_empty());
    }

    #[test]
    fn only_lines_inside_code_blocks_end_in_one() {
        assert!(ends_in_code_block("```\n- code"));
        assert!(ends_in_code_block("Intro\n\n~~~\n- [ ] code"));
        assert!(ends_in_code_block("Intro\n\n    - indented code"));
        assert!(!ends_in_code_block("```\ncode\n```\n- item"));
        assert!(!ends_in_code_block("- item"));
        assert!(!ends_in_code_block("Some `code` and\n- item"));
    }

    #[test]
    fn link_destinations_and_fences() {
        let doc = parse(
            "[site](https://example.org)\n\n```rust\nlet x = 1;\n```\n",
            Units::Chars,
        );
        assert_eq!(doc.links[0].1, "https://example.org");
        assert!(doc.spans.iter().any(|s| s.style == "code-block"));
        assert_eq!(doc.hidden.len(), 4);
    }

    #[test]
    fn single_tilde_and_unicode_never_hide_content() {
        for source in ["~é~", "~~世界~~", "***bold emphasis***", "`🌿`"] {
            let doc = parse(source, Units::Chars);
            let characters: Vec<char> = source.chars().collect();
            for range in doc.hidden {
                assert!(
                    characters[range.start as usize..range.end as usize]
                        .iter()
                        .all(|c| matches!(c, '~' | '*' | '`'))
                );
            }
        }
    }
}
