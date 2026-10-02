//! Markdown parsed for an `EditText`: every range is in UTF-16 units.
use notebook_core::{markdown, model::Units};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Style {
    H1,
    H2,
    H3,
    Strong,
    Emphasis,
    Strike,
    Quote,
    Code,
    CodeBlock,
    Link,
    Task,
    Checked,
    ListItemStart,
}

impl Style {
    fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "h1" => Style::H1,
            "h2" => Style::H2,
            "h3" => Style::H3,
            "strong" => Style::Strong,
            "emphasis" => Style::Emphasis,
            "strike" => Style::Strike,
            "quote" => Style::Quote,
            "code" => Style::Code,
            "code-block" => Style::CodeBlock,
            "link" => Style::Link,
            "task" => Style::Task,
            "checked" => Style::Checked,
            "list-item-start" => Style::ListItemStart,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct TextRange {
    pub start: i32,
    pub end: i32,
}

impl From<&std::ops::Range<i32>> for TextRange {
    fn from(range: &std::ops::Range<i32>) -> Self {
        Self {
            start: range.start,
            end: range.end,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StyledRange {
    pub start: i32,
    pub end: i32,
    pub style: Style,
}

#[derive(uniffi::Object)]
pub struct MarkdownDocument {
    document: markdown::Document,
    /// Where each `\n` is, in UTF-16 units.
    line_breaks: Vec<i32>,
}

/// Parses a note's text. Call it off the main thread for long notes.
#[uniffi::export]
pub fn parse_markdown(text: String) -> Arc<MarkdownDocument> {
    Arc::new(MarkdownDocument {
        document: markdown::parse(&text, Units::Utf16),
        line_breaks: text
            .encode_utf16()
            .enumerate()
            .filter(|(_, unit)| *unit == u16::from(b'\n'))
            .map(|(at, _)| at as i32)
            .collect(),
    })
}

#[uniffi::export]
impl MarkdownDocument {
    pub fn spans(&self) -> Vec<StyledRange> {
        self.document
            .spans
            .iter()
            .filter_map(|span| {
                Some(StyledRange {
                    start: span.range.start,
                    end: span.range.end,
                    style: Style::from_name(span.style)?,
                })
            })
            .collect()
    }

    /// Each list item's indent, bullet and task box, for hanging indents.
    pub fn list_markers(&self) -> Vec<TextRange> {
        self.document.list_markers.iter().map(Into::into).collect()
    }

    /// The blocks the selection from `start` to `end` touches, whose syntax
    /// shows. While they stay the same, the hidden syntax does too.
    pub fn active_blocks(&self, start: i32, end: i32) -> Vec<TextRange> {
        self.document
            .visible_blocks(start..end)
            .iter()
            .map(Into::into)
            .collect()
    }

    /// The Markdown syntax to hide, leaving visible the blocks the selection
    /// from `start` to `end` touches. No range contains a line break: an
    /// Android `ReplacementSpan` can't cross one, so a setext heading's
    /// underline is hidden but its line stays.
    pub fn hidden_outside(&self, start: i32, end: i32) -> Vec<TextRange> {
        let mut ranges = vec![];
        for hidden in self.document.hidden_outside(start..end) {
            let first = self.line_breaks.partition_point(|&at| at < hidden.start);
            let mut from = hidden.start;
            for &at in self.line_breaks[first..]
                .iter()
                .take_while(|&&at| at < hidden.end)
            {
                if at > from {
                    ranges.push(TextRange {
                        start: from,
                        end: at,
                    });
                }
                from = at + 1;
            }
            if hidden.end > from {
                ranges.push(TextRange {
                    start: from,
                    end: hidden.end,
                });
            }
        }
        ranges
    }

    /// The link at `position`, if it's one the app may open: http, https or
    /// mailto.
    pub fn link_at(&self, position: i32) -> Option<String> {
        self.document.link_at(position).map(Into::into)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ListEnter {
    /// Insert `next` at the cursor. Only when the cursor is at or after
    /// `marker`, where the item's text starts.
    Continue { marker: i32, next: String },
    /// The item is empty: remove the line's marker instead.
    End,
}

/// What Enter does at the end of `line`, if it's a list item.
#[uniffi::export]
pub fn list_enter(line: String) -> Option<ListEnter> {
    markdown::list_enter(&line).map(|enter| match enter {
        markdown::ListEnter::Continue { marker, next } => ListEnter::Continue {
            marker: marker as i32,
            next,
        },
        markdown::ListEnter::End => ListEnter::End,
    })
}
