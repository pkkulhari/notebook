//! Android's UTF-16 adapter for the same source edits used by GTK.
use crate::markdown::TextRange;
use notebook_core::{
    commands::{self, Action},
    formatting::{self, Edit, Format},
    markdown,
    model::Units,
};

#[derive(Clone, Debug, uniffi::Record)]
pub struct FormatCommand {
    pub id: i32,
    pub label: String,
    pub icon: String,
    pub hint: String,
    pub inline: bool,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SlashQuery {
    pub range: TextRange,
    pub text: String,
}

#[derive(Clone, Debug, uniffi::Record)]
pub struct SourceEdit {
    pub range: TextRange,
    pub text: String,
    pub selection: TextRange,
}

/// Reject positions inside a surrogate pair instead of rounding them.
fn character(source: &str, position: i32) -> Option<usize> {
    let position = usize::try_from(position).ok()?;
    let mut units = 0;
    for (index, ch) in source.chars().enumerate() {
        if units == position {
            return Some(index);
        }
        units += ch.len_utf16();
    }
    (units == position).then(|| source.chars().count())
}

fn utf16(source: &str, position: usize) -> i32 {
    source
        .chars()
        .take(position)
        .map(char::len_utf16)
        .sum::<usize>() as i32
}

fn selection(source: &str, start: i32, end: i32) -> Option<std::ops::Range<usize>> {
    Some(character(source, start.min(end))?..character(source, start.max(end))?)
}

fn result(source: &str, edit: Edit) -> SourceEdit {
    let mut after: Vec<_> = source.chars().collect();
    after.splice(edit.range.clone(), edit.text.chars());
    let after: String = after.iter().collect();
    SourceEdit {
        range: TextRange {
            start: utf16(source, edit.range.start),
            end: utf16(source, edit.range.end),
        },
        text: edit.text,
        selection: TextRange {
            start: utf16(&after, edit.selection.start),
            end: utf16(&after, edit.selection.end),
        },
    }
}

#[uniffi::export]
pub fn format_commands(query: String, include_inline: bool) -> Vec<FormatCommand> {
    commands::COMMANDS
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            (include_inline || matches!(c.action, Action::Block(_))) && c.matches(&query)
        })
        .map(|(id, c)| FormatCommand {
            id: id as i32,
            label: c.label.into(),
            icon: c.icon.into(),
            hint: c.hint.into(),
            inline: matches!(c.action, Action::Inline(_)),
        })
        .collect()
}

#[uniffi::export]
pub fn slash_query(source: String, cursor: i32) -> Option<SlashQuery> {
    let at = character(&source, cursor)?;
    let query = commands::query(&source, at)?;
    let doc = markdown::parse(&source, Units::Chars);
    if doc.spans.iter().any(|span| {
        matches!(span.style, "code" | "code-block" | "link")
            && span.range.start <= query.start as i32
            && span.range.end >= at as i32
    }) {
        return None;
    }
    Some(SlashQuery {
        range: TextRange {
            start: utf16(&source, query.start),
            end: cursor,
        },
        text: query.text,
    })
}

#[uniffi::export]
pub fn format_text(
    source: String,
    start: i32,
    end: i32,
    command: i32,
    slash: bool,
) -> Option<SourceEdit> {
    let selected = selection(&source, start, end)?;
    let action = commands::COMMANDS
        .get(usize::try_from(command).ok()?)?
        .action;
    let edit = if slash {
        if start != end {
            return None;
        }
        let query = slash_query(source.clone(), start)?;
        let Action::Block(kind) = action else {
            return None;
        };
        let range = selection(&source, query.range.start, query.range.end)?;
        let deletion = formatting::delete(&source, range)?;
        let block = formatting::block(&deletion.text, deletion.selection.start, kind)?;
        let mut chars: Vec<_> = deletion.text.chars().collect();
        chars.splice(block.range, block.text.chars());
        Edit {
            range: 0..source.chars().count(),
            text: chars.iter().collect(),
            selection: block.selection,
        }
    } else {
        match action {
            Action::Inline(kind) => formatting::toggle(&source, selected, kind)?,
            Action::Block(kind) if selected.is_empty() => {
                formatting::block(&source, selected.start, kind)?
            }
            Action::Block(kind) => formatting::selected_blocks(&source, selected, kind)?,
        }
    };
    Some(result(&source, edit))
}

#[uniffi::export]
pub fn toggle_list(source: String, start: i32, end: i32, ordered: bool) -> Option<SourceEdit> {
    let selected = selection(&source, start, end)?;
    let doc = markdown::parse(&source, Units::Chars);
    if doc.spans.iter().any(|s| {
        s.style == "code-block"
            && s.range.start <= selected.end as i32
            && s.range.end > selected.start as i32
    }) {
        return None;
    }
    let edit = formatting::toggle(
        &source,
        selected,
        if ordered {
            Format::Numbers
        } else {
            Format::Bullets
        },
    )?;
    Some(result(&source, edit))
}

