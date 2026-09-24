use gtk::{gdk, gio, glib, pango, prelude::*};
use notebook::{
    markdown::{self, Document},
    model::*,
    storage::{self, Command, Event, Mutation},
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

struct Draft {
    note: Note,
    buffer: gtk::TextBuffer,
    sequence: u64,
    saved: u64,
    queued: u64,
    changed: Instant,
    last_queued: Instant,
    last_opened: Instant,
    created: bool,
}

struct State {
    default_notebook_id: String,
    notebooks: Vec<Notebook>,
    notes: Vec<NoteSummary>,
    counts: NoteCounts,
    drafts: HashMap<String, Draft>,
    active: Option<String>,
    filter: Filter,
    list_generation: u64,
    load_generation: u64,
    parse_generation: u64,
    parsed_generation: u64,
    parse_due: Option<Instant>,
    document: Document,
    hidden_applied: Vec<std::ops::Range<i32>>,
    failed: Vec<Command>,
    closing: bool,
    ready: bool,
    select_first: bool,
    reveal_active: bool,
}

pub(crate) struct Ui {
    window: gtk::ApplicationWindow,
    outer: gtk::Paned,
    inner: gtk::Paned,
    sidebar: gtk::ListBox,
    sidebar_counts: RefCell<Vec<(Filter, gtk::Label)>>,
    search: gtk::SearchEntry,
    list_title: gtk::Label,
    note_model: gio::ListStore,
    note_list: gtk::ListView,
    note_items: Rc<RefCell<Vec<glib::WeakRef<gtk::ListItem>>>>,
    active_position: Rc<Cell<u32>>,
    selection: gtk::SingleSelection,
    editor: gtk::TextView,
    editor_stack: gtk::Stack,
    placeholder: gtk::Label,
    list_empty: gtk::Label,
    words: gtk::Label,
    count: gtk::Label,
    location: gtk::MenuButton,
    trash_button: gtk::Button,
    error_box: gtk::Box,
    error_label: gtk::Label,
    retry: gtk::Button,
    commands: Sender<Command>,
    events: Receiver<Event>,
    parse_requests: Sender<(u64, String)>,
    parse_results: Receiver<(u64, Document)>,
    state: RefCell<State>,
    updating: Cell<bool>,
    allow_close: Cell<bool>,
    search_due: Cell<Option<Instant>>,
    prefs_due: Cell<Instant>,
    dates_updated: Cell<Instant>,
}

pub(crate) fn launch(app: &gtk::Application) -> Rc<Ui> {
    build(app, storage::data_path())
}

fn build(app: &gtk::Application, path: std::path::PathBuf) -> Rc<Ui> {
    let (commands, events) = storage::spawn_worker(path);
    let (parse_requests, incoming) = mpsc::channel::<(u64, String)>();
    let (outgoing, parse_results) = mpsc::channel();
    std::thread::Builder::new()
        .name("notebook-markdown".into())
        .spawn(move || {
            while let Ok(mut job) = incoming.recv() {
                while let Ok(newer) = incoming.try_recv() {
                    job = newer;
                }
                if outgoing.send((job.0, markdown::parse(&job.1))).is_err() {
                    break;
                }
            }
        })
        .expect("could not start Markdown worker");

    let css = gtk::CssProvider::new();
    css.load_from_string(include_str!("style.css"));
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("display"),
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Notebook")
        .default_width(1120)
        .default_height(760)
        .build();
    window.add_css_class("notebook-app");
    let header = gtk::HeaderBar::new();
    let brand = gtk::Label::new(Some("Notebook"));
    brand.add_css_class("brand");
    header.set_title_widget(Some(&brand));
    let focus = icon_button("view-dual-symbolic", "Show or hide navigation · F9");
    focus.set_action_name(Some("win.focus"));
    header.pack_start(&focus);
    let menu = gio::Menu::new();
    menu.append(Some("New note"), Some("win.new-note"));
    menu.append(Some("Search notes"), Some("win.search"));
    menu.append(Some("Focus writing"), Some("win.focus"));
    menu.append(Some("Keyboard shortcuts"), Some("win.shortcuts"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .tooltip_text("Notebook menu")
        .build();
    menu_button.add_css_class("quiet-menu");
    header.pack_end(&menu_button);
    window.set_titlebar(Some(&header));

    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_box.add_css_class("sidebar-panel");
    let sidebar_header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    sidebar_header.add_css_class("pane-heading");
    let notebook_label = gtk::Label::new(Some("Library"));
    notebook_label.set_xalign(0.0);
    notebook_label.set_hexpand(true);
    notebook_label.add_css_class("eyebrow");
    sidebar_header.append(&notebook_label);
    let add_book = icon_button("list-add-symbolic", "New notebook");
    add_book.set_action_name(Some("win.new-notebook"));
    sidebar_header.append(&add_book);
    sidebar_box.append(&sidebar_header);
    let sidebar = gtk::ListBox::new();
    sidebar.set_header_func(|row, _| {
        if row.has_css_class("trash-row") {
            let divider = gtk::Separator::new(gtk::Orientation::Horizontal);
            divider.add_css_class("trash-divider");
            row.set_header(Some(&divider));
        } else {
            row.set_header(None::<&gtk::Widget>);
        }
    });
    sidebar.add_css_class("navigation-sidebar");
    sidebar.set_selection_mode(gtk::SelectionMode::Single);
    let sidebar_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&sidebar)
        .build();
    sidebar_box.append(&sidebar_scroll);
    let list_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list_box.add_css_class("notes-panel");
    let list_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    list_header.add_css_class("list-heading");
    let list_title = gtk::Label::new(Some("Default"));
    list_title.set_xalign(0.0);
    list_title.set_hexpand(true);
    list_title.set_ellipsize(pango::EllipsizeMode::End);
    list_title.add_css_class("list-title");
    list_header.append(&list_title);
    let new_note = gtk::Button::new();
    let new_content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    new_content.append(&gtk::Image::from_icon_name("list-add-symbolic"));
    new_content.append(&gtk::Label::new(Some("New note")));
    new_note.set_child(Some(&new_content));
    new_note.add_css_class("new-note-button");
    new_note.set_tooltip_text(Some("Start writing in Default · Ctrl+N"));
    new_note.set_action_name(Some("win.new-note"));
    list_header.append(&new_note);
    list_box.append(&list_header);
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search all notes")
        .build();
    search.set_margin_start(14);
    search.set_margin_end(14);
    search.set_margin_bottom(14);
    search.add_css_class("note-search");
    search.set_hexpand(true);
    list_box.append(&search);
    let note_model = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(note_model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    let note_items = Rc::new(RefCell::new(Vec::<glib::WeakRef<gtk::ListItem>>::new()));
    let active_position = Rc::new(Cell::new(gtk::INVALID_LIST_POSITION));
    let factory = gtk::SignalListItemFactory::new();
    let items = note_items.clone();
    factory.connect_setup(move |_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
        let row = gtk::Box::new(gtk::Orientation::Vertical, 3);
        row.add_css_class("note-row");
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let title = gtk::Label::new(None);
        title.set_xalign(0.0);
        title.set_hexpand(true);
        title.set_ellipsize(pango::EllipsizeMode::End);
        title.add_css_class("note-title");
        let date = gtk::Label::new(None);
        date.set_xalign(1.0);
        date.add_css_class("note-date");
        heading.append(&title);
        heading.append(&date);
        let preview = gtk::Label::new(None);
        preview.set_xalign(0.0);
        preview.set_ellipsize(pango::EllipsizeMode::End);
        preview.add_css_class("note-preview");
        row.append(&heading);
        row.append(&preview);
        item.set_child(Some(&row));
        items.borrow_mut().push(item.downgrade());
    });
    let active = active_position.clone();
    factory.connect_bind(move |_, object| {
        let item = object.downcast_ref::<gtk::ListItem>().expect("list item");
        let object = item
            .item()
            .and_downcast::<glib::BoxedAnyObject>()
            .expect("note item");
        let note = object.borrow::<NoteSummary>();
        let row = item.child().expect("note row");
        let heading = row.first_child().expect("heading");
        let title = heading
            .first_child()
            .and_downcast::<gtk::Label>()
            .expect("title");
        title.set_text(&note.label);
        let preview = row
            .last_child()
            .and_downcast::<gtk::Label>()
            .expect("preview");
        preview.set_text(if note.preview.is_empty() {
            " "
        } else {
            &note.preview
        });
        update_note_date(item);
        if item.position() != gtk::INVALID_LIST_POSITION && item.position() == active.get() {
            row.add_css_class("active-note");
        } else {
            row.remove_css_class("active-note");
        }
    });
    let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
    list.set_single_click_activate(true);
    list.add_css_class("note-list");
    let list_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    let list_overlay = gtk::Overlay::new();
    list_overlay.set_child(Some(&list_scroll));
    let list_empty = gtk::Label::new(Some("No notes here"));
    list_empty.add_css_class("list-empty");
    list_empty.set_valign(gtk::Align::Start);
    list_empty.set_margin_top(30);
    list_empty.set_can_target(false);
    list_empty.set_visible(false);
    list_overlay.add_overlay(&list_empty);
    list_box.append(&list_overlay);
    let count = gtk::Label::new(Some("Opening your notebook…"));
    count.add_css_class("footer");
    count.set_xalign(0.0);
    list_box.append(&count);

    let writing = gtk::Box::new(gtk::Orientation::Vertical, 0);
    writing.add_css_class("writing-panel");
    let editor_header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    editor_header.add_css_class("editor-heading");
    let location = gtk::MenuButton::builder()
        .label("Default")
        .tooltip_text("Move this note to a notebook")
        .build();
    location.add_css_class("flat");
    location.add_css_class("location-menu");
    editor_header.append(&location);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    editor_header.append(&spacer);
    let trash_button = icon_button(
        "user-trash-symbolic",
        "Move note to Trash · Ctrl+Shift+Delete",
    );
    trash_button.set_action_name(Some("win.trash"));
    editor_header.append(&trash_button);
    writing.append(&editor_header);

    let editor = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(48)
        .right_margin(48)
        .top_margin(34)
        .bottom_margin(80)
        .pixels_below_lines(0)
        .hexpand(true)
        .vexpand(true)
        .build();
    editor.add_css_class("editor");
    editor.set_accepts_tab(true);
    let editor_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&editor)
        .build();
    let editor_stack = gtk::Stack::new();
    editor_stack.set_vexpand(true);
    let editor_overlay = gtk::Overlay::new();
    editor_overlay.set_child(Some(&editor_scroll));
    let placeholder = gtk::Label::new(Some("Start writing…"));
    placeholder.add_css_class("writing-placeholder");
    placeholder.set_halign(gtk::Align::Start);
    placeholder.set_valign(gtk::Align::Start);
    placeholder.set_margin_start(48);
    placeholder.set_margin_top(34);
    placeholder.set_can_target(false);
    editor_overlay.add_overlay(&placeholder);
    editor_stack.add_named(&editor_overlay, Some("editor"));
    let empty = gtk::Box::new(gtk::Orientation::Vertical, 14);
    empty.set_valign(gtk::Align::Center);
    empty.set_halign(gtk::Align::Center);
    let empty_title = gtk::Label::new(Some("Room for your next thought"));
    empty_title.add_css_class("empty-title");
    empty.append(&empty_title);
    let empty_hint = gtk::Label::new(Some("Choose a note, or start something new."));
    empty_hint.add_css_class("dim-label");
    empty.append(&empty_hint);
    let empty_new = gtk::Button::with_label("New note");
    empty_new.set_action_name(Some("win.new-note"));
    empty_new.set_halign(gtk::Align::Center);
    empty.append(&empty_new);
    editor_stack.add_named(&empty, Some("empty"));
    editor_stack.set_visible_child_name("empty");
    writing.append(&editor_stack);
    let editor_footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    editor_footer.add_css_class("editor-footer");
    let words = gtk::Label::new(Some(""));
    words.set_xalign(0.0);
    words.set_hexpand(true);
    editor_footer.append(&words);
    writing.append(&editor_footer);

    let inner = gtk::Paned::new(gtk::Orientation::Horizontal);
    inner.set_start_child(Some(&list_box));
    inner.set_end_child(Some(&writing));
    inner.set_position(280);
    inner.set_resize_start_child(false);
    inner.set_shrink_start_child(false);
    inner.set_shrink_end_child(false);
    list_box.set_size_request(220, -1);
    writing.set_size_request(340, -1);
    let outer = gtk::Paned::new(gtk::Orientation::Horizontal);
    outer.set_start_child(Some(&sidebar_box));
    outer.set_end_child(Some(&inner));
    outer.set_position(190);
    outer.set_resize_start_child(false);
    outer.set_shrink_start_child(false);
    outer.set_shrink_end_child(false);
    sidebar_box.set_size_request(160, -1);
    outer.set_vexpand(true);
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.append(&outer);
    let error_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    error_box.add_css_class("error-banner");
    let error_label = gtk::Label::new(None);
    error_label.set_wrap(true);
    error_label.set_xalign(0.0);
    error_label.set_hexpand(true);
    let retry = gtk::Button::with_label("Retry");
    error_box.append(&error_label);
    error_box.append(&retry);
    error_box.set_visible(false);
    root.append(&error_box);
    window.set_child(Some(&root));
    let ui = Rc::new(Ui {
        window,
        outer,
        inner,
        sidebar,
        sidebar_counts: RefCell::new(Vec::new()),
        search,
        list_title,
        note_model,
        note_list: list,
        note_items,
        active_position,
        selection,
        editor,
        editor_stack,
        placeholder,
        list_empty,
        words,
        count,
        location,
        trash_button,
        error_box,
        error_label,
        retry,
        commands,
        events,
        parse_requests,
        parse_results,
        updating: Cell::new(false),
        allow_close: Cell::new(false),
        search_due: Cell::new(None),
        prefs_due: Cell::new(Instant::now()),
        dates_updated: Cell::new(Instant::now()),
        state: RefCell::new(State {
            default_notebook_id: DEFAULT_NOTEBOOK.into(),
            notebooks: vec![],
            notes: vec![],
            counts: NoteCounts::new(),
            drafts: HashMap::new(),
            active: None,
            filter: Filter::Notebook(DEFAULT_NOTEBOOK.into()),
            list_generation: 0,
            load_generation: 0,
            parse_generation: 0,
            parsed_generation: 0,
            parse_due: None,
            document: Document::default(),
            hidden_applied: vec![],
            failed: vec![],
            closing: false,
            ready: false,
            select_first: false,
            reveal_active: true,
        }),
    });
    ui.connect(app);
    ui.send(Command::Initialize);
    ui.window.present();
    // The timer owns the controller until the window closes; widget callbacks use Weak.
    let controller = ui.clone();
    glib::timeout_add_local(Duration::from_millis(16), move || {
        if !controller.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        controller.tick();
        glib::ControlFlow::Continue
    });
    ui
}

impl Ui {
    fn connect(self: &Rc<Self>, app: &gtk::Application) {
        for name in ["rename-book", "delete-book"] {
            let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
            let weak = Rc::downgrade(self);
            action.connect_activate(move |action, value| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let Some(id) = value.and_then(|v| v.str()) else {
                    return;
                };
                let book = {
                    let s = ui.state.borrow();
                    if id == s.default_notebook_id {
                        return;
                    }
                    s.notebooks.iter().find(|b| b.id == id).cloned()
                };
                if let Some(book) = book {
                    if action.name() == "rename-book" {
                        ui.show_notebook_dialog(Some(book));
                    } else {
                        ui.flush_drafts();
                        ui.send(Command::Mutate(Mutation::DeleteNotebook { id: book.id }));
                    }
                }
            });
            self.window.add_action(&action);
        }
        self.action("new-note", |ui| ui.new_note());
        self.action("search", |ui| {
            ui.outer.start_child().unwrap().set_visible(true);
            ui.inner.start_child().unwrap().set_visible(true);
            ui.search.grab_focus();
        });
        self.action("focus", |ui| {
            let visible = !ui.outer.start_child().unwrap().is_visible();
            ui.outer.start_child().unwrap().set_visible(visible);
            ui.inner.start_child().unwrap().set_visible(visible);
            ui.editor.grab_focus();
        });
        self.action("trash", |ui| ui.trash_or_restore());
        self.action("new-notebook", |ui| ui.notebook_dialog(false));
        self.action("rename-notebook", |ui| ui.notebook_dialog(true));
        self.action("delete-notebook", |ui| {
            let filter = ui.state.borrow().filter.clone();
            let default_id = ui.state.borrow().default_notebook_id.clone();
            if let Filter::Notebook(id) = filter
                && id != default_id
            {
                ui.flush_drafts();
                ui.send(Command::Mutate(Mutation::DeleteNotebook { id }));
            }
        });
        self.action("undo", |ui| {
            let buffer = ui.editor.buffer();
            if ui.editor.is_editable() && buffer.can_undo() {
                buffer.undo();
            }
        });
        self.action("redo", |ui| {
            let buffer = ui.editor.buffer();
            if ui.editor.is_editable() && buffer.can_redo() {
                buffer.redo();
            }
        });
        self.action("save", |ui| ui.flush_drafts());
        self.action("shortcuts", |ui| {
            let dialog = gtk::Window::builder().title("Keyboard shortcuts").transient_for(&ui.window).modal(true).default_width(420).build();
            let label = gtk::Label::new(Some("New note                 Ctrl+N\nSearch all notes         Ctrl+F\nFocus writing            F9\nSave now                 Ctrl+S\nUndo                     Ctrl+Z\nRedo                     Ctrl+Shift+Z\nTrash / restore          Ctrl+Shift+Delete\nOpen link                Ctrl+click\nReturn to writing        Escape"));
            label.set_margin_top(28); label.set_margin_bottom(28); label.set_margin_start(28); label.set_margin_end(28); label.add_css_class("shortcuts"); dialog.set_child(Some(&label)); dialog.present();
        });
        for (action, keys) in [
            ("new-note", vec!["<Primary>n"]),
            ("search", vec!["<Primary>f"]),
            ("focus", vec!["F9"]),
            ("trash", vec!["<Primary><Shift>Delete"]),
            ("undo", vec!["<Primary>z"]),
            ("redo", vec!["<Primary><Shift>z"]),
            ("save", vec!["<Primary>s"]),
        ] {
            app.set_accels_for_action(&format!("win.{action}"), &keys);
        }
        let weak = Rc::downgrade(self);
        self.sidebar.connect_row_selected(move |_, row| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.updating.get() {
                return;
            }
            if let Some(row) = row {
                let index = row.index();
                let filter = {
                    let s = ui.state.borrow();
                    if index == 0 {
                        Filter::All
                    } else if index as usize == s.notebooks.len() + 1 {
                        Filter::Trash
                    } else if let Some(book) = s.notebooks.get(index as usize - 1) {
                        Filter::Notebook(book.id.clone())
                    } else {
                        return;
                    }
                };
                ui.flush_drafts();
                {
                    let mut s = ui.state.borrow_mut();
                    s.filter = filter;
                    s.select_first = true;
                    s.load_generation += 1;
                }
                ui.search.set_text("");
                ui.request_list();
            }
        });
        let weak = Rc::downgrade(self);
        // Single-click mode may select rows on hover. Only activation opens a note.
        self.note_list.connect_activate(move |_, position| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.updating.get() {
                return;
            }
            let id = ui
                .state
                .borrow()
                .notes
                .get(position as usize)
                .map(|n| n.id.clone());
            if let Some(id) = id {
                ui.open_note(&id);
            }
        });
        let weak = Rc::downgrade(self);
        self.search.connect_search_changed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.search_due
                    .set(Some(Instant::now() + Duration::from_millis(100)));
            }
        });
        let weak = Rc::downgrade(self);
        self.search.connect_stop_search(move |search| {
            if let Some(ui) = weak.upgrade() {
                search.set_text("");
                ui.editor.grab_focus();
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape
                && let Some(ui) = weak.upgrade()
            {
                ui.editor.grab_focus();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.window.add_controller(keys);
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_released(move |gesture, _, x, y| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if !gesture
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
            {
                return;
            }
            let (x, y) =
                ui.editor
                    .window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
            if let Some(iter) = ui.editor.iter_at_location(x, y) {
                let url = ui
                    .state
                    .borrow()
                    .document
                    .links
                    .iter()
                    .find(|(range, _)| range.contains(&iter.offset()))
                    .map(|(_, url)| url.clone());
                if let Some(url) = url
                    && (url.starts_with("https://")
                        || url.starts_with("http://")
                        || url.starts_with("mailto:"))
                {
                    gio::AppInfo::launch_default_for_uri_async(
                        &url,
                        None::<&gio::AppLaunchContext>,
                        None::<&gio::Cancellable>,
                        |_| {},
                    );
                }
            }
        });
        self.editor.add_controller(click);
        // Preserve Markdown delimiters even when presentation tags hide them.
        self.editor.connect_copy_clipboard(|view| {
            view.stop_signal_emission_by_name("copy-clipboard");
            let buffer = view.buffer();
            if let Some((start, end)) = buffer.selection_bounds() {
                view.clipboard().set_text(&buffer.text(&start, &end, true));
            }
        });
        self.editor.connect_cut_clipboard(|view| {
            view.stop_signal_emission_by_name("cut-clipboard");
            let buffer = view.buffer();
            if let Some((start, end)) = buffer.selection_bounds() {
                view.clipboard().set_text(&buffer.text(&start, &end, true));
                if view.is_editable() {
                    buffer.begin_user_action();
                    buffer.delete_selection(true, true);
                    buffer.end_user_action();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.retry.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let failed = std::mem::take(&mut ui.state.borrow_mut().failed);
                ui.error_box.set_visible(false);
                for command in failed {
                    if let Command::Save { id, .. } = command {
                        // Retrying an old snapshot could overwrite a more recent save.
                        let mut s = ui.state.borrow_mut();
                        if let Some(d) = s.drafts.get_mut(&id) {
                            ui.send(Command::Save {
                                id,
                                body: d.note.body.clone(),
                                sequence: d.sequence,
                            });
                            d.queued = d.sequence;
                        }
                    } else {
                        ui.send(command);
                    }
                }
                ui.flush_drafts();
            }
        });
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            let Some(ui) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if ui.allow_close.get() {
                return glib::Propagation::Proceed;
            }
            if !ui.state.borrow().failed.is_empty() {
                ui.error_label.set_text(
                    "Changes could not be saved. Retry before closing; your text is still here.",
                );
                ui.error_box.set_visible(true);
                return glib::Propagation::Stop;
            }
            ui.state.borrow_mut().closing = true;
            ui.window.set_sensitive(false);
            ui.flush_drafts();
            ui.save_preferences();
            ui.send(Command::Flush);
            glib::Propagation::Stop
        });
    }

    fn action(self: &Rc<Self>, name: &str, callback: impl Fn(&Rc<Self>) + 'static) {
        let action = gio::SimpleAction::new(name, None);
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, _| {
            if let Some(ui) = weak.upgrade() {
                let ready = ui.state.borrow().ready;
                if ready {
                    callback(&ui);
                }
            }
        });
        self.window.add_action(&action);
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_err() {
            self.error_label.set_text(
                "The storage worker stopped. Keep this window open to preserve unsaved text.",
            );
            self.error_box.set_visible(true);
        }
    }

    fn request_list(&self) {
        let mut s = self.state.borrow_mut();
        let can_manage = matches!(&s.filter, Filter::Notebook(id) if id != &s.default_notebook_id);
        for name in ["rename-notebook", "delete-notebook"] {
            if let Some(action) = self
                .window
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(can_manage);
            }
        }
        s.list_generation += 1;
        let query = self.search.text().to_string();
        let filter = if query.trim().is_empty() || s.filter == Filter::Trash {
            s.filter.clone()
        } else {
            Filter::All
        };
        self.list_title.set_text(if !query.trim().is_empty() {
            "Search results"
        } else {
            match &filter {
                Filter::All => "All notes",
                Filter::Trash => "Trash",
                Filter::Notebook(id) => s
                    .notebooks
                    .iter()
                    .find(|b| b.id == *id)
                    .map(|b| b.name.as_str())
                    .unwrap_or("Default"),
            }
        });
        self.send(Command::List {
            filter,
            query,
            generation: s.list_generation,
        });
    }

    fn rebuild_sidebar(self: &Rc<Self>) {
        self.updating.set(true);
        self.sidebar_counts.borrow_mut().clear();
        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }
        let s = self.state.borrow();
        let mut entries = vec![("All notes".to_string(), "view-list-symbolic", Filter::All)];
        entries.extend(s.notebooks.iter().map(|n| {
            (
                n.name.clone(),
                "folder-symbolic",
                Filter::Notebook(n.id.clone()),
            )
        }));
        entries.push(("Trash".to_string(), "user-trash-symbolic", Filter::Trash));
        for (name, icon, filter) in entries {
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
            content.append(&gtk::Image::from_icon_name(icon));
            let label = gtk::Label::new(Some(&name));
            label.set_xalign(0.0);
            label.set_ellipsize(pango::EllipsizeMode::End);
            label.set_hexpand(true);
            content.append(&label);
            if let Filter::Notebook(id) = &filter
                && id != &s.default_notebook_id
            {
                let menu = gio::Menu::new();
                for (label, action) in [
                    ("Rename notebook", "win.rename-book"),
                    ("Delete · move notes to Default", "win.delete-book"),
                ] {
                    let item = gio::MenuItem::new(Some(label), None);
                    item.set_action_and_target_value(Some(action), Some(&id.to_variant()));
                    menu.append_item(&item);
                }
                let options = gtk::MenuButton::builder()
                    .icon_name("view-more-symbolic")
                    .menu_model(&menu)
                    .tooltip_text(format!("Options for {name}"))
                    .build();
                options.add_css_class("notebook-options");
                options.add_css_class("quiet-menu");
                content.append(&options);
            }
            let count = gtk::Label::new(Some(
                &s.counts.get(&filter).copied().unwrap_or(0).to_string(),
            ));
            count.add_css_class("notebook-count");
            count.set_xalign(1.0);
            content.append(&count);
            self.sidebar_counts
                .borrow_mut()
                .push((filter.clone(), count));
            let row = gtk::ListBoxRow::new();
            if filter == Filter::Trash {
                row.add_css_class("trash-row");
            }
            row.set_child(Some(&content));
            self.sidebar.append(&row);
            if filter == s.filter {
                self.sidebar.select_row(Some(&row));
            }
        }
        drop(s);
        self.updating.set(false);
        self.update_location();
    }

    fn update_notebook_counts(&self) {
        let s = self.state.borrow();
        for (filter, label) in self.sidebar_counts.borrow().iter() {
            let text = s.counts.get(filter).copied().unwrap_or(0).to_string();
            if label.text() != text {
                label.set_text(&text);
            }
        }
    }

    fn update_location(self: &Rc<Self>) {
        let s = self.state.borrow();
        let draft = s.active.as_ref().and_then(|id| s.drafts.get(id));
        self.location
            .set_sensitive(draft.is_some_and(|d| !d.note.deleted));
        self.trash_button.set_sensitive(draft.is_some());
        let Some(draft) = draft else {
            return;
        };
        self.location.set_label(
            &s.notebooks
                .iter()
                .find(|b| b.id == draft.note.notebook_id)
                .map(|b| b.name.clone())
                .unwrap_or_else(|| "Default".into()),
        );
        self.trash_button.set_icon_name(if draft.note.deleted {
            "edit-undo-symbolic"
        } else {
            "user-trash-symbolic"
        });
        self.trash_button
            .set_tooltip_text(Some(if draft.note.deleted {
                "Restore note"
            } else {
                "Move note to Trash"
            }));
        let popover = gtk::Popover::new();
        let choices = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let caption = gtk::Label::new(Some("Move to notebook"));
        caption.add_css_class("dim-label");
        caption.set_margin_bottom(8);
        choices.append(&caption);
        for book in &s.notebooks {
            let button = gtk::Button::with_label(&book.name);
            button.add_css_class("flat");
            let weak = Rc::downgrade(self);
            let notebook_id = book.id.clone();
            let popover = popover.downgrade();
            button.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    let id = ui.state.borrow().active.clone();
                    if let Some(id) = id {
                        ui.flush_drafts();
                        ui.send(Command::Mutate(Mutation::Move {
                            id,
                            notebook_id: notebook_id.clone(),
                        }));
                    }
                }
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            });
            choices.append(&button);
        }
        popover.set_child(Some(&choices));
        self.location.set_popover(Some(&popover));
    }

    fn install_draft(self: &Rc<Self>, note: Note, created: bool) {
        let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
        configure_tags(&buffer);
        buffer.set_text(&note.body);
        buffer.set_enable_undo(true);
        buffer.set_max_undo_levels(500);
        let id = note.id.clone();
        let weak = Rc::downgrade(self);
        buffer.connect_changed(move |buffer| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let mut s = ui.state.borrow_mut();
            let Some(draft) = s.drafts.get_mut(&id) else {
                return;
            };
            draft.note.body = buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string();
            draft.sequence += 1;
            draft.changed = Instant::now();
            let empty = draft.note.body.is_empty();
            if s.active.as_deref() == Some(&id) {
                ui.placeholder.set_visible(empty);
                s.parse_generation += 1;
                s.parse_due = Some(Instant::now() + Duration::from_millis(80));
                s.document = Document::default();
                // Tags elsewhere track edits automatically. Reveal only the edited line
                // while a new parse is pending, avoiding whole-document flicker.
                let mut start = buffer.iter_at_offset(buffer.cursor_position());
                start.set_line_offset(0);
                let mut end = start;
                end.forward_to_line_end();
                buffer.remove_tag_by_name("hidden", &start, &end);
            }
        });
        let weak = Rc::downgrade(self);
        buffer.connect_mark_set(move |_, _, mark| {
            if (mark.name().as_deref() == Some("insert")
                || mark.name().as_deref() == Some("selection_bound"))
                && let Some(ui) = weak.upgrade()
                && !ui.updating.get()
            {
                ui.update_hidden();
            }
        });
        self.state.borrow_mut().drafts.insert(
            note.id.clone(),
            Draft {
                note,
                buffer,
                sequence: 0,
                saved: 0,
                queued: 0,
                changed: Instant::now(),
                last_queued: Instant::now(),
                last_opened: Instant::now(),
                created,
            },
        );
    }

    fn display_note(self: &Rc<Self>, id: &str, cursor: Option<i32>) {
        self.updating.set(true);
        let (buffer, editable) = {
            let mut s = self.state.borrow_mut();
            s.active = Some(id.into());
            s.parse_generation += 1;
            s.parse_due = Some(Instant::now());
            s.document = Document::default();
            let d = s.drafts.get_mut(id).expect("loaded draft");
            d.last_opened = Instant::now();
            (d.buffer.clone(), !d.note.deleted)
        };
        self.editor.set_buffer(Some(&buffer));
        self.update_writing_margins();
        self.placeholder
            .set_visible(buffer.char_count() == 0 && editable);
        self.editor.set_editable(editable);
        if let Some(cursor) = cursor {
            buffer.place_cursor(&buffer.iter_at_offset(cursor.clamp(0, buffer.char_count())));
        }
        self.editor_stack.set_visible_child_name("editor");
        self.updating.set(false);
        self.update_location();
        self.editor.grab_focus();
        self.update_word_count();
        self.trim_cache();
        self.update_active_marker();
    }

    // GTK's selection follows hover in single-click mode; the open note does not.
    fn update_active_marker(&self) {
        let position = {
            let s = self.state.borrow();
            s.notes
                .iter()
                .position(|n| s.active.as_deref() == Some(&n.id))
                .map(|p| p as u32)
                .unwrap_or(gtk::INVALID_LIST_POSITION)
        };
        self.active_position.set(position);
        self.note_items.borrow_mut().retain(|weak| {
            let Some(item) = weak.upgrade() else {
                return false;
            };
            if let Some(child) = item.child() {
                if position != gtk::INVALID_LIST_POSITION && item.position() == position {
                    child.add_css_class("active-note");
                } else {
                    child.remove_css_class("active-note");
                }
            }
            true
        });
    }

    fn trim_cache(&self) {
        let mut s = self.state.borrow_mut();
        let mut candidates: Vec<_> = s
            .drafts
            .iter()
            .filter(|(id, d)| s.active.as_ref() != Some(id) && d.created && d.sequence == d.saved)
            .map(|(id, d)| (d.last_opened, id.clone()))
            .collect();
        candidates.sort();
        let excess = s.drafts.len().saturating_sub(12);
        for (_, id) in candidates.into_iter().take(excess) {
            s.drafts.remove(&id);
        }
    }

    fn open_note(self: &Rc<Self>, id: &str) {
        self.flush_drafts();
        let (cached, generation) = {
            let mut s = self.state.borrow_mut();
            s.load_generation += 1;
            (s.drafts.contains_key(id), s.load_generation)
        };
        if cached {
            self.display_note(id, None);
        } else {
            self.send(Command::Load {
                id: id.into(),
                generation,
            });
        }
    }

    fn new_note(self: &Rc<Self>) {
        self.flush_drafts();
        let mut note = Note::blank();
        note.notebook_id = self.state.borrow().default_notebook_id.clone();
        let id = note.id.clone();
        {
            let mut s = self.state.borrow_mut();
            s.load_generation += 1;
            s.filter = Filter::Notebook(s.default_notebook_id.clone());
            s.select_first = false;
            s.reveal_active = true;
        }
        self.search.set_text("");
        self.rebuild_sidebar();
        self.install_draft(note.clone(), false);
        self.display_note(&id, Some(0));
        self.send(Command::Create(note));
        self.request_list();
    }

    fn trash_or_restore(&self) {
        self.flush_drafts();
        let s = self.state.borrow();
        if let Some(d) = s.active.as_ref().and_then(|id| s.drafts.get(id)) {
            self.send(Command::Mutate(if d.note.deleted {
                Mutation::Restore {
                    id: d.note.id.clone(),
                }
            } else {
                Mutation::Trash {
                    id: d.note.id.clone(),
                }
            }));
        }
    }

    fn flush_drafts(&self) {
        let mut s = self.state.borrow_mut();
        for (id, draft) in &mut s.drafts {
            if draft.sequence > draft.queued {
                self.send(Command::Save {
                    id: id.clone(),
                    body: draft.note.body.clone(),
                    sequence: draft.sequence,
                });
                draft.queued = draft.sequence;
                draft.last_queued = Instant::now();
            }
        }
    }

    fn save_preferences(&self) {
        let s = self.state.borrow();
        if !s.ready {
            return;
        }
        self.send(Command::Preferences(Preferences {
            selected_note: s.active.clone(),
            cursor: self.editor.buffer().cursor_position(),
            sidebar_width: self.outer.position(),
            list_width: self.inner.position(),
            width: self.window.width(),
            height: self.window.height(),
        }));
    }

    fn update_word_count(&self) {
        let s = self.state.borrow();
        if let Some(d) = s.active.as_ref().and_then(|id| s.drafts.get(id)) {
            let count = d.note.body.split_whitespace().count();
            self.words.set_text(&format!(
                "{count} {}",
                if count == 1 { "word" } else { "words" }
            ));
        } else {
            self.words.set_text("");
        }
    }

    fn apply_document(&self) {
        let buffer = self.editor.buffer();
        let start = buffer.start_iter();
        let end = buffer.end_iter();
        buffer.remove_all_tags(&start, &end);
        self.state.borrow_mut().hidden_applied.clear();
        for span in &self.state.borrow().document.spans {
            buffer.apply_tag_by_name(
                span.style,
                &buffer.iter_at_offset(span.range.start),
                &buffer.iter_at_offset(span.range.end),
            );
        }
        self.apply_hanging_indents();
        self.update_hidden();
    }

    fn apply_hanging_indents(&self) {
        let buffer = self.editor.buffer();
        let tags = buffer.tag_table();
        for marker in &self.state.borrow().document.list_markers {
            let start = buffer.iter_at_offset(marker.start);
            let end = buffer.iter_at_offset(marker.end);
            let (from, to) = (
                self.editor.iter_location(&start),
                self.editor.iter_location(&end),
            );
            let width = to.x() - from.x();
            if width <= 0 || to.y() != from.y() {
                continue;
            }
            let name = format!("hang-{width}");
            if tags.lookup(&name).is_none() {
                tags.add(
                    &gtk::TextTag::builder()
                        .name(&name)
                        .left_margin(self.editor.left_margin() + width)
                        .indent(-width)
                        .build(),
                );
            }
            buffer.apply_tag_by_name(&name, &start, &end);
        }
    }

    fn update_hidden(&self) {
        let buffer = self.editor.buffer();
        let mut s = self.state.borrow_mut();
        if s.parsed_generation != s.parse_generation {
            return;
        }
        let selection = buffer
            .selection_bounds()
            .map(|(a, b)| a.offset()..b.offset())
            .unwrap_or_else(|| {
                let p = buffer.cursor_position();
                p..p
            });
        let target = s.document.hidden_outside(selection);
        if target == s.hidden_applied {
            return;
        }
        let previous: std::collections::HashSet<_> = s.hidden_applied.iter().cloned().collect();
        let next: std::collections::HashSet<_> = target.iter().cloned().collect();
        for range in previous.difference(&next) {
            buffer.remove_tag_by_name(
                "hidden",
                &buffer.iter_at_offset(range.start),
                &buffer.iter_at_offset(range.end),
            );
        }
        for range in next.difference(&previous) {
            buffer.apply_tag_by_name(
                "hidden",
                &buffer.iter_at_offset(range.start),
                &buffer.iter_at_offset(range.end),
            );
        }
        s.hidden_applied = target;
    }

    fn update_writing_margins(&self) {
        let margin = ((self.editor.width() - 720) / 2).max(48);
        if self.editor.left_margin() != margin {
            self.editor.set_left_margin(margin);
            self.editor.set_right_margin(margin);
            self.placeholder.set_margin_start(margin);
        }
        // TextTag margins replace the TextView margin; they are not relative indents.
        let tags = self.editor.buffer().tag_table();
        for (name, inset) in [("quote", 24), ("code-block", 18)] {
            if let Some(tag) = tags.lookup(name) {
                tag.set_left_margin(margin + inset);
            }
        }
        tags.foreach(|tag| {
            if let Some(width) = tag
                .name()
                .and_then(|name| name.strip_prefix("hang-")?.parse::<i32>().ok())
            {
                tag.set_left_margin(margin + width);
            }
        });
    }

    fn tick(self: &Rc<Self>) {
        if self.editor.left_margin() != ((self.editor.width() - 720) / 2).max(48) {
            self.update_writing_margins();
        }
        while let Ok(event) = self.events.try_recv() {
            self.handle(event);
        }
        while let Ok((generation, document)) = self.parse_results.try_recv() {
            if generation == self.state.borrow().parse_generation {
                {
                    let mut s = self.state.borrow_mut();
                    s.document = document;
                    s.parsed_generation = generation;
                }
                self.apply_document();
            }
        }
        let now = Instant::now();
        if now.duration_since(self.dates_updated.get()) >= Duration::from_secs(60) {
            for item in self
                .note_items
                .borrow()
                .iter()
                .filter_map(|item| item.upgrade())
            {
                update_note_date(&item);
            }
            self.dates_updated.set(now);
        }
        if self.search_due.get().is_some_and(|due| due <= now) {
            self.search_due.set(None);
            self.request_list();
        }
        {
            let mut s = self.state.borrow_mut();
            for (id, d) in &mut s.drafts {
                if d.sequence > d.queued
                    && (now.duration_since(d.changed) >= Duration::from_millis(300)
                        || now.duration_since(d.last_queued) >= Duration::from_secs(2))
                {
                    self.send(Command::Save {
                        id: id.clone(),
                        body: d.note.body.clone(),
                        sequence: d.sequence,
                    });
                    d.queued = d.sequence;
                    d.last_queued = now;
                }
            }
            if s.parse_due.is_some_and(|due| due <= now) {
                s.parse_due = None;
                if let Some(d) = s.active.as_ref().and_then(|id| s.drafts.get(id)) {
                    let _ = self
                        .parse_requests
                        .send((s.parse_generation, d.note.body.clone()));
                }
            }
        }
        if now.duration_since(self.prefs_due.get()) >= Duration::from_secs(2) {
            self.save_preferences();
            self.prefs_due.set(now);
        }
    }

    fn handle(self: &Rc<Self>, event: Event) {
        match event {
            Event::Ready {
                default_notebook_id,
                notebooks,
                note,
                preferences,
            } => {
                let id = note.id.clone();
                {
                    let mut s = self.state.borrow_mut();
                    s.default_notebook_id = default_notebook_id;
                    s.notebooks = notebooks;
                    s.filter = Filter::Notebook(note.notebook_id.clone());
                    s.ready = true;
                }
                self.window.set_default_size(
                    preferences.width.clamp(780, 3000),
                    preferences.height.clamp(480, 2000),
                );
                self.outer
                    .set_position(preferences.sidebar_width.clamp(160, 400));
                self.inner
                    .set_position(preferences.list_width.clamp(220, 600));
                self.install_draft(note, true);
                self.rebuild_sidebar();
                self.display_note(&id, Some(preferences.cursor));
                self.request_list();
            }
            Event::Listed {
                notes,
                counts,
                generation,
            } => {
                if generation != self.state.borrow().list_generation {
                    return;
                }
                self.state.borrow_mut().counts = counts;
                self.update_notebook_counts();
                self.updating.set(true);
                self.list_empty.set_visible(notes.is_empty());
                self.list_empty
                    .set_text(if self.search.text().trim().is_empty() {
                        "No notes here"
                    } else {
                        "No matching notes"
                    });
                let (prefix, suffix, old_len, fallback_position) = {
                    let s = self.state.borrow();
                    let prefix = s
                        .notes
                        .iter()
                        .zip(&notes)
                        .take_while(|(a, b)| a == b)
                        .count();
                    let suffix = s.notes[prefix..]
                        .iter()
                        .rev()
                        .zip(notes[prefix..].iter().rev())
                        .take_while(|(a, b)| a == b)
                        .count();
                    // Keep the deleted note's position so its next neighbor opens;
                    // clamping below falls back to the previous note at the end.
                    let fallback_position = s
                        .active
                        .as_ref()
                        .filter(|id| {
                            s.filter != Filter::Trash
                                && s.drafts.get(*id).is_some_and(|d| d.note.deleted)
                        })
                        .and_then(|id| s.notes.iter().position(|n| &n.id == id))
                        .unwrap_or(0);
                    (prefix, suffix, s.notes.len(), fallback_position)
                };
                let items: Vec<_> = notes[prefix..notes.len() - suffix]
                    .iter()
                    .cloned()
                    .map(glib::BoxedAnyObject::new)
                    .collect();
                let selected = notes
                    .iter()
                    .position(|n| self.state.borrow().active.as_deref() == Some(&n.id));
                if old_len != prefix + suffix || !items.is_empty() {
                    self.note_model.splice(
                        prefix as u32,
                        (old_len - prefix - suffix) as u32,
                        &items,
                    );
                }
                self.selection.set_selected(
                    selected
                        .map(|i| i as u32)
                        .unwrap_or(gtk::INVALID_LIST_POSITION),
                );
                let reveal_active = self.state.borrow().reveal_active;
                if let Some(index) = selected
                    && reveal_active
                {
                    self.note_list
                        .scroll_to(index as u32, gtk::ListScrollFlags::NONE, None);
                    self.state.borrow_mut().reveal_active = false;
                }
                self.count.set_text(&format!(
                    "{} {}",
                    notes.len(),
                    if notes.len() == 1 { "note" } else { "notes" }
                ));
                self.state.borrow_mut().notes = notes;
                self.update_active_marker();
                self.updating.set(false);
                let select_first = std::mem::take(&mut self.state.borrow_mut().select_first);
                if select_first && selected.is_none() {
                    let next = {
                        let s = self.state.borrow();
                        let position = fallback_position.min(s.notes.len().saturating_sub(1));
                        s.notes
                            .get(position)
                            .map(|n| (position as u32, n.id.clone()))
                    };
                    if let Some((position, id)) = next {
                        self.updating.set(true);
                        self.selection.set_selected(position);
                        self.updating.set(false);
                        self.open_note(&id);
                        self.note_list
                            .scroll_to(position, gtk::ListScrollFlags::NONE, None);
                    } else {
                        self.state.borrow_mut().active = None;
                        self.update_active_marker();
                        self.editor_stack.set_visible_child_name("empty");
                        self.update_location();
                        self.update_word_count();
                    }
                }
            }
            Event::Loaded { note, generation } => {
                if generation != self.state.borrow().load_generation {
                    return;
                }
                if let Some(note) = note {
                    let id = note.id.clone();
                    self.install_draft(note, true);
                    self.display_note(&id, None);
                }
            }
            Event::Created(id) => {
                if let Some(d) = self.state.borrow_mut().drafts.get_mut(&id) {
                    d.created = true;
                }
                self.request_list();
                self.update_word_count();
            }
            Event::Saved { id, sequence } => {
                {
                    let mut s = self.state.borrow_mut();
                    if let Some(d) = s.drafts.get_mut(&id) {
                        d.saved = d.saved.max(sequence);
                    }
                    s.failed.retain(|c| !matches!(c, Command::Save { id: failed_id, sequence: failed_seq, .. } if failed_id == &id && *failed_seq <= sequence));
                    if s.failed.is_empty() {
                        self.error_box.set_visible(false);
                    }
                }
                self.update_word_count();
                self.request_list();
                self.trim_cache();
            }
            Event::Mutated {
                mutation,
                notebooks,
            } => {
                {
                    let mut s = self.state.borrow_mut();
                    s.notebooks = notebooks;
                    match mutation {
                        Mutation::Move { id, notebook_id } => {
                            if let Some(d) = s.drafts.get_mut(&id) {
                                d.note.notebook_id = notebook_id;
                            }
                        }
                        Mutation::Trash { id } => {
                            if let Some(d) = s.drafts.get_mut(&id) {
                                d.note.deleted = true;
                            }
                            if s.active.as_ref() == Some(&id) && s.filter != Filter::Trash {
                                s.select_first = true;
                                s.load_generation += 1;
                            }
                        }
                        Mutation::Restore { id } => {
                            if let Some(d) = s.drafts.get_mut(&id) {
                                d.note.deleted = false;
                            }
                        }
                        Mutation::DeleteNotebook { id } => {
                            let default_id = s.default_notebook_id.clone();
                            for d in s.drafts.values_mut() {
                                if d.note.notebook_id == id {
                                    d.note.notebook_id = default_id.clone();
                                }
                            }
                            if s.filter == Filter::Notebook(id) {
                                s.filter = Filter::Notebook(default_id);
                            }
                        }
                        _ => {}
                    }
                    if let Some(d) = s.active.as_ref().and_then(|id| s.drafts.get(id)) {
                        self.editor.set_editable(!d.note.deleted);
                    }
                }
                self.rebuild_sidebar();
                self.request_list();
                self.update_word_count();
            }
            Event::Flushed => {
                if self.state.borrow().closing && self.state.borrow().failed.is_empty() {
                    self.allow_close.set(true);
                    self.window.close();
                }
            }
            Event::Error { command, message } => {
                self.window.set_sensitive(true);
                let mut s = self.state.borrow_mut();
                s.closing = false;
                if matches!(
                    command,
                    Command::Save { .. }
                        | Command::Create(_)
                        | Command::Initialize
                        | Command::Preferences(_)
                ) {
                    s.failed
                        .retain(|old| failure_key(old) != failure_key(&command));
                    s.failed.push(command);
                }
                self.retry.set_label(if s.failed.is_empty() {
                    "Dismiss"
                } else {
                    "Retry"
                });
                self.error_label.set_text(&format!(
                    "Could not save or load: {message}. Your open notes are kept in memory."
                ));
                self.error_box.set_visible(true);
                drop(s);
                self.update_word_count();
            }
        }
    }

    fn notebook_dialog(self: &Rc<Self>, rename: bool) {
        let current = {
            let s = self.state.borrow();
            match &s.filter {
                Filter::Notebook(id) if id != &s.default_notebook_id => {
                    s.notebooks.iter().find(|b| b.id == *id).cloned()
                }
                _ => None,
            }
        };
        if rename && current.is_none() {
            return;
        }
        self.show_notebook_dialog(if rename { current } else { None });
    }

    fn show_notebook_dialog(self: &Rc<Self>, current: Option<Notebook>) {
        let rename = current.is_some();
        let dialog = gtk::Window::builder()
            .title(if rename {
                "Rename notebook"
            } else {
                "New notebook"
            })
            .transient_for(&self.window)
            .modal(true)
            .resizable(false)
            .default_width(340)
            .build();
        dialog.add_css_class("notebook-app");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
        content.set_margin_top(24);
        content.set_margin_bottom(24);
        content.set_margin_start(24);
        content.set_margin_end(24);
        let entry = gtk::Entry::builder()
            .placeholder_text("Notebook name")
            .max_length(100)
            .build();
        if let Some(book) = &current {
            entry.set_text(&book.name);
            entry.select_region(0, -1);
        }
        content.append(&entry);
        let button = gtk::Button::with_label(if rename { "Rename" } else { "Create notebook" });
        button.add_css_class("suggested-action");
        content.append(&button);
        dialog.set_child(Some(&content));
        let weak = Rc::downgrade(self);
        let d = dialog.downgrade();
        let e = entry.clone();
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let name = e.text().trim().to_string();
                if name.is_empty() {
                    return;
                }
                ui.send(Command::Mutate(if let Some(book) = &current {
                    Mutation::RenameNotebook {
                        id: book.id.clone(),
                        name,
                    }
                } else {
                    Mutation::CreateNotebook {
                        id: uuid::Uuid::new_v4().to_string(),
                        name,
                    }
                }));
                if let Some(dialog) = d.upgrade() {
                    dialog.close();
                }
                ui.window.present();
                ui.editor.grab_focus();
            }
        });
        let button = button.downgrade();
        entry.connect_activate(move |_| {
            if let Some(button) = button.upgrade() {
                button.emit_clicked();
            }
        });
        dialog.present();
        entry.grab_focus();
    }
}

