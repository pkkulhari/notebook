//! Pure Markdown presentation: source is never modified, and all UI offsets are characters.
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
}

pub fn parse(source: &str) -> Document {
    let mut doc = Document::default();
    let mut offsets = vec![0i32; source.len() + 1];
    for (count, (byte, ch)) in source.char_indices().enumerate() {
        offsets[byte..byte + ch.len_utf8()].fill(count as i32);
        offsets[byte + ch.len_utf8()] = count as i32 + 1;
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
    let end = source.chars().count() as i32;
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

    #[test]
    fn list_spacing_only_applies_to_item_starts() {
        let source = "- 世界\n  continuation\n  - nested\n- [ ] task\n\n1. ordered\n2. next\n";
        let chars: Vec<char> = source.chars().collect();
        let starts: Vec<String> = parse(source)
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
        let markers: Vec<String> = parse(source)
            .list_markers
            .iter()
            .map(|r| chars[r.start as usize..r.end as usize].iter().collect())
            .collect();
        assert_eq!(markers, ["- ", "  - [ ] ", "10. "]);
    }

    #[test]
    fn unicode_ranges_and_nested_formatting() {
        let source = "# नमस्ते 🌿\n\n**hello _世界_** and `λ`\n";
        let doc = parse(source);
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
        let doc = parse("**one**\n\n*two*");
        assert_eq!(doc.hidden_outside(2..2).len(), 2);
        assert!(doc.hidden_outside(2..12).is_empty());
        assert_eq!(doc.hidden_outside(14..14).len(), 2);
    }

    #[test]
    fn incomplete_syntax_is_preserved() {
        let source = "hello **unfinished [link](\n\n![image](pic.png)";
        let doc = parse(source);
        assert!(doc.hidden.is_empty());
        assert!(doc.links.is_empty());
    }

    #[test]
    fn link_destinations_and_fences() {
        let doc = parse("[site](https://example.org)\n\n```rust\nlet x = 1;\n```\n");
        assert_eq!(doc.links[0].1, "https://example.org");
        assert!(doc.spans.iter().any(|s| s.style == "code-block"));
        assert_eq!(doc.hidden.len(), 4);
    }

    #[test]
    fn single_tilde_and_unicode_never_hide_content() {
        for source in ["~é~", "~~世界~~", "***bold emphasis***", "`🌿`"] {
            let doc = parse(source);
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
