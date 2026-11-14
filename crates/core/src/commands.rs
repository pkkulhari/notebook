//! Commands and slash queries shared by both editors.
use crate::formatting::{Block, Format};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Block(Block),
    Inline(Format),
    /// Ask for a URL, then link the selection or insert a link.
    Link,
}

pub struct Command {
    pub action: Action,
    pub label: &'static str,
    pub icon: &'static str,
    pub hint: &'static str,
    pub search: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        action: Action::Inline(Format::Bold),
        label: "Bold",
        icon: "B",
        hint: "Ctrl+B",
        search: "bold strong",
    },
    Command {
        action: Action::Inline(Format::Italic),
        label: "Italic",
        icon: "𝘐",
        hint: "Ctrl+I",
        search: "italic emphasis",
    },
    Command {
        action: Action::Inline(Format::Strike),
        label: "Strikethrough",
        icon: "S̶",
        hint: "Ctrl+Shift+X",
        search: "strike strikethrough",
    },
    Command {
        action: Action::Block(Block::Text),
        label: "Text",
        icon: "T",
        hint: "",
        search: "text paragraph plain normal",
    },
    Command {
        action: Action::Block(Block::Heading(1)),
        label: "Heading 1",
        icon: "H₁",
        hint: "#",
        search: "h1 heading1 title",
    },
    Command {
        action: Action::Block(Block::Heading(2)),
        label: "Heading 2",
        icon: "H₂",
        hint: "##",
        search: "h2 heading2 subtitle",
    },
    Command {
        action: Action::Block(Block::Heading(3)),
        label: "Heading 3",
        icon: "H₃",
        hint: "###",
        search: "h3 heading3",
    },
    Command {
        action: Action::Block(Block::Heading(4)),
        label: "Heading 4",
        icon: "H₄",
        hint: "####",
        search: "h4 heading4",
    },
    Command {
        action: Action::Block(Block::Bullets),
        label: "Bulleted list",
        icon: "• ≡",
        hint: "-",
        search: "bullet unordered ul list",
    },
    Command {
        action: Action::Block(Block::Numbers),
        label: "Numbered list",
        icon: "1. ≡",
        hint: "1.",
        search: "number ordered ol list",
    },
    Command {
        action: Action::Block(Block::Tasks),
        label: "To-do list",
        icon: "☑",
        hint: "[]",
        search: "todo to-do task checklist checkbox",
    },
    Command {
        action: Action::Block(Block::Quote),
        label: "Quote",
        icon: "❝",
        hint: ">",
        search: "quote blockquote",
    },
    Command {
        action: Action::Link,
        label: "Link",
        icon: "🔗",
        hint: "Ctrl+K",
        search: "link url hyperlink web address",
    },
    Command {
        action: Action::Block(Block::Heading(5)),
        label: "Heading 5",
        icon: "H₅",
        hint: "#####",
        search: "h5 heading5",
    },
    Command {
        action: Action::Block(Block::Heading(6)),
        label: "Heading 6",
        icon: "H₆",
        hint: "######",
        search: "h6 heading6",
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// A slash at a word boundary, on the caret's line. URLs and paths don't
/// trigger commands, and moving onto another line ends the query.
pub fn query(source: &str, cursor: usize) -> Option<Query> {
    let chars: Vec<_> = source.chars().collect();
    if cursor > chars.len() {
        return None;
    }
    let start = chars[..cursor].iter().rposition(|ch| *ch == '/')?;
    if start > 0 && !chars[start - 1].is_whitespace() {
        return None;
    }
    let text = &chars[start + 1..cursor];
    if text.len() > 48
        || text
            .iter()
            .any(|c| !c.is_ascii_alphanumeric() && *c != ' ' && *c != '-')
    {
        return None;
    }
    Some(Query {
        start,
        end: cursor,
        text: text.iter().collect(),
    })
}

impl Command {
    pub fn matches(&self, query: &str) -> bool {
        let terms = format!("{} {}", self.label.to_lowercase(), self.search);
        query
            .to_lowercase()
            .split_whitespace()
            .all(|word| terms.contains(word))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_use_character_offsets_and_ignore_paths() {
        assert_eq!(
            query("🌿 /h2", 5),
            Some(Query {
                start: 2,
                end: 5,
                text: "h2".into()
            })
        );
        assert_eq!(query("/heading 3", 10).unwrap().text, "heading 3");
        for text in [
            "https://example.org",
            "/usr/bin",
            "a/b",
            "/h1\nnext",
            "/bad!",
        ] {
            assert!(query(text, text.chars().count()).is_none(), "{text}");
        }
    }

    #[test]
    fn commands_filter_by_names_and_aliases() {
        for (term, block) in [
            ("h4", Block::Heading(4)),
            ("heading 6", Block::Heading(6)),
            ("todo", Block::Tasks),
            ("paragraph", Block::Text),
        ] {
            let found: Vec<_> = COMMANDS.iter().filter(|c| c.matches(term)).collect();
            assert_eq!(found.len(), 1, "{term}");
            assert_eq!(found[0].action, Action::Block(block));
        }
        assert!(!COMMANDS.iter().any(|c| c.matches("unknown")));
        let found: Vec<_> = COMMANDS.iter().filter(|c| c.matches("url")).collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].action, Action::Link);
    }
}