#[uniffi::export]
pub fn visible_text(source: String, start: i32, end: i32) -> String {
    let Some(selected) = selection(&source, start, end) else {
        return String::new();
    };
    let doc = markdown::parse(&source, Units::Chars);
    source
        .chars()
        .enumerate()
        .filter(|(at, _)| {
            selected.contains(at) && !doc.hidden.iter().any(|r| r.contains(&(*at as i32)))
        })
        .map(|(_, ch)| ch)
        .collect()
}

/// Repair a native replacement after the IME has applied it. Common unchanged
/// prefixes/suffixes keep composing-word rewrites from stripping formatting.
#[uniffi::export]
pub fn repair_rich_edit(before: String, after: String) -> Option<SourceEdit> {
    let old: Vec<_> = before.chars().collect();
    let new: Vec<_> = after.chars().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    if prefix == old.len() && prefix == new.len() {
        return None;
    }
    let deletion = formatting::delete(&before, prefix..old.len() - suffix);
    let (base, at) = deletion.map_or((before, prefix), |e| (e.text, e.selection.start));
    let inserted: String = new[prefix..new.len() - suffix].iter().collect();
    let mut edit = formatting::insert(&base, at, &inserted);
    if edit.text == after {
        return None;
    }
    edit.range = 0..new.len();
    Some(result(&after, edit))
}

/// Native Backspace/Delete must cross invisible delimiters in one keystroke.
#[uniffi::export]
pub fn delete_visible(source: String, start: i32, end: i32, backwards: bool) -> Option<SourceEdit> {
    let mut selected = selection(&source, start, end)?;
    if selected.is_empty() {
        let doc = markdown::parse(&source, Units::Chars);
        let hidden = |at: usize| doc.hidden.iter().any(|r| r.contains(&(at as i32)));
        if backwards {
            while selected.start > 0 && hidden(selected.start - 1) {
                selected.start -= 1;
            }
            selected.start = selected.start.checked_sub(1)?;
        } else {
            let len = source.chars().count();
            while selected.end < len && hidden(selected.end) {
                selected.end += 1;
            }
            if selected.end == len {
                return None;
            }
            selected.end += 1;
        }
    }
    Some(result(&source, formatting::delete(&source, selected)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(source: &str, edit: SourceEdit) -> String {
        let mut units: Vec<_> = source.encode_utf16().collect();
        units.splice(
            edit.range.start as usize..edit.range.end as usize,
            edit.text.encode_utf16(),
        );
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn android_commands_preserve_unicode_and_selected_lines() {
        let heading = format_commands("h6".into(), false)[0].id;
        let edit = format_text("🌿 first\nsecond\nlast".into(), 0, 16, heading, false).unwrap();
        assert_eq!(
            apply("🌿 first\nsecond\nlast", edit),
            "###### 🌿 first\n###### second\nlast"
        );
        assert!(format_text("🌿".into(), 1, 2, 0, false).is_none());
        let bold = format_text("🌿 hello".into(), 3, 8, 0, false).unwrap();
        assert_eq!(bold.selection, TextRange { start: 3, end: 12 });
        assert_eq!(apply("🌿 hello", bold), "🌿 **hello**");
    }

    #[test]
    fn slash_commands_ignore_code_links_and_paths() {
        assert!(slash_query("`/todo`".into(), 6).is_none());
        for source in [
            "`/todo`",
            "```\n/todo",
            "    /todo",
            "https://a/b",
            "/usr/bin",
        ] {
            assert!(
                slash_query(source.into(), source.len() as i32).is_none(),
                "{source}"
            );
        }
        let source = "🌿 /todo";
        let command = format_commands("todo".into(), false)[0].id;
        let edit = format_text(source.into(), 8, 8, command, true).unwrap();
        assert_eq!(apply(source, edit), "- [ ] 🌿 ");
    }

    #[test]
    fn edits_and_clipboard_use_visible_characters() {
        assert_eq!(visible_text("🌿 **bold**".into(), 0, 11), "🌿 bold");
        let edit = delete_visible("**🌿**".into(), 6, 6, true).unwrap();
        assert_eq!(apply("**🌿**", edit), "");
        let edit = repair_rich_edit("**hello**".into(), "**hello **".into()).unwrap();
        assert_eq!(apply("**hello **", edit), "**hello** ");
        let edit = delete_visible("- [x] task".into(), 6, 6, true).unwrap();
        assert_eq!(apply("- [x] task", edit), "task");
    }
}
