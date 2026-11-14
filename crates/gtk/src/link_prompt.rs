use gtk::prelude::*;
use std::{cell::RefCell, ops::Range};

use crate::slash_menu::point_popover;

/// What a link prompt will link: the text or caret `range` in a note, as the
/// note read when the prompt opened. A changed note makes the range stale.
pub struct LinkTarget {
    pub note: String,
    pub body: String,
    pub range: Range<usize>,
}

/// Asks for the URL of a link from the slash menu, selection menu or Ctrl+K.
pub struct LinkPrompt {
    pub popover: gtk::Popover,
    pub entry: gtk::Entry,
    pub target: RefCell<Option<LinkTarget>>,
}

impl LinkPrompt {
    pub fn new(editor: &gtk::TextView) -> Self {
        let popover = gtk::Popover::builder()
            .autohide(true)
            .has_arrow(false)
            .position(gtk::PositionType::Bottom)
            .halign(gtk::Align::Start)
            .build();
        popover.add_css_class("slash-menu");
        popover.add_css_class("link-prompt");
        popover.set_parent(editor);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_size_request(320, -1);
        let heading = gtk::Label::new(Some("Link"));
        heading.set_xalign(0.0);
        heading.add_css_class("slash-heading");
        root.append(&heading);
        let entry = gtk::Entry::builder()
            .placeholder_text("Paste or type a link")
            .input_purpose(gtk::InputPurpose::Url)
            .activates_default(false)
            .build();
        entry.add_css_class("link-entry");
        root.append(&entry);
        let footer = gtk::Label::new(Some("↵ Insert    esc Cancel"));
        footer.set_xalign(0.0);
        footer.add_css_class("slash-footer");
        root.append(&footer);
        popover.set_child(Some(&root));
        Self {
            popover,
            entry,
            target: RefCell::new(None),
        }
    }

    pub fn open(&self, target: LinkTarget, editor: &gtk::TextView) {
        point_popover(&self.popover, target.range.end, editor);
        *self.target.borrow_mut() = Some(target);
        self.entry.set_text("");
        self.popover.popup();
        self.entry.grab_focus();
    }
}
