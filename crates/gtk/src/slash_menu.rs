use crate::formatting::{Block, Format};
use gtk::{gdk, prelude::*};
use std::{
    cell::{Cell, RefCell},
    ops::Range,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Block(Block),
    Inline(Format),
}

pub struct Command {
    pub action: Action,
    pub label: &'static str,
    icon: &'static str,
    hint: &'static str,
    search: &'static str,
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
    fn matches(&self, query: &str) -> bool {
        let terms = format!("{} {}", self.label.to_lowercase(), self.search);
        query
            .to_lowercase()
            .split_whitespace()
            .all(|word| terms.contains(word))
    }
}

pub struct SlashMenu {
    pub popover: gtk::Popover,
    pub list: gtk::ListBox,
    heading: gtk::Label,
    footer: gtk::Label,
    scroll: gtk::ScrolledWindow,
    rows: Vec<gtk::ListBoxRow>,
    pub query: RefCell<Option<Query>>,
    pub selection: RefCell<Option<Range<usize>>>,
    dismissed: Cell<bool>,
}

impl SlashMenu {
    pub fn new(editor: &gtk::TextView) -> Self {
        let popover = gtk::Popover::builder()
            .autohide(false)
            .has_arrow(false)
            .position(gtk::PositionType::Bottom)
            .halign(gtk::Align::Start)
            .build();
        popover.add_css_class("slash-menu");
        popover.set_parent(editor);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_size_request(320, -1);
        let heading = gtk::Label::new(Some("Basic blocks"));
        heading.set_xalign(0.0);
        heading.add_css_class("slash-heading");
        root.append(&heading);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(true);
        list.set_focusable(false);
        list.set_focus_on_click(false);
        list.add_css_class("slash-options");
        let empty = gtk::Label::new(Some("No matching commands"));
        empty.add_css_class("slash-empty");
        list.set_placeholder(Some(&empty));
        let mut rows = Vec::new();
        for command in COMMANDS {
            let row = gtk::ListBoxRow::new();
            row.set_focusable(false);
            row.set_focus_on_click(false);
            let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            let icon = gtk::Label::new(Some(command.icon));
            icon.set_width_chars(3);
            icon.add_css_class("slash-icon");
            line.append(&icon);
            let name = gtk::Label::new(Some(command.label));
            name.set_xalign(0.0);
            name.set_hexpand(true);
            line.append(&name);
            let hint = gtk::Label::new(Some(command.hint));
            hint.add_css_class("dim-label");
            line.append(&hint);
            row.set_child(Some(&line));
            list.append(&row);
            rows.push(row);
        }
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_width(300)
            .max_content_height(330)
            .propagate_natural_height(true)
            .child(&list)
            .build();
        root.append(&scroll);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let footer = gtk::Label::new(Some("↑↓ Navigate    ↵ Insert    esc Close"));
        footer.set_xalign(0.0);
        footer.add_css_class("slash-footer");
        root.append(&footer);
        popover.set_child(Some(&root));
        Self {
            popover,
            list,
            heading,
            footer,
            scroll,
            rows,
            query: RefCell::new(None),
            selection: RefCell::new(None),
            dismissed: Cell::new(false),
        }
    }

    pub fn close(&self) {
        self.query.borrow_mut().take();
        self.dismissed.set(true);
        self.popover.popdown();
    }

    /// A dismissed selection stays dismissed until the user changes it.
    pub fn observe_selection(&self, selection: Option<Range<usize>>) -> bool {
        if *self.selection.borrow() == selection {
            return false;
        }
        self.close();
        *self.selection.borrow_mut() = selection;
        self.dismissed.set(false);
        true
    }

    pub fn show(&self, query: Query, editor: &gtk::TextView) {
        let changed = self
            .query
            .borrow()
            .as_ref()
            .is_none_or(|old| old.text != query.text);
        if changed {
            self.filter(|command| {
                matches!(command.action, Action::Block(_)) && command.matches(&query.text)
            });
        }
        self.heading.set_text("Basic blocks");
        self.footer.set_text("↑↓ Navigate    ↵ Insert    esc Close");
        self.popover.set_position(gtk::PositionType::Bottom);
        self.point_to(query.start, editor);
        *self.query.borrow_mut() = Some(query);
        if !self.popover.is_visible() {
            self.popover.popup();
        }
    }

    pub fn show_selection(&self, editor: &gtk::TextView) {
        if self.selection.borrow().is_none() || self.dismissed.get() {
            return;
        }
        if !self.popover.is_visible() {
            self.filter(|_| true);
        }
        self.heading.set_text("Basic blocks");
        self.footer.set_text("↑↓ Navigate    ↵ Apply    esc Close");
        self.popover.set_position(gtk::PositionType::Bottom);
        self.point_to(editor.buffer().cursor_position() as usize, editor);
        if !self.popover.is_visible() {
            self.popover.popup();
        }
    }

    fn filter(&self, include: impl Fn(&Command) -> bool) {
        self.list.select_row(None::<&gtk::ListBoxRow>);
        for (row, command) in self.rows.iter().zip(COMMANDS) {
            row.set_visible(include(command));
        }
        if let Some(row) = self.rows.iter().find(|row| row.get_visible()) {
            self.list.select_row(Some(row));
        }
        self.scroll.vadjustment().set_value(0.0);
    }

    fn point_to(&self, offset: usize, editor: &gtk::TextView) {
        let rect = editor.iter_location(&editor.buffer().iter_at_offset(offset as i32));
        let visible = editor.visible_rect();
        // Select-all can leave the caret offscreen. Keep the menu in view.
        let (x, y) = editor.buffer_to_window_coords(
            gtk::TextWindowType::Widget,
            rect.x().clamp(visible.x(), visible.x() + visible.width()),
            rect.y().clamp(visible.y(), visible.y() + visible.height()),
        );
        self.popover
            .set_pointing_to(Some(&gdk::Rectangle::new(x, y, 1, rect.height())));
    }

    pub fn move_selection(&self, direction: i32) {
        let shown: Vec<_> = self.rows.iter().filter(|row| row.get_visible()).collect();
        if shown.is_empty() {
            return;
        }
        let selected = self.list.selected_row();
        let position = shown
            .iter()
            .position(|row| Some(*row) == selected.as_ref())
            .unwrap_or(0);
        let next = (position as i32 + direction).rem_euclid(shown.len() as i32) as usize;
        let row = shown[next];
        self.list.select_row(Some(row));
        if let Some(bounds) = row.compute_bounds(&self.list) {
            self.scroll
                .vadjustment()
                .clamp_page(bounds.y() as f64, (bounds.y() + bounds.height()) as f64);
        }
    }

    pub fn selected(&self) -> Option<Action> {
        self.list
            .selected_row()
            .and_then(|row| COMMANDS.get(row.index() as usize))
            .map(|c| c.action)
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
    }
}
