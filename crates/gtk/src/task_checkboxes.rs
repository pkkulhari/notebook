use gtk::{glib, prelude::*};
use notebook_core::markdown::Document;
use std::{cell::RefCell, rc::Rc};

type Toggle = Rc<dyn Fn(&gtk::TextBuffer, i32, bool)>;

struct Task {
    button: gtk::CheckButton,
    mark: gtk::TextMark,
    toggled: glib::SignalHandlerId,
    position: Option<(i32, i32)>,
}

/// Native controls over the task markers. Keeping widgets outside the buffer
/// preserves its Markdown, character offsets, clipboard and CRDT edit history.
#[derive(Default)]
pub struct TaskCheckboxes {
    tasks: RefCell<Vec<Task>>,
    on_toggle: RefCell<Option<Toggle>>,
}

impl TaskCheckboxes {
    pub fn connect_toggled(&self, callback: impl Fn(&gtk::TextBuffer, i32, bool) + 'static) {
        *self.on_toggle.borrow_mut() = Some(Rc::new(callback));
    }

    pub fn set_editable(&self, editable: bool) {
        for task in self.tasks.borrow().iter() {
            task.button.set_sensitive(editable);
        }
    }

    pub fn sync(&self, view: &gtk::TextView, document: &Document) {
        let buffer = view.buffer();
        let mut previous = self.tasks.take();
        let mut tasks = Vec::new();
        for span in &document.spans {
            if !matches!(span.style, "task" | "checked") {
                continue;
            }
            let start = buffer.iter_at_offset(span.range.start);
            let existing = previous.iter().position(|task| {
                task.mark.buffer().as_ref() == Some(&buffer)
                    && buffer.iter_at_mark(&task.mark).offset() == span.range.start
            });
            let task = if let Some(index) = existing {
                previous.remove(index)
            } else {
                let button = gtk::CheckButton::new();
                button.add_css_class("task-checkbox");
                button.set_cursor_from_name(Some("default"));
                // CheckButton normally claims clicks on release. Claim its
                // native gesture on press so TextView cannot move the caret.
                let controllers = button.observe_controllers();
                for index in 0..controllers.n_items() {
                    if let Some(click) = controllers.item(index).and_downcast::<gtk::GestureClick>()
                    {
                        click.connect_pressed(|gesture, _, _, _| {
                            gesture.set_state(gtk::EventSequenceState::Claimed);
                        });
                    }
                }
                let mark = buffer.create_mark(None, &start, false);
                let callback = self.on_toggle.borrow().as_ref().unwrap().clone();
                let location = mark.clone();
                let toggled = button.connect_toggled(move |button| {
                    if let Some(buffer) = location.buffer() {
                        callback(
                            &buffer,
                            buffer.iter_at_mark(&location).offset(),
                            button.is_active(),
                        );
                    }
                });
                Task {
                    button,
                    mark,
                    toggled,
                    position: None,
                }
            };
            task.button.block_signal(&task.toggled);
            task.button.set_active(span.style == "checked");
            task.button.unblock_signal(&task.toggled);
            task.button.set_sensitive(view.is_editable());
            task.button
                .set_tooltip_text(Some(if span.style == "checked" {
                    "Mark as incomplete"
                } else {
                    "Mark as complete"
                }));
            let content = buffer.iter_at_offset(span.range.end);
            let mut end = content;
            if !end.ends_line() {
                end.forward_to_line_end();
            }
            let label = buffer.text(&content, &end, false);
            let label = label.trim();
            task.button
                .update_property(&[gtk::accessible::Property::Label(if label.is_empty() {
                    "Task"
                } else {
                    label
                })]);

            // An unordered task needs only the checkbox, while ordered tasks
            // keep their number and nested tasks keep their indentation.
            let mut line = start;
            line.set_line_offset(0);
            let prefix = buffer.text(&line, &start, true);
            let trimmed = prefix.trim_end_matches([' ', '\t']);
            if trimmed.ends_with(['-', '*', '+']) {
                let bullet = line.offset() + trimmed.chars().count() as i32 - 1;
                buffer.apply_tag_by_name("task-bullet", &buffer.iter_at_offset(bullet), &start);
            }
            tasks.push(task);
        }
        // GTK 4.12/4.14 can remove the overlay container, but not an
        // individual overlay. Reattach surviving controls when removing any
        // tasks; this component owns the TextView's overlay container.
        let focused = tasks.iter().find(|task| task.button.has_focus());
        let focused = focused.map(|task| task.button.clone());
        if let Some(parent) = previous.first().and_then(|task| task.button.parent()) {
            view.remove(&parent);
        }
        for task in previous {
            if let Some(buffer) = task.mark.buffer() {
                buffer.delete_mark(&task.mark);
            }
        }
        for task in &mut tasks {
            if task.button.parent().is_none() {
                view.add_overlay(&task.button, 0, 0);
                task.position = None;
            }
        }
        if let Some(button) = focused {
            button.grab_focus();
        }
        *self.tasks.borrow_mut() = tasks;
        self.place(view);
    }

    /// Overlay coordinates are buffer coordinates, so GTK handles scrolling.
    /// Recheck after layout to follow wrapping, margin and font changes.
    pub fn place(&self, view: &gtk::TextView) {
        let buffer = view.buffer();
        for task in self.tasks.borrow_mut().iter_mut() {
            if task.mark.buffer().as_ref() != Some(&buffer) {
                task.button.set_visible(false);
                continue;
            }
            let start = buffer.iter_at_mark(&task.mark);
            let rect = view.iter_location(&start);
            let last = buffer.iter_at_offset((start.offset() + 2).min(buffer.char_count()));
            let last = view.iter_location(&last);
            let left = rect.x().min(last.x());
            let width = (rect.x() + rect.width()).max(last.x() + last.width()) - left;
            if task.button.width_request() != width {
                task.button.set_width_request(width);
            }
            let (_, button_height, _, _) = task.button.measure(gtk::Orientation::Vertical, -1);
            // Align with the task text, not the smaller Markdown marker font.
            let content = buffer.iter_at_offset((start.offset() + 3).min(buffer.char_count()));
            let line = view.iter_location(&content);
            let position = (left, line.y() + (line.height() - button_height) / 2);
            if task.position != Some(position) {
                view.move_overlay(&task.button, position.0, position.1);
                task.position = Some(position);
            }
            task.button.set_visible(true);
        }
    }

    #[cfg(test)]
    pub fn buttons(&self) -> Vec<gtk::CheckButton> {
        self.tasks
            .borrow()
            .iter()
            .map(|task| task.button.clone())
            .collect()
    }
}