fn note_date(date: &glib::DateTime, now: &glib::DateTime) -> String {
    let age = now.to_unix() - date.to_unix();
    if (0..60).contains(&age) {
        return "Now".into();
    }
    let day = date.format("%Y-%m-%d").unwrap_or_default();
    if day == now.format("%Y-%m-%d").unwrap_or_default() {
        return "Today".into();
    }
    for days in 1..=6 {
        if now
            .add_days(-days)
            .is_ok_and(|past| past.format("%Y-%m-%d").unwrap_or_default() == day)
        {
            return if days == 1 {
                "Yesterday".into()
            } else {
                date.format("%a").unwrap_or_default().to_string()
            };
        }
    }
    date.format(if date.year() == now.year() {
        "%b %-d"
    } else {
        "%b %-d, %Y"
    })
    .unwrap_or_default()
    .to_string()
}

fn update_note_date(item: &gtk::ListItem) {
    let Some(object) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
        return;
    };
    let note = object.borrow::<NoteSummary>();
    let Some(label) = item
        .child()
        .and_then(|row| row.first_child())
        .and_then(|heading| heading.last_child())
        .and_downcast::<gtk::Label>()
    else {
        return;
    };
    if let (Ok(date), Ok(now)) = (
        glib::DateTime::from_unix_local(note.updated_at / 1000),
        glib::DateTime::now_local(),
    ) {
        let text = note_date(&date, &now);
        if label.text() != text {
            label.set_text(&text);
        }
        label.set_tooltip_text(Some(&format!(
            "Last edited: {}",
            date.format("%c").unwrap_or_default()
        )));
    } else {
        label.set_text("");
        label.set_tooltip_text(None);
    }
}

fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(tooltip));
    button.add_css_class("flat");
    button
}

fn failure_key(command: &Command) -> String {
    match command {
        Command::Save { id, .. } => format!("save:{id}"),
        Command::Create(note) => format!("create:{}", note.id),
        Command::Initialize => "initialize".into(),
        Command::Preferences(_) => "preferences".into(),
        _ => "other".into(),
    }
}

fn configure_tags(buffer: &gtk::TextBuffer) {
    let table = buffer.tag_table();
    for (name, scale) in [("h1", 1.875), ("h2", 1.3), ("h3", 1.12)] {
        table.add(
            &gtk::TextTag::builder()
                .name(name)
                .scale(scale)
                .weight(700)
                .pixels_above_lines(0)
                .pixels_below_lines(0)
                .build(),
        );
    }
    table.add(&gtk::TextTag::builder().name("strong").weight(700).build());
    table.add(
        &gtk::TextTag::builder()
            .name("list-item-start")
            .pixels_above_lines(3)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("emphasis")
            .style(pango::Style::Italic)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("strike")
            .strikethrough(true)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("quote")
            .style(pango::Style::Italic)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("code")
            .family("monospace")
            .scale(0.92)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("code-block")
            .family("monospace")
            .scale(0.92)
            .pixels_above_lines(0)
            .pixels_below_lines(0)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("link")
            .underline(pango::Underline::Single)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("task")
            .family("monospace")
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("checked")
            .family("monospace")
            .strikethrough(true)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("hidden")
            .invisible(true)
            .build(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_use_calendar_days_and_include_older_years() {
        let date = |value| glib::DateTime::from_iso8601(value, None).unwrap();
        let now = date("2026-09-18T00:01:00Z");
        assert_eq!(note_date(&date("2026-09-18T00:00:45Z"), &now), "Now");
        assert_eq!(note_date(&date("2026-09-18T00:00:00Z"), &now), "Today");
        assert_eq!(note_date(&date("2026-09-17T23:59:00Z"), &now), "Yesterday");
        let monday = date("2026-09-14T12:00:00Z");
        assert_eq!(
            note_date(&monday, &now),
            monday.format("%a").unwrap().as_str()
        );
        let older = date("2026-09-01T12:00:00Z");
        assert_eq!(
            note_date(&older, &now),
            older.format("%b %-d").unwrap().as_str()
        );
        assert!(note_date(&date("2025-12-31T12:00:00Z"), &now).contains("2025"));
        assert_eq!(
            note_date(&date("2025-12-31T12:00:00Z"), &date("2026-01-01T01:00:00Z")),
            "Yesterday"
        );
    }

    fn pump_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        let context = glib::MainContext::default();
        loop {
            for _ in 0..100 {
                if !context.pending() {
                    break;
                }
                context.iteration(false);
            }
            if predicate() {
                break;
            }
            assert!(Instant::now() < deadline, "UI condition timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    #[ignore = "requires a display; run under Xvfb"]
    fn desktop_workflow() {
        gtk::init().unwrap();
        let app = gtk::Application::builder()
            .application_id("io.github.pkkulhari.Notebook.Tests")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.db");
        let ui = build(&app, path.clone());
        pump_until(|| ui.state.borrow().ready && ui.editor.has_focus());
        assert!(ui.placeholder.is_visible());
        let first = ui.state.borrow().active.clone().unwrap();
        assert_eq!(
            ui.state.borrow().drafts[&first].note.notebook_id,
            DEFAULT_NOTEBOOK
        );
        assert!(
            ui.editor
                .buffer()
                .text(
                    &ui.editor.buffer().start_iter(),
                    &ui.editor.buffer().end_iter(),
                    true
                )
                .is_empty()
        );

        let buffer = ui.editor.buffer();
        buffer.begin_user_action();
        buffer.insert_at_cursor("# Hello 🌿\n\n**A thought** and _another_.\n\nLast line");
        buffer.end_user_action();
        assert!(!ui.placeholder.is_visible());
        pump_until(|| {
            ui.state.borrow().drafts[&first].saved > 0
                && ui.state.borrow().parsed_generation == ui.state.borrow().parse_generation
                && ui
                    .state
                    .borrow()
                    .notes
                    .first()
                    .is_some_and(|n| n.label == "Hello 🌿")
        });
        assert_eq!(ui.state.borrow().notes[0].label, "Hello 🌿");
        let hidden = buffer.tag_table().lookup("hidden").unwrap();
        assert!(buffer.start_iter().has_tag(&hidden));
        if let Some(directory) = std::env::var_os("NOTEBOOK_TEST_SCREENSHOTS") {
            let directory = std::path::PathBuf::from(directory);
            std::fs::create_dir_all(&directory).unwrap();
            let wait = Instant::now() + Duration::from_millis(100);
            pump_until(|| Instant::now() >= wait);
            screenshot(&ui, &directory.join("notebook-light.png"));
            let settings = gtk::Settings::default().unwrap();
            settings.set_gtk_application_prefer_dark_theme(true);
            let wait = Instant::now() + Duration::from_millis(150);
            pump_until(|| Instant::now() >= wait);
            screenshot(&ui, &directory.join("notebook-dark.png"));
            settings.set_gtk_application_prefer_dark_theme(false);
        }
        buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
        assert!(!buffer.start_iter().has_tag(&hidden));
        ui.editor.emit_by_name::<()>("copy-clipboard", &[]);
        let clipboard = Rc::new(RefCell::new(None));
        let result = clipboard.clone();
        ui.editor
            .clipboard()
            .read_text_async(None::<&gio::Cancellable>, move |text| {
                *result.borrow_mut() = Some(text.unwrap().unwrap().to_string())
            });
        pump_until(|| clipboard.borrow().is_some());
        assert!(
            clipboard
                .borrow()
                .as_ref()
                .unwrap()
                .contains("**A thought**")
        );
        buffer.place_cursor(&buffer.end_iter());
        buffer.begin_user_action();
        buffer.insert_at_cursor("!");
        buffer.end_user_action();
        buffer.undo();
        assert!(
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .ends_with("Last line")
        );
        buffer.redo();
        assert!(
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .ends_with("Last line!")
        );

        ui.send(Command::Mutate(Mutation::CreateNotebook {
            id: "work".into(),
            name: "Work".into(),
        }));
        pump_until(|| ui.state.borrow().notebooks.len() == 2);
        // Row actions target that notebook, even while Default is selected.
        gtk::prelude::WidgetExt::activate_action(
            &ui.window,
            "win.rename-book",
            Some(&"work".to_variant()),
        )
        .unwrap();
        let dialog = gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
            .find(|w| w.title().as_deref() == Some("Rename notebook"))
            .unwrap();
        let entry = dialog
            .child()
            .unwrap()
            .first_child()
            .unwrap()
            .downcast::<gtk::Entry>()
            .unwrap();
        assert_eq!(entry.text(), "Work");
        entry.set_text("Projects");
        entry.emit_activate();
        pump_until(|| {
            ui.state
                .borrow()
                .notebooks
                .iter()
                .any(|n| n.name == "Projects")
        });
        assert_eq!(
            ui.state.borrow().filter,
            Filter::Notebook(DEFAULT_NOTEBOOK.into())
        );
        ui.sidebar.select_row(ui.sidebar.row_at_index(2).as_ref());
        pump_until(|| ui.editor_stack.visible_child_name().as_deref() == Some("empty"));
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.new-note", None).unwrap();
        let second = ui.state.borrow().active.clone().unwrap();
        assert_ne!(first, second);
        assert_eq!(
            ui.state.borrow().filter,
            Filter::Notebook(DEFAULT_NOTEBOOK.into())
        );
        assert!(ui.editor.has_focus());
        ui.editor.buffer().insert_at_cursor("A second note");
        ui.open_note(&first);
        ui.editor.buffer().insert_at_cursor(" Latest");
        ui.open_note(&second);
        pump_until(|| {
            ui.state
                .borrow()
                .drafts
                .values()
                .all(|d| d.sequence == d.saved && d.created)
        });

        // Hover-driven selection must not replace the note being edited.
        pump_until(|| ui.state.borrow().notes.iter().any(|n| n.id == first));
        let first_position = ui
            .state
            .borrow()
            .notes
            .iter()
            .position(|n| n.id == first)
            .unwrap() as u32;
        ui.selection.set_selected(first_position);
        assert_eq!(ui.state.borrow().active.as_deref(), Some(second.as_str()));
        let active_rows = || {
            ui.note_items
                .borrow()
                .iter()
                .filter_map(|weak| weak.upgrade())
                .filter(|item| {
                    item.child()
                        .is_some_and(|child| child.has_css_class("active-note"))
                })
                .map(|item| item.position())
                .collect::<Vec<_>>()
        };
        let second_position = ui
            .state
            .borrow()
            .notes
            .iter()
            .position(|n| n.id == second)
            .unwrap() as u32;
        assert!(active_rows().contains(&second_position));
        assert!(!active_rows().contains(&first_position));
        ui.note_list
            .emit_by_name::<()>("activate", &[&first_position]);
        pump_until(|| ui.state.borrow().active.as_deref() == Some(first.as_str()));
        assert!(active_rows().contains(&first_position));
        assert!(!active_rows().contains(&second_position));
        assert!(ui.editor.has_focus());
        ui.open_note(&second);

        ui.search.set_text("Latest");
        pump_until(|| ui.state.borrow().notes.len() == 1 && ui.state.borrow().notes[0].id == first);
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.new-note", None).unwrap();
        assert!(ui.search.text().is_empty());
        ui.open_note(&second);
        ui.trash_or_restore();
        pump_until(|| ui.state.borrow().active.as_deref() != Some(second.as_str()));
        assert!(ui.editor.is_editable());
        // Reopen the trashed note explicitly to exercise restoration.
        ui.open_note(&second);
        assert!(!ui.editor.is_editable());
        ui.trash_or_restore();
        pump_until(|| ui.editor.is_editable());

        let inspection = rusqlite::Connection::open(&path).unwrap();
        inspection.execute_batch("CREATE TRIGGER fail_save BEFORE UPDATE OF body ON notes BEGIN SELECT RAISE(ABORT, 'test failure'); END").unwrap();
        ui.editor.buffer().insert_at_cursor(" unsaved");
        pump_until(|| !ui.state.borrow().failed.is_empty());
        assert!(ui.error_box.is_visible());
        ui.editor.buffer().insert_at_cursor(" newest");
        assert!(ui.error_box.is_visible());
        inspection.execute_batch("DROP TRIGGER fail_save").unwrap();
        ui.retry.emit_clicked();
        pump_until(|| {
            ui.state.borrow().failed.is_empty()
                && ui.state.borrow().drafts[&second].saved
                    == ui.state.borrow().drafts[&second].sequence
        });
        let stored: String = inspection
            .query_row("SELECT body FROM notes WHERE id=?1", [&second], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(stored.ends_with("unsaved newest"));
        gtk::prelude::WidgetExt::activate_action(
            &ui.window,
            "win.delete-book",
            Some(&"work".to_variant()),
        )
        .unwrap();
        pump_until(|| ui.state.borrow().notebooks.len() == 1);
        assert_eq!(ui.state.borrow().active.as_deref(), Some(second.as_str()));

        let block_sample = "Ordinary paragraph\n\n> A quoted paragraph\n\n```rust\nfn main() {}\n```\n\n- [ ] Record real robot rollout data and run the same trajectories in the bench X12 actuator at the same frequency, then compare the logged currents\n\nEnd";
        ui.new_note();
        let blocks = ui.state.borrow().active.clone().unwrap();
        ui.editor.buffer().insert_at_cursor(block_sample);
        pump_until(|| ui.state.borrow().parsed_generation == ui.state.borrow().parse_generation);
        assert_block_alignment(&ui, block_sample);
        let normal_margin = ui.editor.left_margin();
        // Resize with another buffer open, then return to the cached Markdown note.
        ui.open_note(&second);
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.focus", None).unwrap();
        pump_until(|| ui.editor.left_margin() > normal_margin);
        ui.open_note(&blocks);
        pump_until(|| ui.state.borrow().parsed_generation == ui.state.borrow().parse_generation);
        assert_block_alignment(&ui, block_sample);
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.focus", None).unwrap();
        pump_until(|| ui.editor.left_margin() == normal_margin);
        assert_block_alignment(&ui, block_sample);
        ui.open_note(&second);

        ui.window.close();
        pump_until(|| !ui.window.is_visible());
        let mut repo = storage::Repository::open(&path).unwrap();
        let (_, note, preferences) = repo.bootstrap().unwrap();
        assert_eq!(note.id, second);
        assert_eq!(note.body, stored);
        assert!(preferences.cursor > 0);
        deletion_navigation(&app);
        if std::env::var_os("NOTEBOOK_BENCH_UI").is_some() {
            benchmark_ui(&app);
        }
        if let Some(directory) = std::env::var_os("NOTEBOOK_TEST_SCREENSHOTS") {
            design_preview(&app, std::path::Path::new(&directory));
        }
    }

    fn deletion_navigation(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let ui = build(app, dir.path().join("deletion.db"));
        pump_until(|| ui.state.borrow().ready && ui.state.borrow().notes.len() == 1);
        for count in 2..=4 {
            ui.new_note();
            pump_until(|| ui.state.borrow().notes.len() == count);
        }
        let ids: Vec<_> = ui
            .state
            .borrow()
            .notes
            .iter()
            .map(|n| n.id.clone())
            .collect();
        // Middle -> next, last -> previous, first -> next, only -> empty.
        for (deleted, expected, remaining) in [
            (&ids[1], Some(&ids[2]), 3),
            (&ids[3], Some(&ids[2]), 2),
            (&ids[0], Some(&ids[2]), 1),
            (&ids[2], None, 0),
        ] {
            ui.open_note(deleted);
            pump_until(|| ui.state.borrow().active.as_ref() == Some(deleted));
            ui.trash_or_restore();
            pump_until(|| {
                let s = ui.state.borrow();
                s.notes.len() == remaining && s.active.as_ref() == expected
            });
            if expected.is_some() {
                assert!(ui.editor.is_editable());
                assert!(ui.editor.has_focus());
                assert_eq!(
                    ui.editor_stack.visible_child_name().as_deref(),
                    Some("editor")
                );
            } else {
                assert_eq!(
                    ui.editor_stack.visible_child_name().as_deref(),
                    Some("empty")
                );
            }
            let counts = ui.sidebar_counts.borrow();
            let count_for =
                |filter: Filter| counts.iter().find(|(f, _)| *f == filter).unwrap().1.text();
            assert_eq!(
                count_for(Filter::Notebook(DEFAULT_NOTEBOOK.into())),
                remaining.to_string()
            );
            assert_eq!(count_for(Filter::Trash), (4 - remaining).to_string());
        }
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
    }

    fn assert_block_alignment(ui: &Ui, source: &str) {
        let buffer = ui.editor.buffer();
        let x = |text: &str| {
            let offset = source[..source.find(text).unwrap()].chars().count() as i32;
            ui.editor.iter_location(&buffer.iter_at_offset(offset)).x()
        };
        let prose = x("Ordinary");
        for (name, left) in [("quote", x("> A")), ("code", x("fn main"))] {
            assert!(
                (0..=32).contains(&(left - prose)),
                "{name} starts at {left}, outside prose column at {prose}"
            );
        }
        let item = source.find("Record").unwrap();
        let content = source[..item].chars().count() as i32;
        let first = ui.editor.iter_location(&buffer.iter_at_offset(content));
        let wrapped = (content..content + source[item..].find('\n').unwrap() as i32)
            .map(|offset| ui.editor.iter_location(&buffer.iter_at_offset(offset)))
            .find(|location| location.y() > first.y())
            .expect("task item should wrap");
        assert_eq!(wrapped.x(), first.x(), "wrapped task line is not aligned");
        assert_eq!(
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), true),
            source
        );
    }

    fn design_preview(app: &gtk::Application, directory: &std::path::Path) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preview.db");
        let mut repo = storage::Repository::open(&path).unwrap();
        for name in ["Personal", "Work"] {
            repo.mutate(&Mutation::CreateNotebook {
                id: name.into(),
                name: name.into(),
            })
            .unwrap();
        }
        for body in [
            "Weekend notes\nA long walk. A good book. No particular plans.",
            "Things worth keeping\nSmall ideas, before they slip away.",
            "A reading list\nBooks to come back to this autumn.",
        ] {
            let mut note = Note::blank();
            note.body = body.into();
            repo.create_note(&note).unwrap();
        }
        let mut note = Note::blank();
        note.body = "# A little room to think\n\nSome days, all you need is a quiet place to put things down. No structure to get right. Just a thought, and the next one.\n\n## Keep it simple\n\n- Write while the idea is still fresh.\n- Make space for **what matters**.\n- Come back to the rest later.\n\nA notebook can be a place to figure things out, not just a place to keep the answers.\n\nOne thought at a time.".into();
        repo.create_note(&note).unwrap();
        repo.save_preferences(&Preferences {
            selected_note: Some(note.id),
            cursor: note.body.chars().count() as i32,
            width: 1200,
            height: 800,
            sidebar_width: 192,
            list_width: 290,
        })
        .unwrap();
        drop(repo);
        let ui = build(app, path);
        pump_until(|| {
            ui.state.borrow().ready
                && ui.state.borrow().notes.len() == 4
                && ui.state.borrow().parsed_generation == ui.state.borrow().parse_generation
        });
        let settings = gtk::Settings::default().unwrap();
        for (dark, file) in [(false, "notebook-light.png"), (true, "notebook-dark.png")] {
            settings.set_gtk_application_prefer_dark_theme(dark);
            let wait = Instant::now() + Duration::from_millis(400);
            pump_until(|| Instant::now() >= wait);
            screenshot(&ui, &directory.join(file));
        }
        ui.new_note();
        pump_until(|| ui.state.borrow().notes.len() == 5);
        let wait = Instant::now() + Duration::from_millis(100);
        pump_until(|| Instant::now() >= wait);
        screenshot(&ui, &directory.join("notebook-empty.png"));
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
        settings.set_gtk_application_prefer_dark_theme(false);
    }

    fn screenshot(ui: &Ui, path: &std::path::Path) {
        let paintable = gtk::WidgetPaintable::new(Some(&ui.window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            &snapshot,
            ui.window.width() as f64,
            ui.window.height() as f64,
        );
        let node = snapshot.to_node().unwrap();
        let texture = ui.window.renderer().unwrap().render_texture(&node, None);
        texture.save_to_png(path).unwrap();
    }

    fn benchmark_ui(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench.db");
        let mut repo = storage::Repository::open(&path).unwrap();
        for i in 0..10_000 {
            let mut note = Note::blank();
            note.body = format!("# Note {i}\n\nA thought about gardens and writing.");
            repo.create_note(&note).unwrap();
        }
        let mut note = Note::blank();
        note.body = "# A working document\n\n".to_string()
            + &"A **bold** thought with _emphasis_, `code`, and [a link](https://example.org).\n\n"
                .repeat(1_350);
        repo.create_note(&note).unwrap();
        repo.save_preferences(&Preferences {
            selected_note: Some(note.id.clone()),
            ..Default::default()
        })
        .unwrap();
        drop(repo);
        let start = Instant::now();
        let ui = build(app, path);
        pump_until(|| {
            ui.state.borrow().ready
                && ui.state.borrow().parsed_generation > 0
                && ui.state.borrow().parsed_generation == ui.state.borrow().parse_generation
                && !ui.state.borrow().notes.is_empty()
        });
        println!(
            "GUI: create window, restore 108 KB note, load 10,000 summaries, apply Markdown: {:.2?}",
            start.elapsed()
        );
        let start = Instant::now();
        ui.apply_document();
        println!("GUI: apply Markdown presentation: {:.2?}", start.elapsed());
        let buffer = ui.editor.buffer();
        buffer.place_cursor(&buffer.end_iter());
        let frame_clock = ui.editor.frame_clock().unwrap();
        let pending = Rc::new(Cell::new(None::<Instant>));
        let times = Rc::new(RefCell::new(Vec::new()));
        let p = pending.clone();
        let t = times.clone();
        let handler = frame_clock.connect_after_paint(move |_| {
            if let Some(start) = p.take() {
                t.borrow_mut().push(start.elapsed());
            }
        });
        for _ in 0..30 {
            pending.set(Some(Instant::now()));
            buffer.insert_at_cursor("x");
            ui.editor.queue_draw();
            pump_until(|| pending.get().is_none());
        }
        frame_clock.disconnect(handler);
        let mut timings = times.borrow().clone();
        timings.sort();
        println!(
            "GUI: input-to-paint median {:.2?}, p95 {:.2?} (virtual display)",
            timings[15], timings[28]
        );
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
    }
}
