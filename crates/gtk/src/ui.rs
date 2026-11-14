use crate::{
    formatting::{self, Block, Format},
    link_prompt::{LinkPrompt, LinkTarget},
    slash_menu::{self, Action as MenuAction, SlashMenu},
    task_checkboxes::TaskCheckboxes,
};
use gtk::{gdk, gio, glib, pango, prelude::*};
use notebook_core::{
    editor::{Applied, Draft, Drafts, Failures, OutOfSync, TextEdit},
    markdown::{self, Document},
    model::*,
    storage::{self, Command, Event, Mutation},
    sync::{self, Control, Pairing, SyncHandle},
};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};

/// The buffer showing an open draft. It mirrors the draft's text: typing is
/// written into the draft, and the draft's imports and undos into the buffer.
struct Page {
    buffer: gtk::TextBuffer,
    /// The buffer's text as of its last change.
    body: String,
    /// Source before the current native edit, for repairing hidden delimiters
    /// once GTK has finished using its insertion/deletion iterators.
    before_edit: Option<String>,
}

struct State {
    default_notebook_id: String,
    notebooks: Vec<Notebook>,
    notes: Vec<NoteSummary>,
    counts: NoteCounts,
    drafts: Drafts,
    pages: HashMap<String, Page>,
    active: Option<String>,
    filter: Filter,
    list_generation: u64,
    load_generation: u64,
    parse_generation: u64,
    parsed_generation: u64,
    document: Document,
    hidden_applied: Vec<std::ops::Range<i32>>,
    failed: Failures,
    closing: bool,
    ready: bool,
    select_first: bool,
    reveal_active: bool,
}

impl State {
    fn leave_missing_notebook(&mut self) {
        if matches!(&self.filter, Filter::Notebook(id) if !self.notebooks.iter().any(|b| &b.id == id))
        {
            self.filter = Filter::Notebook(self.default_notebook_id.clone());
        }
    }
}

struct SyncDialog {
    window: gtk::Window,
    /// Set while the widgets are updated from status, so they don't echo back.
    quiet: Rc<Cell<bool>>,
    enabled: gtk::Switch,
    name: gtk::Entry,
    devices: gtk::Box,
    pairing: gtk::Stack,
    pairing_box: gtk::Box,
    pairing_note: gtk::Label,
    code: gtk::Label,
    relay: gtk::Entry,
    problem: gtk::Label,
    /// The ID, name, and state shown for each paired device.
    rows: RefCell<Vec<[String; 3]>>,
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
    task_checkboxes: TaskCheckboxes,
    slash: SlashMenu,
    link: LinkPrompt,
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
    state: RefCell<State>,
    sync: SyncHandle,
    sync_statuses: Receiver<sync::Status>,
    sync_status: RefCell<sync::Status>,
    sync_button: gtk::Button,
    sync_dialog: RefCell<Option<SyncDialog>>,
    updating: Cell<bool>,
    /// The buffer is being changed from its document, not by typing.
    from_doc: Cell<bool>,
    allow_close: Cell<bool>,
    search_due: Cell<Option<Instant>>,
    selection_due: Cell<Option<Instant>>,
    prefs_due: Cell<Instant>,
    dates_updated: Cell<Instant>,
}

impl Drop for Ui {
    fn drop(&mut self) {
        self.slash.popover.unparent();
        self.link.popover.unparent();
    }
}

pub(crate) fn launch(app: &gtk::Application) -> Rc<Ui> {
    build(
        app,
        crate::system::data_path(),
        crate::system::config_path(),
        crate::system::device_name(),
        sync::Options::default(),
    )
}

fn build(
    app: &gtk::Application,
    path: std::path::PathBuf,
    sync_config: std::path::PathBuf,
    device_name: String,
    sync_options: sync::Options,
) -> Rc<Ui> {
    let (commands, events) = storage::spawn_worker(path.clone());
    let (statuses, sync_statuses) = mpsc::channel();
    let sync = sync::spawn(
        sync_config,
        path,
        device_name,
        commands.clone(),
        move |status| {
            let _ = statuses.send(status);
        },
        sync_options,
    );
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
    menu.append(Some("Sync devices"), Some("win.sync"));
    menu.append(Some("Keyboard shortcuts"), Some("win.shortcuts"));
    let menu_button = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .tooltip_text("Notebook menu")
        .build();
    menu_button.add_css_class("quiet-menu");
    header.pack_end(&menu_button);
    let sync_button = icon_button(
        "emblem-synchronizing-symbolic",
        "Sync with your other devices",
    );
    sync_button.set_action_name(Some("win.sync"));
    header.pack_end(&sync_button);
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
    let version = gtk::Label::new(Some(concat!("Notebook v", env!("CARGO_PKG_VERSION"))));
    version.add_css_class("footer");
    version.set_xalign(0.0);
    sidebar_box.append(&version);
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
    let trash_button = icon_button("user-trash-symbolic", "Move to Trash · Ctrl+Shift+Delete");
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
    let slash = SlashMenu::new(&editor);
    let link = LinkPrompt::new(&editor);
    let formats = gio::Menu::new();
    formats.append(Some("Bold"), Some("win.bold"));
    formats.append(Some("Italic"), Some("win.italic"));
    formats.append(Some("Strikethrough"), Some("win.strike"));
    formats.append(Some("Link…"), Some("win.link"));
    editor.set_extra_menu(Some(&formats));
    let editor_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&editor)
        .build();
    let editor_stack = gtk::Stack::new();
    editor_stack.set_vexpand(true);
    let editor_overlay = gtk::Overlay::new();
    editor_overlay.set_child(Some(&editor_scroll));
    let placeholder = gtk::Label::new(Some("Start writing, or type / for commands…"));
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
    editor_footer.append(&gtk::Label::new(Some("Type / for commands")));
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
        task_checkboxes: TaskCheckboxes::default(),
        slash,
        link,
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
        sync,
        sync_statuses,
        sync_status: RefCell::new(sync::Status::default()),
        sync_button,
        sync_dialog: RefCell::new(None),
        updating: Cell::new(false),
        from_doc: Cell::new(false),
        allow_close: Cell::new(false),
        search_due: Cell::new(None),
        selection_due: Cell::new(None),
        prefs_due: Cell::new(Instant::now()),
        dates_updated: Cell::new(Instant::now()),
        state: RefCell::new(State {
            default_notebook_id: DEFAULT_NOTEBOOK.into(),
            notebooks: vec![],
            notes: vec![],
            counts: NoteCounts::new(),
            drafts: Drafts::new(Units::Chars),
            pages: HashMap::new(),
            active: None,
            filter: Filter::Notebook(DEFAULT_NOTEBOOK.into()),
            list_generation: 0,
            load_generation: 0,
            parse_generation: 0,
            parsed_generation: 0,
            document: Document::default(),
            hidden_applied: vec![],
            failed: Failures::default(),
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
        let weak = Rc::downgrade(self);
        self.task_checkboxes
            .connect_toggled(move |buffer, offset, checked| {
                if let Some(ui) = weak.upgrade() {
                    ui.set_task_checked(buffer, offset, checked);
                }
            });
        let weak = Rc::downgrade(self);
        self.editor.connect_editable_notify(move |editor| {
            if let Some(ui) = weak.upgrade() {
                ui.task_checkboxes.set_editable(editor.is_editable());
            }
        });
        let weak = Rc::downgrade(self);
        self.slash.list.connect_row_activated(move |_, row| {
            if let Some(ui) = weak.upgrade()
                && let Some(command) = slash_menu::COMMANDS.get(row.index() as usize)
            {
                ui.run_menu_action(command.action);
            }
        });
        let weak = Rc::downgrade(self);
        self.link.entry.connect_activate(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.apply_link();
            }
        });
        let weak = Rc::downgrade(self);
        self.link.popover.connect_closed(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.link.target.borrow_mut().take();
                if ui.window.is_visible() {
                    ui.editor.grab_focus();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.editor.connect_has_focus_notify(move |editor| {
            if !editor.has_focus()
                && let Some(ui) = weak.upgrade()
            {
                ui.dismiss_format_menu();
            }
        });
        let dismiss = gtk::GestureClick::new();
        dismiss.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        dismiss.connect_pressed(move |_, _, x, y| {
            if let Some(ui) = weak.upgrade()
                && (ui.slash.popover.is_visible() || ui.selection_due.get().is_some())
            {
                let in_menu = ui
                    .window
                    .pick(x, y, gtk::PickFlags::DEFAULT)
                    .is_some_and(|target| {
                        target == ui.slash.popover || target.is_ancestor(&ui.slash.popover)
                    });
                if !in_menu {
                    ui.dismiss_format_menu();
                }
            }
        });
        self.window.add_controller(dismiss);
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
        self.action("undo", |ui| ui.undo_redo(true));
        self.action("redo", |ui| ui.undo_redo(false));
        for (action, format) in [
            ("bold", Format::Bold),
            ("italic", Format::Italic),
            ("strike", Format::Strike),
            ("bullets", Format::Bullets),
            ("numbers", Format::Numbers),
        ] {
            self.action(action, move |ui| ui.format_selection(format));
        }
        self.action("link", |ui| ui.prompt_link());
        self.action("save", |ui| ui.flush_drafts());
        self.action("sync", |ui| ui.show_sync_dialog());
        self.action("shortcuts", |ui| {
            let dialog = gtk::Window::builder().title("Keyboard shortcuts").transient_for(&ui.window).modal(true).default_width(420).build();
            let label = gtk::Label::new(Some("New note                 Ctrl+N\nSearch all notes         Ctrl+F\nFocus writing            F9\nSave now                 Ctrl+S\nUndo                     Ctrl+Z\nRedo                     Ctrl+Shift+Z\nBlock menu               Type /\nChoose block             ↑↓ then Enter or Tab\nBold selection           Ctrl+B\nItalic selection         Ctrl+I\nStrikethrough selection  Ctrl+Shift+X\nLink selection           Ctrl+K\nBulleted list            Ctrl+Shift+7\nNumbered list            Ctrl+Shift+8\nTrash / restore          Ctrl+Shift+Delete\nOpen link                Ctrl+click\nClose menu / editor      Escape"));
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
            ("bold", vec!["<Primary>b"]),
            ("italic", vec!["<Primary>i"]),
            ("strike", vec!["<Primary><Shift>x"]),
            ("link", vec!["<Primary>k"]),
            ("bullets", vec!["<Primary><Shift>7"]),
            ("numbers", vec!["<Primary><Shift>8"]),
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
                    .link_at(iter.offset())
                    .map(str::to_owned);
                if let Some(url) = url {
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
        let enter = gtk::EventControllerKey::new();
        enter.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        enter.connect_key_pressed(move |_, key, _, modifiers| {
            if !modifiers.intersects(
                gdk::ModifierType::SHIFT_MASK
                    | gdk::ModifierType::CONTROL_MASK
                    | gdk::ModifierType::ALT_MASK,
            ) && let Some(ui) = weak.upgrade()
                && ui.slash_key(key)
            {
                return glib::Propagation::Stop;
            }
            let plain_enter = matches!(key, gdk::Key::Return | gdk::Key::KP_Enter)
                && !modifiers.intersects(
                    gdk::ModifierType::SHIFT_MASK
                        | gdk::ModifierType::CONTROL_MASK
                        | gdk::ModifierType::ALT_MASK,
                );
            match weak.upgrade() {
                Some(ui) if ui.editor.has_focus() && plain_enter && ui.continue_list() => {
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        self.editor.add_controller(enter);
        // Delete a visible grapheme, not the invisible delimiter beside it.
        let weak = Rc::downgrade(self);
        self.editor.connect_backspace(move |view| {
            if let Some(ui) = weak.upgrade() {
                view.stop_signal_emission_by_name("backspace");
                ui.delete_visible(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.editor
            .connect_delete_from_cursor(move |view, kind, count| {
                if kind == gtk::DeleteType::Chars
                    && let Some(ui) = weak.upgrade()
                {
                    view.stop_signal_emission_by_name("delete-from-cursor");
                    for _ in 0..count.unsigned_abs() {
                        ui.delete_visible(count < 0);
                    }
                }
            });
        // Copy the text the user sees, without hidden Markdown syntax.
        self.editor.connect_copy_clipboard(|view| {
            view.stop_signal_emission_by_name("copy-clipboard");
            let buffer = view.buffer();
            if let Some((start, end)) = buffer.selection_bounds() {
                view.clipboard().set_text(&buffer.text(&start, &end, false));
            }
        });
        // A pasted URL becomes a link. Other text pastes as usual.
        let weak = Rc::downgrade(self);
        self.editor.connect_paste_clipboard(move |view| {
            if !view.is_editable() {
                return;
            }
            view.stop_signal_emission_by_name("paste-clipboard");
            let weak = weak.clone();
            let clipboard = view.clipboard();
            clipboard.read_text_async(None::<&gio::Cancellable>, move |result| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                let buffer = ui.editor.buffer();
                let editable = ui.editor.is_editable();
                match result.ok().flatten() {
                    Some(text) if ui.paste_link(&text) => {}
                    Some(text) => {
                        buffer.begin_user_action();
                        buffer.delete_selection(true, editable);
                        buffer.insert_interactive_at_cursor(&text, editable);
                        buffer.end_user_action();
                    }
                    // Not text: let GTK paste whatever it can.
                    None => buffer.paste_clipboard(&ui.editor.clipboard(), None, editable),
                }
                ui.editor.scroll_mark_onscreen(&buffer.get_insert());
            });
        });
        self.editor.connect_cut_clipboard(|view| {
            view.stop_signal_emission_by_name("cut-clipboard");
            let buffer = view.buffer();
            if let Some((start, end)) = buffer.selection_bounds() {
                view.clipboard().set_text(&buffer.text(&start, &end, false));
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
                let commands = {
                    let s = &mut *ui.state.borrow_mut();
                    s.failed.retry(&mut s.drafts)
                };
                ui.error_box.set_visible(false);
                for command in commands {
                    ui.send(command);
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
        self.refresh_slash(false);
        let s = self.state.borrow();
        let note = s
            .active
            .as_ref()
            .and_then(|id| s.drafts.get(id))
            .map(Draft::note);
        self.location
            .set_sensitive(note.is_some_and(|n| !n.deleted));
        self.trash_button.set_sensitive(note.is_some());
        let Some(note) = note else {
            return;
        };
        self.location.set_label(
            &s.notebooks
                .iter()
                .find(|b| b.id == note.notebook_id)
                .map(|b| b.name.clone())
                .unwrap_or_else(|| "Default".into()),
        );
        self.trash_button.set_icon_name(if note.deleted {
            "edit-undo-symbolic"
        } else {
            "user-trash-symbolic"
        });
        self.trash_button.set_tooltip_text(Some(if note.deleted {
            "Restore note"
        } else {
            "Move to Trash"
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

    /// A new note passes an empty `snapshot`.
    fn install_draft(self: &Rc<Self>, note: Note, created: bool, snapshot: &[u8]) {
        let id = note.id.clone();
        let body = self
            .state
            .borrow_mut()
            .drafts
            .open(note, snapshot, created)
            .text();
        let buffer = gtk::TextBuffer::new(None::<&gtk::TextTagTable>);
        configure_tags(&buffer);
        buffer.set_text(&body);
        buffer.set_enable_undo(false);
        let (weak, page) = (Rc::downgrade(self), id.clone());
        buffer.connect_insert_text(move |_, at, inserted| {
            if let Some(ui) = weak.upgrade() {
                ui.mirror_typing(&page, |d| d.insert(at.offset() as usize, inserted));
            }
        });
        let (weak, page) = (Rc::downgrade(self), id.clone());
        buffer.connect_delete_range(move |_, start, end| {
            let (start, end) = (start.offset() as usize, end.offset() as usize);
            if end > start
                && let Some(ui) = weak.upgrade()
            {
                ui.mirror_typing(&page, |d| d.delete(start, end - start));
            }
        });
        let (weak, page) = (Rc::downgrade(self), id.clone());
        buffer.connect_changed(move |buffer| {
            if let Some(ui) = weak.upgrade()
                && !ui.from_doc.get()
            {
                ui.text_changed(&page, buffer);
                let weak = Rc::downgrade(&ui);
                let page = page.clone();
                // Changes outside a GTK user action (including accessibility
                // edits) also settle before the next frame.
                glib::idle_add_local_once(move || {
                    if let Some(ui) = weak.upgrade() {
                        ui.finish_rich_edit(&page);
                    }
                });
            }
        });
        let (weak, page) = (Rc::downgrade(self), id.clone());
        buffer.connect_end_user_action(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.finish_rich_edit(&page);
            }
        });
        let weak = Rc::downgrade(self);
        buffer.connect_mark_set(move |buffer, _, mark| {
            if (mark.name().as_deref() == Some("insert")
                || mark.name().as_deref() == Some("selection_bound"))
                && let Some(ui) = weak.upgrade()
                && !ui.updating.get()
                && !ui.from_doc.get()
                && *buffer == ui.editor.buffer()
            {
                ui.refresh_slash(false);
            }
        });
        self.state.borrow_mut().pages.insert(
            id,
            Page {
                buffer,
                body,
                before_edit: None,
            },
        );
    }

    fn display_note(self: &Rc<Self>, id: &str, cursor: Option<i32>) {
        self.slash.close();
        self.updating.set(true);
        let (buffer, editable) = {
            let mut s = self.state.borrow_mut();
            s.active = Some(id.into());
            s.parse_generation += 1;
            s.document = Document::default();
            let d = s.drafts.get_mut(id).expect("loaded draft");
            d.touch();
            let editable = !d.note().deleted;
            (s.pages[id].buffer.clone(), editable)
        };
        self.editor.set_buffer(Some(&buffer));
        self.update_writing_margins();
        self.placeholder
            .set_visible(buffer.char_count() == 0 && editable);
        self.editor.set_editable(editable);
        if let Some(cursor) = cursor {
            buffer.place_cursor(&buffer.iter_at_offset(cursor.clamp(0, buffer.char_count())));
        }
        self.dismiss_format_menu();
        self.editor_stack.set_visible_child_name("editor");
        self.updating.set(false);
        self.render_now(id);
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
        let s = &mut *s;
        for id in s.drafts.trim(s.active.as_deref()) {
            s.pages.remove(&id);
        }
    }

    fn open_note(self: &Rc<Self>, id: &str) {
        self.flush_drafts();
        let (cached, generation) = {
            let mut s = self.state.borrow_mut();
            s.load_generation += 1;
            (s.drafts.get(id).is_some(), s.load_generation)
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
        self.install_draft(note.clone(), false, &[]);
        self.display_note(&id, Some(0));
        self.send(Command::Create(note));
        self.request_list();
    }

    fn trash_or_restore(&self) {
        self.flush_drafts();
        let s = self.state.borrow();
        if let Some(note) = s
            .active
            .as_ref()
            .and_then(|id| s.drafts.get(id))
            .map(Draft::note)
        {
            self.send(Command::Mutate(if note.deleted {
                Mutation::Restore {
                    id: note.id.clone(),
                }
            } else {
                Mutation::Trash {
                    id: note.id.clone(),
                }
            }));
        }
    }

    fn flush_drafts(&self) {
        let ids: Vec<_> = self.state.borrow().pages.keys().cloned().collect();
        for id in ids {
            self.finish_rich_edit(&id);
        }
        let commands = self.state.borrow_mut().drafts.flush();
        for command in commands {
            self.send(command);
        }
    }

    /// Runs before GTK applies the edit, while the offsets still describe the
    /// text both sides share.
    fn mirror_typing(&self, id: &str, edit: impl FnOnce(&mut Draft) -> Result<(), OutOfSync>) {
        if self.from_doc.get() {
            return;
        }
        let mut s = self.state.borrow_mut();
        if let Some(page) = s.pages.get_mut(id)
            && page.before_edit.is_none()
        {
            page.before_edit = Some(page.body.clone());
        }
        if let Some(d) = s.drafts.get_mut(id) {
            let _ = edit(d);
        }
    }

    fn text_changed(&self, id: &str, buffer: &gtk::TextBuffer) {
        let mut s = self.state.borrow_mut();
        let Some(page) = s.pages.get_mut(id) else {
            return;
        };
        page.body = buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), true)
            .to_string();
        let empty = page.body.is_empty();
        if s.active.as_deref() == Some(id) {
            self.placeholder.set_visible(empty);
            s.parse_generation += 1;
            s.document = Document::default();
        }
        guard_buffer_end(buffer);
    }

    fn render_now(&self, id: &str) {
        let body = {
            let s = self.state.borrow();
            if s.active.as_deref() != Some(id) {
                return;
            }
            s.pages[id].body.clone()
        };
        let document = markdown::parse(&body, Units::Chars);
        {
            let mut s = self.state.borrow_mut();
            s.document = document;
            s.parsed_generation = s.parse_generation;
        }
        self.apply_document();
        self.refresh_slash(false);
    }

    fn finish_rich_edit(&self, id: &str) {
        let pending = {
            let mut s = self.state.borrow_mut();
            let Some(page) = s.pages.get_mut(id) else {
                return;
            };
            page.before_edit
                .take()
                .map(|before| (before, page.body.clone(), page.buffer.clone()))
        };
        let Some((before, after, buffer)) = pending else {
            return;
        };
        let old: Vec<_> = before.chars().collect();
        let new: Vec<_> = after.chars().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        // Reconstruct the native replacement using visible-text deletion.
        // Deleting only syntax keeps it intact; inserted text still survives.
        let deletion = formatting::delete(&before, prefix..old.len() - suffix);
        let (base, at) =
            deletion.map_or((before, prefix), |edit| (edit.text, edit.selection.start));
        let inserted: String = new[prefix..new.len() - suffix].iter().collect();
        let mut edit = formatting::insert(&base, at, &inserted);
        if edit.text != after {
            edit.range = 0..new.len();
            self.replace_source(id, &buffer, edit);
        }
        self.render_now(id);
        let active = self.state.borrow().active.as_deref() == Some(id);
        if active {
            self.refresh_slash(inserted.contains('/'));
        }
    }

    fn replace_source(&self, id: &str, buffer: &gtk::TextBuffer, edit: formatting::Edit) {
        {
            let mut s = self.state.borrow_mut();
            let Some(draft) = s.drafts.get_mut(id) else {
                return;
            };
            if draft
                .replace(edit.range.start, edit.range.len(), &edit.text)
                .is_err()
            {
                return;
            }
        }
        let old: Vec<_> = buffer
            .text(
                &buffer.iter_at_offset(edit.range.start as i32),
                &buffer.iter_at_offset(edit.range.end as i32),
                true,
            )
            .chars()
            .collect();
        let new: Vec<_> = edit.text.chars().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let start = edit.range.start + prefix;
        let end = edit.range.end - suffix;
        let inserted: String = new[prefix..new.len() - suffix].iter().collect();
        self.from_doc.set(true);
        self.updating.set(true);
        let inserted_len = inserted.chars().count();
        buffer.insert(&mut buffer.iter_at_offset(start as i32), &inserted);
        if buffer.has_selection() && edit.selection.start != edit.selection.end && inserted_len > 0
        {
            // Keep the primary selection alive throughout the replacement.
            // Clearing and reclaiming it in one event can make X11 deliver a
            // stale ownership-loss notification that deselects the new text.
            buffer.select_range(
                &buffer.iter_at_offset((start + inserted_len) as i32),
                &buffer.iter_at_offset(start as i32),
            );
        }
        buffer.delete(
            &mut buffer.iter_at_offset((start + inserted_len) as i32),
            &mut buffer.iter_at_offset((end + inserted_len) as i32),
        );
        buffer.select_range(
            &buffer.iter_at_offset(edit.selection.end as i32),
            &buffer.iter_at_offset(edit.selection.start as i32),
        );
        self.updating.set(false);
        self.from_doc.set(false);
        self.text_changed(id, buffer);
    }

    fn set_task_checked(&self, buffer: &gtk::TextBuffer, offset: i32, checked: bool) {
        if !self.editor.is_editable() || *buffer != self.editor.buffer() {
            return;
        }
        let Some(id) = self.state.borrow().active.clone() else {
            return;
        };
        self.finish_rich_edit(&id);
        // Reparse pending edits before trusting a widget's location. This also
        // prevents an obsolete control from changing ordinary text or code.
        self.render_now(&id);
        let is_task = self
            .state
            .borrow()
            .document
            .spans
            .iter()
            .any(|span| span.range.start == offset && matches!(span.style, "task" | "checked"));
        if !is_task {
            return;
        }
        let cursor = buffer.cursor_position() as usize;
        let bound = buffer.iter_at_mark(&buffer.selection_bound()).offset() as usize;
        self.state
            .borrow_mut()
            .drafts
            .get_mut(&id)
            .unwrap()
            .undo_boundary();
        self.replace_source(
            &id,
            buffer,
            formatting::Edit {
                range: offset as usize + 1..offset as usize + 2,
                text: if checked { "x" } else { " " }.into(),
                selection: bound..cursor,
            },
        );
        self.state
            .borrow_mut()
            .drafts
            .get_mut(&id)
            .unwrap()
            .undo_boundary();
        self.render_now(&id);
    }

    fn format_selection(&self, format: Format) {
        if !self.editor.is_editable() {
            return;
        }
        let Some(id) = self.state.borrow().active.clone() else {
            return;
        };
        self.finish_rich_edit(&id);
        let buffer = self.editor.buffer();
        let selection = buffer
            .selection_bounds()
            .map(|(a, b)| a.offset() as usize..b.offset() as usize)
            .unwrap_or_else(|| {
                let at = buffer.cursor_position() as usize;
                at..at
            });
        let body = self.state.borrow().pages[&id].body.clone();
        if let Some(edit) = formatting::toggle(&body, selection, format) {
            self.state
                .borrow_mut()
                .drafts
                .get_mut(&id)
                .unwrap()
                .undo_boundary();
            self.replace_source(&id, &buffer, edit);
            self.state
                .borrow_mut()
                .drafts
                .get_mut(&id)
                .unwrap()
                .undo_boundary();
            self.dismiss_format_menu();
            self.render_now(&id);
        }
        self.editor.grab_focus();
    }

    fn refresh_slash(&self, may_open: bool) {
        let buffer = self.editor.buffer();
        let editable = {
            let s = self.state.borrow();
            s.active
                .as_ref()
                .and_then(|id| s.drafts.get(id))
                .is_some_and(|d| !d.note().deleted)
        };
        if !editable || !self.editor.is_editable() || !self.editor.has_focus() {
            self.slash.close();
            self.selection_due.set(None);
            return;
        }
        let selection = buffer
            .selection_bounds()
            .map(|(a, b)| a.offset() as usize..b.offset() as usize);
        if self.slash.observe_selection(selection.clone()) {
            // Wait for the selection to settle, so the popup never interrupts
            // dragging or a sequence of Shift+arrow presses.
            self.selection_due.set(
                selection
                    .as_ref()
                    .map(|_| Instant::now() + Duration::from_millis(150)),
            );
        }
        if selection.is_some() {
            if self
                .selection_due
                .get()
                .is_some_and(|due| due <= Instant::now())
            {
                let dragging = self
                    .editor
                    .display()
                    .default_seat()
                    .and_then(|seat| seat.pointer())
                    .is_some_and(|pointer| {
                        pointer
                            .modifier_state()
                            .contains(gdk::ModifierType::BUTTON1_MASK)
                    });
                if !dragging {
                    self.selection_due.set(None);
                    self.slash.show_selection(&self.editor);
                }
            }
            return;
        }
        if !may_open && !self.slash.popover.is_visible() {
            return;
        }
        let source = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        let Some(query) = slash_menu::query(&source, buffer.cursor_position() as usize) else {
            self.slash.close();
            return;
        };
        let moved = self
            .slash
            .query
            .borrow()
            .as_ref()
            .is_some_and(|old| old.start != query.start);
        let code = self.state.borrow().document.spans.iter().any(|span| {
            matches!(span.style, "code" | "code-block")
                && span.range.contains(&(query.start as i32))
        });
        if (!may_open && moved) || code {
            self.slash.close();
            return;
        }
        self.slash.show(query, &self.editor);
    }

    fn dismiss_format_menu(&self) {
        let buffer = self.editor.buffer();
        self.slash.observe_selection(
            buffer
                .selection_bounds()
                .map(|(a, b)| a.offset() as usize..b.offset() as usize),
        );
        self.slash.close();
        self.selection_due.set(None);
    }

    fn slash_key(&self, key: gdk::Key) -> bool {
        if !self.slash.popover.is_visible() {
            if key == gdk::Key::Escape && self.selection_due.get().is_some() {
                self.dismiss_format_menu();
                return true;
            }
            return false;
        }
        match key {
            gdk::Key::Escape => self.slash.close(),
            gdk::Key::Up => self.slash.move_selection(-1),
            gdk::Key::Down => self.slash.move_selection(1),
            gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::Tab => {
                if let Some(action) = self.slash.selected() {
                    self.run_menu_action(action);
                } else {
                    self.slash.close();
                    return false;
                }
            }
            _ => return false,
        }
        true
    }

    fn run_menu_action(&self, action: MenuAction) {
        if !self.editor.is_editable() {
            self.slash.close();
            return;
        }
        let selection = self.slash.selection.borrow().clone();
        if let Some(selection) = selection {
            let buffer = self.editor.buffer();
            let current = buffer
                .selection_bounds()
                .map(|(a, b)| a.offset() as usize..b.offset() as usize);
            if current.as_ref() != Some(&selection) {
                self.dismiss_format_menu();
                return;
            }
            if action == MenuAction::Link {
                self.prompt_link();
                return;
            }
            match action {
                MenuAction::Link => {}
                MenuAction::Inline(format) => self.format_selection(format),
                MenuAction::Block(block) => {
                    let Some(id) = self.state.borrow().active.clone() else {
                        return;
                    };
                    self.finish_rich_edit(&id);
                    let body = self.state.borrow().pages[&id].body.clone();
                    if let Some(edit) = formatting::selected_blocks(&body, selection, block) {
                        self.state
                            .borrow_mut()
                            .drafts
                            .get_mut(&id)
                            .unwrap()
                            .undo_boundary();
                        self.replace_source(&id, &buffer, edit);
                        self.state
                            .borrow_mut()
                            .drafts
                            .get_mut(&id)
                            .unwrap()
                            .undo_boundary();
                        self.dismiss_format_menu();
                        self.render_now(&id);
                    }
                }
            }
            self.dismiss_format_menu();
            self.editor.grab_focus();
        } else if let MenuAction::Block(block) = action {
            self.run_slash_command(block);
        } else if action == MenuAction::Link {
            self.run_link_command();
        }
    }

    /// `/link` removes its query, then asks for a URL to insert at the caret.
    fn run_link_command(&self) {
        let Some(id) = self.state.borrow().active.clone() else {
            return;
        };
        self.finish_rich_edit(&id);
        let Some(query) = self.slash.query.borrow().clone() else {
            return;
        };
        let buffer = self.editor.buffer();
        let body = self.state.borrow().pages[&id].body.clone();
        self.slash.close();
        if buffer.has_selection()
            || slash_menu::query(&body, buffer.cursor_position() as usize).as_ref() != Some(&query)
        {
            return;
        }
        let Some(deletion) = formatting::delete(&body, query.start..query.end) else {
            return;
        };
        self.state
            .borrow_mut()
            .drafts
            .get_mut(&id)
            .unwrap()
            .undo_boundary();
        self.replace_source(&id, &buffer, deletion);
        self.render_now(&id);
        self.prompt_link();
    }

    /// Ask for a URL to link the selection to, or to insert at the caret.
    fn prompt_link(&self) {
        if !self.editor.is_editable() {
            return;
        }
        let Some(id) = self.state.borrow().active.clone() else {
            return;
        };
        if self
            .state
            .borrow()
            .drafts
            .get(&id)
            .is_none_or(|d| d.note().deleted)
        {
            return;
        }
        self.finish_rich_edit(&id);
        let buffer = self.editor.buffer();
        let range = buffer
            .selection_bounds()
            .map(|(a, b)| a.offset() as usize..b.offset() as usize)
            .unwrap_or_else(|| {
                let at = buffer.cursor_position() as usize;
                at..at
            });
        let body = self.state.borrow().pages[&id].body.clone();
        self.dismiss_format_menu();
        self.link.open(
            LinkTarget {
                note: id,
                body,
                range,
            },
            &self.editor,
        );
    }

    fn apply_link(&self) {
        let target = self.link.target.borrow_mut().take();
        let url = formatting::typed_url(&self.link.entry.text());
        self.link.popover.popdown();
        let (Some(target), Some(url)) = (target, url) else {
            return;
        };
        let Some(id) = self.state.borrow().active.clone() else {
            return;
        };
        if id != target.note || !self.editor.is_editable() {
            return;
        }
        self.finish_rich_edit(&id);
        let body = self.state.borrow().pages[&id].body.clone();
        // An edit from another device moved the text since the prompt opened.
        if body != target.body {
            return;
        }
        let Some(edit) = formatting::link(&body, target.range, &url) else {
            return;
        };
        self.apply_edit(&id, edit);
    }

    /// Link a URL pasted on its own, or link the selection to it. Returns
    /// false when the text should paste as it is.
    fn paste_link(&self, pasted: &str) -> bool {
        if !self.editor.is_editable() {
            return false;
        }
        let Some(id) = self.state.borrow().active.clone() else {
            return false;
        };
        self.finish_rich_edit(&id);
        let buffer = self.editor.buffer();
        let selection = buffer
            .selection_bounds()
            .map(|(a, b)| a.offset() as usize..b.offset() as usize)
            .unwrap_or_else(|| {
                let at = buffer.cursor_position() as usize;
                at..at
            });
        let body = self.state.borrow().pages[&id].body.clone();
        let Some(edit) = formatting::paste_link(&body, selection, pasted) else {
            return false;
        };
        self.dismiss_format_menu();
        self.apply_edit(&id, edit);
        true
    }

    /// Replace source as one undo step and show the result.
    fn apply_edit(&self, id: &str, edit: formatting::Edit) {
        let buffer = self.editor.buffer();
        self.state
            .borrow_mut()
            .drafts
            .get_mut(id)
            .unwrap()
            .undo_boundary();
        self.replace_source(id, &buffer, edit);
        self.state
            .borrow_mut()
            .drafts
            .get_mut(id)
            .unwrap()
            .undo_boundary();
        self.render_now(id);
        self.editor.grab_focus();
        self.editor.scroll_mark_onscreen(&buffer.get_insert());
    }

    fn run_slash_command(&self, block: Block) {
        if !self.editor.is_editable() {
            return;
        }
        let Some(id) = self.state.borrow().active.clone() else {
            return;
        };
        self.finish_rich_edit(&id);
        let Some(query) = self.slash.query.borrow().clone() else {
            return;
        };
        let buffer = self.editor.buffer();
        let body = self.state.borrow().pages[&id].body.clone();
        if buffer.has_selection()
            || slash_menu::query(&body, buffer.cursor_position() as usize).as_ref() != Some(&query)
        {
            self.slash.close();
            return;
        }
        let old_len = body.chars().count();
        let Some(deletion) = formatting::delete(&body, query.start..query.end) else {
            self.slash.close();
            return;
        };
        let mut chars: Vec<_> = deletion.text.chars().collect();
        let Some(edit) = formatting::block(&deletion.text, deletion.selection.start, block) else {
            self.slash.close();
            return;
        };
        chars.splice(edit.range, edit.text.chars());
        self.slash.close();
        self.state
            .borrow_mut()
            .drafts
            .get_mut(&id)
            .unwrap()
            .undo_boundary();
        self.replace_source(
            &id,
            &buffer,
            formatting::Edit {
                range: 0..old_len,
                text: chars.iter().collect(),
                selection: edit.selection,
            },
        );
        self.state
            .borrow_mut()
            .drafts
            .get_mut(&id)
            .unwrap()
            .undo_boundary();
        self.render_now(&id);
        self.editor.grab_focus();
        self.editor.scroll_mark_onscreen(&buffer.get_insert());
    }

    fn delete_visible(&self, backwards: bool) {
        if !self.editor.is_editable() {
            return;
        }
        let Some(id) = self.state.borrow().active.clone() else {
            return;
        };
        self.finish_rich_edit(&id);
        let buffer = self.editor.buffer();
        let (mut start, mut end) = buffer.selection_bounds().unwrap_or_else(|| {
            let cursor = buffer.iter_at_mark(&buffer.get_insert());
            let mut other = cursor;
            if backwards {
                other.backward_visible_cursor_position();
                (other, cursor)
            } else {
                other.forward_visible_cursor_position();
                (cursor, other)
            }
        });
        buffer.begin_user_action();
        buffer.delete(&mut start, &mut end);
        buffer.end_user_action();
    }

    /// Writes a draft's changes from an import or undo into its buffer.
    /// Returns the offset just after the last change.
    fn apply_incoming(&self, id: &str, buffer: &gtk::TextBuffer, applied: Applied) -> Option<i32> {
        if applied.edits.is_empty() {
            return None;
        }
        self.from_doc.set(true);
        for edit in applied.edits {
            match edit {
                TextEdit::Insert { at, text } => {
                    buffer.insert(&mut buffer.iter_at_offset(at as i32), &text);
                }
                TextEdit::Delete { at, len } => {
                    buffer.delete(
                        &mut buffer.iter_at_offset(at as i32),
                        &mut buffer.iter_at_offset((at + len) as i32),
                    );
                }
            }
        }
        self.from_doc.set(false);
        self.text_changed(id, buffer);
        if *buffer == self.editor.buffer() {
            self.dismiss_format_menu();
        }
        self.render_now(id);
        applied.cursor.map(|cursor| cursor as i32)
    }

    fn undo_redo(&self, undo: bool) {
        if !self.editor.is_editable() {
            return;
        }
        let active = self.state.borrow().active.clone();
        if let Some(id) = active {
            self.finish_rich_edit(&id);
        }
        let target = {
            let mut s = self.state.borrow_mut();
            let s = &mut *s;
            let Some(id) = s.active.clone() else {
                return;
            };
            let Some(d) = s.drafts.get_mut(&id) else {
                return;
            };
            let Some(applied) = (if undo { d.undo() } else { d.redo() }) else {
                return;
            };
            Some((s.pages[&id].buffer.clone(), id, applied))
        };
        if let Some((buffer, id, applied)) = target
            && let Some(offset) = self.apply_incoming(&id, &buffer, applied)
        {
            buffer.place_cursor(&buffer.iter_at_offset(offset));
            self.editor.scroll_mark_onscreen(&buffer.get_insert());
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
        if let Some(page) = s.active.as_ref().and_then(|id| s.pages.get(id)) {
            let count = page
                .buffer
                .text(&page.buffer.start_iter(), &page.buffer.end_iter(), false)
                .split_whitespace()
                .count();
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
        guard_buffer_end(&buffer);
        self.state.borrow_mut().hidden_applied.clear();
        for span in &self.state.borrow().document.spans {
            buffer.apply_tag_by_name(
                span.style,
                &buffer.iter_at_offset(span.range.start),
                &buffer.iter_at_offset(span.range.end),
            );
        }
        self.task_checkboxes
            .sync(&self.editor, &self.state.borrow().document);
        self.apply_hanging_indents();
        self.update_hidden();
        self.task_checkboxes.place(&self.editor);
    }

    fn continue_list(&self) -> bool {
        let buffer = self.editor.buffer();
        if !self.editor.is_editable() || buffer.has_selection() {
            return false;
        }
        let mut cursor = buffer.iter_at_mark(&buffer.get_insert());
        let mut start = cursor;
        start.set_line_offset(0);
        let mut end = cursor;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        let line = buffer.text(&start, &end, true);
        let enter = markdown::list_enter(&line);
        if enter.is_some()
            && markdown::ends_in_code_block(&buffer.text(&buffer.start_iter(), &end, true))
        {
            return false;
        }
        match enter {
            Some(markdown::ListEnter::Continue { marker, next })
                if cursor.line_offset() >= marker as i32 =>
            {
                let start = cursor.offset() + 1;
                buffer.begin_user_action();
                buffer.insert(&mut cursor, &next);
                buffer.end_user_action();
                buffer.apply_tag_by_name("list-item-start", &buffer.iter_at_offset(start), &cursor);
                if let Some(task) = next.find("[ ]") {
                    let task = cursor.offset() - next[task..].chars().count() as i32;
                    buffer.apply_tag_by_name(
                        "task",
                        &buffer.iter_at_offset(task),
                        &buffer.iter_at_offset(task + 3),
                    );
                }
            }
            Some(markdown::ListEnter::End) => {
                buffer.begin_user_action();
                buffer.delete(&mut start, &mut end);
                buffer.end_user_action();
            }
            _ => return false,
        }
        self.editor.scroll_mark_onscreen(&buffer.get_insert());
        true
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
            let mut display_start = end;
            if !self.editor.starts_display_line(&display_start) {
                self.editor.backward_display_line_start(&mut display_start);
            }
            if width <= 0 || display_start.offset() > start.offset() {
                continue;
            }
            let name = format!("hang-{width}");
            if tags.lookup(&name).is_none() {
                tags.add(
                    &gtk::TextTag::builder()
                        .name(&name)
                        .left_margin(self.editor.left_margin())
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
        let target = s.document.hidden.clone();
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
            if tag.name().is_some_and(|name| name.starts_with("hang-")) {
                tag.set_left_margin(margin);
            }
        });
    }

    fn tick(self: &Rc<Self>) {
        let pending: Vec<_> = self
            .state
            .borrow()
            .pages
            .iter()
            .filter(|(_, page)| page.before_edit.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in pending {
            self.finish_rich_edit(&id);
        }
        if self.editor.left_margin() != ((self.editor.width() - 720) / 2).max(48) {
            self.update_writing_margins();
        }
        self.task_checkboxes.place(&self.editor);
        while let Ok(event) = self.events.try_recv() {
            self.handle(event);
        }
        if let Some(status) = self.sync_statuses.try_iter().last() {
            *self.sync_status.borrow_mut() = status;
            self.update_sync_button();
            self.update_sync_dialog();
        }
        let now = Instant::now();
        if self.selection_due.get().is_some_and(|due| due <= now) {
            self.refresh_slash(false);
        }
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
            for command in s.drafts.due(now) {
                self.send(command);
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
                snapshot,
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
                self.install_draft(note, true, &snapshot);
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
                                && s.drafts.get(id).is_some_and(|d| d.note().deleted)
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
            Event::Loaded {
                note,
                snapshot,
                generation,
            } => {
                if generation != self.state.borrow().load_generation {
                    return;
                }
                if let Some(note) = note {
                    let id = note.id.clone();
                    self.install_draft(note, true, &snapshot);
                    self.display_note(&id, None);
                }
            }
            Event::Created(id) => {
                self.state.borrow_mut().drafts.created(&id);
                self.request_list();
                self.update_word_count();
            }
            Event::Saved { id, sequence } => {
                {
                    let mut s = self.state.borrow_mut();
                    s.drafts.saved(&id, sequence);
                    s.failed.saved(&id, sequence);
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
                    let s = &mut *s;
                    s.notebooks = notebooks;
                    s.drafts.mutated(&mutation, &s.default_notebook_id);
                    if let Mutation::Trash { id } = &mutation
                        && s.active.as_ref() == Some(id)
                        && s.filter != Filter::Trash
                    {
                        s.select_first = true;
                        s.load_generation += 1;
                    }
                    s.leave_missing_notebook();
                    if let Some(d) = s.active.as_ref().and_then(|id| s.drafts.get(id)) {
                        self.editor.set_editable(!d.note().deleted);
                    }
                }
                self.rebuild_sidebar();
                self.request_list();
                self.update_word_count();
            }
            Event::NoteDelta { id, delta } => {
                let target = {
                    let mut s = self.state.borrow_mut();
                    let s = &mut *s;
                    s.drafts
                        .get_mut(&id)
                        .map(|d| (d.import(&delta), s.pages[&id].buffer.clone()))
                };
                if let Some((applied, buffer)) = target
                    && self.apply_incoming(&id, &buffer, applied).is_some()
                    && self.state.borrow().active.as_deref() == Some(&id)
                {
                    self.update_word_count();
                }
            }
            Event::Remote { notes, notebooks } => {
                let books_changed = notebooks.is_some();
                {
                    let mut s = self.state.borrow_mut();
                    if let Some(notebooks) = notebooks {
                        s.notebooks = notebooks;
                        s.leave_missing_notebook();
                    }
                    let trashed = s.drafts.remote(&notes);
                    if s.filter != Filter::Trash
                        && s.active.as_ref().is_some_and(|id| trashed.contains(id))
                    {
                        s.select_first = true;
                        s.load_generation += 1;
                    }
                    if let Some(d) = s.active.as_ref().and_then(|id| s.drafts.get(id)) {
                        self.editor.set_editable(!d.note().deleted);
                    }
                }
                if books_changed {
                    self.rebuild_sidebar();
                }
                self.update_location();
                self.request_list();
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
                s.failed.record(command);
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

    fn update_sync_button(&self) {
        let status = self.sync_status.borrow();
        let connected: Vec<&str> = status
            .devices
            .iter()
            .filter(|d| d.connected)
            .map(|d| d.name.as_str())
            .collect();
        let tooltip = if let Some(problem) = &status.problem {
            problem.clone()
        } else if !status.enabled {
            "Sync with your other devices".into()
        } else {
            match connected.as_slice() {
                [] => "Sync is on. No devices are connected".into(),
                [one] => format!("Syncing with {one}"),
                many => format!("Syncing with {} devices", many.len()),
            }
        };
        self.sync_button.set_tooltip_text(Some(&tooltip));
        if connected.is_empty() {
            self.sync_button.remove_css_class("sync-active");
        } else {
            self.sync_button.add_css_class("sync-active");
        }
    }

    fn show_sync_dialog(self: &Rc<Self>) {
        if let Some(dialog) = self.sync_dialog.borrow().as_ref() {
            dialog.window.present();
            return;
        }
        let window = gtk::Window::builder()
            .title("Sync")
            .transient_for(&self.window)
            .modal(true)
            .default_width(420)
            .build();
        window.add_css_class("notebook-app");
        window.add_css_class("sync-dialog");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 10);
        content.set_margin_top(24);
        content.set_margin_bottom(24);
        content.set_margin_start(24);
        content.set_margin_end(24);
        content.set_size_request(372, -1);
        let quiet = Rc::new(Cell::new(false));
        let label = wrapped_label;

        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let intro = gtk::Box::new(gtk::Orientation::Vertical, 4);
        intro.set_hexpand(true);
        intro.append(&label("Sync with your other devices", "sync-heading"));
        intro.append(&label(
            "Notes travel directly between your paired devices, encrypted. Nothing leaves this network unless you add a relay.",
            "sync-hint",
        ));
        heading.append(&intro);
        let enabled = gtk::Switch::new();
        enabled.set_valign(gtk::Align::Center);
        heading.append(&enabled);
        content.append(&heading);

        content.append(&label("This device", "eyebrow"));
        let name = gtk::Entry::builder()
            .placeholder_text("Device name")
            .max_length(60)
            .build();
        content.append(&name);

        content.append(&label("Paired devices", "eyebrow"));
        let devices = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.append(&devices);

        content.append(&label("Pair a device", "eyebrow"));
        let pairing = gtk::Stack::new();
        pairing.set_vhomogeneous(false);
        let pairing_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let pairing_note = label("", "sync-hint");
        pairing_box.append(&pairing_note);
        let show_code = gtk::Button::with_label("Show a pairing code");
        pairing_box.append(&show_code);
        let join_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let join = gtk::Entry::builder()
            .placeholder_text("Code shown on the other device")
            .max_length(7)
            .input_purpose(gtk::InputPurpose::Digits)
            .hexpand(true)
            .build();
        let pair = gtk::Button::with_label("Pair");
        pair.add_css_class("suggested-action");
        join_row.append(&join);
        join_row.append(&pair);
        pairing_box.append(&join_row);
        pairing.add_named(&pairing_box, Some("idle"));
        let showing = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let code = label("", "pairing-code");
        code.set_selectable(true);
        showing.append(&code);
        showing.append(&label(
            "Enter this code on your other device. It expires in five minutes.",
            "sync-hint",
        ));
        let cancel_code = gtk::Button::with_label("Cancel");
        showing.append(&cancel_code);
        pairing.add_named(&showing, Some("showing"));
        let searching = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let spinner = gtk::Spinner::new();
        spinner.start();
        searching.append(&spinner);
        let looking = label("Looking for your other device…", "sync-hint");
        looking.set_hexpand(true);
        searching.append(&looking);
        let cancel_search = gtk::Button::with_label("Cancel");
        searching.append(&cancel_search);
        pairing.add_named(&searching, Some("searching"));
        content.append(&pairing);

        let internet = gtk::Expander::new(Some("Sync over the internet"));
        let relay_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
        relay_box.set_margin_top(8);
        relay_box.append(&label(
            "Devices on different networks can meet through a relay server you run, such as iroh-relay. Enter the same address on each device. Notes stay end-to-end encrypted.",
            "sync-hint",
        ));
        let relay_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let relay = gtk::Entry::builder()
            .placeholder_text("https://relay.example.com")
            .hexpand(true)
            .build();
        let apply = gtk::Button::with_label("Apply");
        relay_row.append(&relay);
        relay_row.append(&apply);
        relay_box.append(&relay_row);
        internet.set_child(Some(&relay_box));
        content.append(&internet);

        let problem = label("", "sync-problem");
        content.append(&problem);
        window.set_child(Some(&content));

        let weak = Rc::downgrade(self);
        let q = quiet.clone();
        enabled.connect_active_notify(move |switch| {
            if let Some(ui) = weak.upgrade()
                && !q.get()
            {
                ui.sync.send(Control::SetEnabled(switch.is_active()));
            }
        });
        let weak = Rc::downgrade(self);
        name.connect_activate(move |entry| {
            if let Some(ui) = weak.upgrade() {
                ui.sync
                    .send(Control::SetDeviceName(entry.text().to_string()));
            }
        });
        self.send_on_click(&show_code, Control::StartPairing);
        let weak = Rc::downgrade(self);
        let entry = join.clone();
        pair.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.sync.send(Control::JoinPairing(entry.text().to_string()));
                entry.set_text("");
            }
        });
        let pair = pair.downgrade();
        join.connect_activate(move |_| {
            if let Some(pair) = pair.upgrade() {
                pair.emit_clicked();
            }
        });
        for cancel in [&cancel_code, &cancel_search] {
            self.send_on_click(cancel, Control::CancelPairing);
        }
        let weak = Rc::downgrade(self);
        let entry = relay.clone();
        apply.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                let url = entry.text().trim().to_string();
                ui.sync
                    .send(Control::SetRelay((!url.is_empty()).then_some(url)));
            }
        });
        let weak = Rc::downgrade(self);
        let entry = name.clone();
        window.connect_close_request(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.sync
                    .send(Control::SetDeviceName(entry.text().to_string()));
                ui.sync_dialog.borrow_mut().take();
                ui.window.present();
                ui.editor.grab_focus();
            }
            glib::Propagation::Proceed
        });
        *self.sync_dialog.borrow_mut() = Some(SyncDialog {
            window: window.clone(),
            quiet,
            enabled,
            name,
            devices,
            pairing,
            pairing_box,
            pairing_note,
            code,
            relay,
            problem,
            rows: RefCell::default(),
        });
        self.update_sync_dialog();
        window.present();
    }

    fn send_on_click(self: &Rc<Self>, button: &gtk::Button, control: Control) {
        let weak = Rc::downgrade(self);
        button.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.sync.send(control.clone());
            }
        });
    }

    fn update_sync_dialog(self: &Rc<Self>) {
        let dialog = self.sync_dialog.borrow();
        let Some(d) = dialog.as_ref() else {
            return;
        };
        let status = self.sync_status.borrow();
        d.quiet.set(true);
        d.enabled.set_active(status.enabled);
        if !d.name.has_focus() {
            d.name.set_text(&status.device_name);
        }
        if !d.relay.has_focus() {
            d.relay
                .set_text(status.relay_url.as_deref().unwrap_or_default());
        }
        let rows: Vec<[String; 3]> = status
            .devices
            .iter()
            .map(|device| {
                let state = if device.connected {
                    "Connected".to_string()
                } else if let Some(at) = device.last_synced
                    && let (Ok(date), Ok(now)) = (
                        glib::DateTime::from_unix_local(at / 1000),
                        glib::DateTime::now_local(),
                    )
                {
                    format!("Not connected. Last synced: {}", note_date(&date, &now))
                } else {
                    "Not connected".to_string()
                };
                [device.id.clone(), device.name.clone(), state]
            })
            .collect();
        // Status changes with every sync; rebuilding unchanged rows would
        // only lose focus on their buttons.
        if *d.rows.borrow() != rows {
            while let Some(child) = d.devices.first_child() {
                d.devices.remove(&child);
            }
            if rows.is_empty() {
                d.devices
                    .append(&wrapped_label("No paired devices yet.", "sync-hint"));
            }
            for [id, device, state] in &rows {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                row.add_css_class("sync-device");
                let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
                text.set_hexpand(true);
                let name = gtk::Label::new(Some(device));
                name.set_xalign(0.0);
                name.set_ellipsize(pango::EllipsizeMode::End);
                name.add_css_class("sync-device-name");
                text.append(&name);
                text.append(&wrapped_label(state, "sync-hint"));
                row.append(&text);
                let remove = gtk::Button::with_label("Remove");
                remove.add_css_class("flat");
                remove.set_valign(gtk::Align::Center);
                remove.set_tooltip_text(Some("Stop syncing with this device"));
                self.send_on_click(&remove, Control::Forget(id.clone()));
                row.append(&remove);
                d.devices.append(&row);
            }
            *d.rows.borrow_mut() = rows;
        }
        match &status.pairing {
            Pairing::Showing(code) => {
                d.code.set_text(code);
                d.pairing.set_visible_child_name("showing");
            }
            Pairing::Searching => d.pairing.set_visible_child_name("searching"),
            other => {
                d.pairing_note.set_text(&match other {
                    Pairing::Paired(name) => format!("Paired with {name}."),
                    Pairing::Failed(message) => format!("{message}."),
                    _ => "Turn on sync on both devices. They must be on the same network to pair."
                        .into(),
                });
                d.pairing.set_visible_child_name("idle");
            }
        }
        d.pairing_box.set_sensitive(status.running());
        d.problem
            .set_text(status.problem.as_deref().unwrap_or_default());
        d.problem.set_visible(status.problem.is_some());
        d.quiet.set(false);
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

fn wrapped_label(text: &str, class: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    // Wrap to the window instead of widening it.
    label.set_max_width_chars(1);
    label.add_css_class(class);
    label
}

fn configure_tags(buffer: &gtk::TextBuffer) {
    let table = buffer.tag_table();
    for (name, scale) in [
        ("h1", 1.875),
        ("h2", 1.5),
        ("h3", 1.25),
        ("h4", 1.12),
        ("h5", 1.0),
        ("h6", 0.94),
    ] {
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
    // The opaque native checkbox covers these markers while their measured
    // width reserves space in the text layout.
    table.add(
        &gtk::TextTag::builder()
            .name("task")
            .family("monospace")
            .scale(0.8)
            .allow_breaks(false)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("checked")
            .family("monospace")
            .scale(0.8)
            .allow_breaks(false)
            .build(),
    );
    // Collapse the unordered bullet without invisible runs: older GTK
    // versions misreport iterator geometry after hidden text on the line.
    table.add(
        &gtk::TextTag::builder()
            .name("task-bullet")
            .scale(0.0)
            .build(),
    );
    table.add(
        &gtk::TextTag::builder()
            .name("hidden")
            .invisible(true)
            .build(),
    );
    table.add(&gtk::TextTag::builder().name("end-guard").build());
}

// Workaround for a GTK bug: it aborts when a click below a last line with hidden text
// overruns that line's final segment; a tag toggle before the closing newline keeps the
// overrun harmless.
fn guard_buffer_end(buffer: &gtk::TextBuffer) {
    let end = buffer.end_iter();
    let mut last = end;
    if last.backward_char() {
        buffer.apply_tag_by_name("end-guard", &last, &end);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notebook_core::{crdt, editor::UNDO_MERGE_MS};

    fn draft<'a>(ui: &'a Ui, id: &str) -> std::cell::Ref<'a, Draft> {
        std::cell::Ref::map(ui.state.borrow(), |s| s.drafts.get(id).unwrap())
    }

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

    fn test_ui(app: &gtk::Application, path: std::path::PathBuf) -> Rc<Ui> {
        let config = path.with_extension("sync.json");
        build(
            app,
            path,
            config,
            "Desktop".into(),
            sync::Options {
                mdns: false,
                ..Default::default()
            },
        )
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
        let ui = test_ui(&app, path.clone());
        pump_until(|| ui.state.borrow().ready && ui.editor.has_focus());
        assert!(ui.placeholder.is_visible());
        let first = ui.state.borrow().active.clone().unwrap();
        assert_eq!(draft(&ui, &first).note().notebook_id, DEFAULT_NOTEBOOK);
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
            !draft(&ui, &first).has_unsaved()
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
        assert!(buffer.start_iter().has_tag(&hidden));
        ui.editor.emit_by_name::<()>("copy-clipboard", &[]);
        let clipboard = Rc::new(RefCell::new(None));
        let result = clipboard.clone();
        ui.editor
            .clipboard()
            .read_text_async(None::<&gio::Cancellable>, move |text| {
                *result.borrow_mut() = Some(text.unwrap().unwrap().to_string())
            });
        pump_until(|| clipboard.borrow().is_some());
        assert!(clipboard.borrow().as_ref().unwrap().contains("A thought"));
        assert!(!clipboard.borrow().as_ref().unwrap().contains("**"));
        buffer.place_cursor(&buffer.end_iter());
        // Typing after a pause starts a new undo step.
        let wait = Instant::now() + Duration::from_millis(UNDO_MERGE_MS as u64 + 50);
        pump_until(|| Instant::now() >= wait);
        buffer.begin_user_action();
        buffer.insert_at_cursor("!");
        buffer.end_user_action();
        let text = || buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.undo", None).unwrap();
        assert!(text().ends_with("Last line"));
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.redo", None).unwrap();
        assert!(text().ends_with("Last line!"));

        // Another device edits the open note: the text arrives in place, the
        // cursor stays put, and undo leaves the other device's edit alone.
        pump_until(|| !draft(&ui, &first).has_unsaved());
        let reader = storage::open_reader(&path).unwrap();
        let mut other = storage::Repository::open(&dir.path().join("other.db")).unwrap();
        let shared = storage::export_since(&reader, &first, None)
            .unwrap()
            .unwrap();
        other.apply_remote(&[shared], "this").unwrap();
        let version = other.versions(&[crdt::bucket(&first)]).unwrap();
        let body = other.load(&first).unwrap().unwrap().body;
        other
            .save(&first, &format!("> From the laptop\n{body}"))
            .unwrap();
        let from_laptop = other
            .export_since(
                &first,
                Some(&version.iter().find(|v| v.id == first).unwrap().version),
            )
            .unwrap()
            .unwrap();
        let wait = Instant::now() + Duration::from_millis(UNDO_MERGE_MS as u64 + 50);
        pump_until(|| Instant::now() >= wait);
        buffer.insert_at_cursor(" Local");
        let cursor = buffer.cursor_position();
        ui.send(Command::Remote {
            docs: vec![from_laptop],
            from: "laptop".into(),
        });
        pump_until(|| text().starts_with("> From the laptop\n"));
        assert!(text().ends_with("Last line! Local"));
        let arrived = "> From the laptop\n".chars().count() as i32;
        assert_eq!(buffer.cursor_position(), cursor + arrived);
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.undo", None).unwrap();
        assert!(text().starts_with("> From the laptop\n"));
        assert!(text().ends_with("Last line!"));
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.redo", None).unwrap();
        pump_until(|| !draft(&ui, &first).has_unsaved());
        let merged = storage::Repository::open(&path)
            .unwrap()
            .load(&first)
            .unwrap()
            .unwrap();
        assert!(merged.body.starts_with("> From the laptop\n# Hello 🌿"));
        assert!(merged.body.ends_with("Last line! Local"));

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
                .iter()
                .all(|d| !d.has_unsaved() && d.is_created())
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
        pump_until(|| ui.state.borrow().failed.is_empty() && !draft(&ui, &second).has_unsaved());
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
        ui.open_note(&second);
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.focus", None).unwrap();
        pump_until(|| ui.editor.left_margin() > normal_margin);
        ui.open_note(&blocks);
        pump_until(|| ui.state.borrow().parsed_generation == ui.state.borrow().parse_generation);
        assert_block_alignment(&ui, block_sample);
        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.focus", None).unwrap();
        pump_until(|| ui.editor.left_margin() == normal_margin);
        assert_block_alignment(&ui, block_sample);

        ui.new_note();
        let buffer = ui.editor.buffer();
        let text = || buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
        buffer.insert_at_cursor("- [x] done");
        pump_until(|| ui.state.borrow().parsed_generation == ui.state.borrow().parse_generation);
        assert!(ui.continue_list());
        assert_eq!(text(), "- [x] done\n- [ ] ");
        let x = |offset| ui.editor.iter_location(&buffer.iter_at_offset(offset)).x();
        assert_eq!(x(11), x(0), "new item shifts before reparse");
        assert_eq!(x(17), x(6), "new item text shifts before reparse");
        assert!(ui.continue_list());
        assert_eq!(text(), "- [x] done\n");
        buffer.insert_at_cursor("plain");
        assert!(!ui.continue_list());
        ui.open_note(&second);

        ui.window.close();
        pump_until(|| !ui.window.is_visible());
        let mut repo = storage::Repository::open(&path).unwrap();
        let (_, note, preferences) = repo.bootstrap().unwrap();
        assert_eq!(note.id, second);
        assert_eq!(note.body, stored);
        assert!(preferences.cursor > 0);
        deletion_navigation(&app);
        click_below_hidden_last_line(&app);
        rich_text_workflow(&app);
        task_checkbox_workflow(&app);
        task_checkbox_deletion_workflow(&app);
        slash_command_workflow(&app);
        selection_menu_workflow(&app);
        link_workflow(&app);
        sync_with_another_device(&app);
        if std::env::var_os("NOTEBOOK_BENCH_UI").is_some() {
            benchmark_ui(&app);
        }
        if let Some(directory) = std::env::var_os("NOTEBOOK_TEST_SCREENSHOTS") {
            design_preview(&app, std::path::Path::new(&directory));
        }
    }

    fn slash_command_workflow(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slash.db");
        let ui = test_ui(app, path.clone());
        pump_until(|| ui.state.borrow().ready && ui.editor.has_focus());
        let id = ui.state.borrow().active.clone().unwrap();
        let buffer = ui.editor.buffer();
        let source = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };
        let visible = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string()
        };
        let type_text = |text: &str| {
            buffer.begin_user_action();
            buffer.insert_at_cursor(text);
            buffer.end_user_action();
        };
        type_text("/");
        assert!(ui.slash.popover.is_visible());
        assert_eq!(ui.slash.selected(), Some(MenuAction::Block(Block::Text)));
        assert!(ui.editor.has_focus(), "typing stays in the editor");
        assert!(ui.slash_key(gdk::Key::Down));
        assert_eq!(
            ui.slash.selected(),
            Some(MenuAction::Block(Block::Heading(1)))
        );
        type_text("h2");
        assert_eq!(
            ui.slash.selected(),
            Some(MenuAction::Block(Block::Heading(2)))
        );
        assert!(ui.slash_key(gdk::Key::Return));
        assert!(!ui.slash.popover.is_visible());
        assert_eq!(source(), "## ");
        assert_eq!(visible(), "");
        type_text("Section 🌿");
        assert_eq!(visible(), "Section 🌿");
        assert!(
            buffer
                .iter_at_offset(3)
                .has_tag(&buffer.tag_table().lookup("h2").unwrap())
        );
        type_text("\n/h6");
        assert_eq!(
            ui.slash.selected(),
            Some(MenuAction::Block(Block::Heading(6)))
        );
        activate_menu_row(&ui, MenuAction::Block(Block::Heading(6)));
        assert!(source().ends_with("\n###### "));
        ui.undo_redo(true);
        assert!(source().ends_with("\n/h6"));
        assert!(
            !ui.slash.popover.is_visible(),
            "undo doesn't reopen a dismissed command"
        );
        ui.undo_redo(false);
        type_text("Small heading\n/todo");
        assert_eq!(ui.slash.selected(), Some(MenuAction::Block(Block::Tasks)));
        ui.slash_key(gdk::Key::Tab);
        type_text("Check this");
        assert!(source().ends_with("- [ ] Check this"));
        assert!(ui.continue_list());
        assert!(source().ends_with("\n- [ ] "));
        assert!(ui.continue_list());
        type_text("**Bold**");
        buffer.place_cursor(&buffer.iter_at_offset(buffer.char_count() - 2));
        type_text(" /h4");
        assert_eq!(
            ui.slash.selected(),
            Some(MenuAction::Block(Block::Heading(4)))
        );
        ui.slash_key(gdk::Key::Return);
        assert!(source().ends_with("#### **Bold** "));
        assert!(visible().ends_with("Bold "));
        type_text("/text");
        ui.slash_key(gdk::Key::Return);
        assert!(source().ends_with("\n**Bold** "));
        type_text("\n/heading");
        assert_eq!(
            ui.slash.selected(),
            Some(MenuAction::Block(Block::Heading(1)))
        );
        ui.slash_key(gdk::Key::Down);
        assert_eq!(
            ui.slash.selected(),
            Some(MenuAction::Block(Block::Heading(2)))
        );
        ui.slash_key(gdk::Key::Up);
        assert_eq!(
            ui.slash.selected(),
            Some(MenuAction::Block(Block::Heading(1)))
        );
        ui.slash_key(gdk::Key::Escape);
        let cancelled = source();
        assert!(cancelled.ends_with("/heading"));
        type_text(" ordinary");
        assert!(!ui.slash.popover.is_visible());
        type_text("\n/unknown");
        assert!(ui.slash.popover.is_visible());
        assert_eq!(ui.slash.selected(), None);
        assert!(!ui.slash_key(gdk::Key::Return));
        type_text("\nhttps://example.org/path");
        assert!(!ui.slash.popover.is_visible());
        type_text("\n```\n/h2");
        assert!(
            !ui.slash.popover.is_visible(),
            "code never triggers commands"
        );
        type_text("\n```\n/");
        assert!(ui.slash.popover.is_visible());
        buffer.place_cursor(&buffer.start_iter());
        assert!(
            !ui.slash.popover.is_visible(),
            "moving away dismisses the menu"
        );
        buffer.place_cursor(&buffer.end_iter());
        assert!(
            !ui.slash.popover.is_visible(),
            "moving back doesn't reopen it"
        );
        type_text(" /");
        assert!(ui.slash.popover.is_visible());
        if let Some(directory) = std::env::var_os("NOTEBOOK_TEST_SCREENSHOTS") {
            let directory = std::path::PathBuf::from(directory);
            let wait = Instant::now() + Duration::from_millis(150);
            pump_until(|| Instant::now() >= wait);
            let paintable = gtk::WidgetPaintable::new(Some(&ui.slash.popover));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                &snapshot,
                ui.slash.popover.width() as f64,
                ui.slash.popover.height() as f64,
            );
            let texture = ui
                .window
                .renderer()
                .unwrap()
                .render_texture(snapshot.to_node().unwrap(), None);
            texture
                .save_to_png(directory.join("slash-menu.png"))
                .unwrap();
        }
        ui.new_note();
        assert!(!ui.slash.popover.is_visible());
        ui.open_note(&id);
        assert_eq!(source(), draft(&ui, &id).text());
        ui.flush_drafts();
        pump_until(|| !draft(&ui, &id).has_unsaved());
        let saved = source();
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
        let mut repo = storage::Repository::open(&path).unwrap();
        assert_eq!(repo.bootstrap().unwrap().1.body, saved);
    }

    fn selection_menu_workflow(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("selection.db");
        let ui = test_ui(app, path.clone());
        pump_until(|| ui.state.borrow().ready && ui.editor.has_focus());
        let id = ui.state.borrow().active.clone().unwrap();
        let buffer = ui.editor.buffer();
        let source = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };
        let visible = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string()
        };
        let select = |start, end| {
            buffer.place_cursor(&buffer.iter_at_offset(start));
            // Let X11 process the old primary selection before simulating a
            // separate user gesture that selects text again.
            let settled = Instant::now() + Duration::from_millis(20);
            pump_until(|| Instant::now() >= settled);
            buffer.select_range(&buffer.iter_at_offset(end), &buffer.iter_at_offset(start));
            pump_until(|| ui.slash.popover.is_visible());
            assert!(
                ui.editor.has_focus(),
                "selection menu keeps the editor focused"
            );
            assert!(ui.slash.query.borrow().is_none());
        };
        buffer.begin_user_action();
        buffer.insert_at_cursor("Hello 🌿 world\nSecond line\nUntouched");
        buffer.end_user_action();
        select(0, 7);
        assert_eq!(ui.slash.selected(), Some(MenuAction::Inline(Format::Bold)));
        activate_menu_row(&ui, MenuAction::Inline(Format::Bold));
        assert_eq!(source(), "**Hello 🌿** world\nSecond line\nUntouched");
        assert_eq!(visible(), "Hello 🌿 world\nSecond line\nUntouched");
        assert!(
            buffer.has_selection(),
            "clicking the menu preserves selected text"
        );
        assert!(!ui.slash.popover.is_visible());
        ui.refresh_slash(false);
        assert!(
            !ui.slash.popover.is_visible(),
            "applying doesn't reopen the menu"
        );
        ui.undo_redo(true);
        assert_eq!(source(), "Hello 🌿 world\nSecond line\nUntouched");
        ui.undo_redo(false);
        assert_eq!(source(), "**Hello 🌿** world\nSecond line\nUntouched");
        // Toggle existing formatting off through the selection menu.
        select(2, 9);
        activate_menu_row(&ui, MenuAction::Inline(Format::Bold));
        assert_eq!(source(), "Hello 🌿 world\nSecond line\nUntouched");
        for (format, expected) in [
            (Format::Italic, "*Hello 🌿* world\nSecond line\nUntouched"),
            (Format::Strike, "~~Hello 🌿~~ world\nSecond line\nUntouched"),
        ] {
            select(0, 7);
            activate_menu_row(&ui, MenuAction::Inline(format));
            assert_eq!(source(), expected);
            assert_eq!(visible(), "Hello 🌿 world\nSecond line\nUntouched");
            ui.undo_redo(true);
        }
        // Selection ends precisely at the third line's start.
        select(2, 26);
        activate_menu_row(&ui, MenuAction::Block(Block::Heading(3)));
        assert_eq!(source(), "### Hello 🌿 world\n### Second line\nUntouched");
        assert_eq!(visible(), "Hello 🌿 world\nSecond line\nUntouched");
        assert_eq!(
            buffer
                .selection_bounds()
                .map(|(a, b)| a.offset()..b.offset()),
            Some(6..34)
        );
        ui.undo_redo(true);
        select(0, 26);
        activate_menu_row(&ui, MenuAction::Block(Block::Numbers));
        assert_eq!(source(), "1. Hello 🌿 world\n2. Second line\nUntouched");
        ui.undo_redo(true);
        select(0, 7);
        let before = source();
        assert!(ui.slash_key(gdk::Key::Escape));
        ui.render_now(&id);
        assert_eq!(source(), before);
        assert!(!ui.slash.popover.is_visible(), "Escape stays dismissed");
        assert!(buffer.has_selection());
        // Extending a selection shows the menu again; collapsing hides it.
        buffer.select_range(&buffer.iter_at_offset(8), &buffer.start_iter());
        pump_until(|| ui.slash.popover.is_visible());
        buffer.place_cursor(&buffer.start_iter());
        assert!(!ui.slash.popover.is_visible());
        select(0, 7);
        buffer.begin_user_action();
        buffer.delete_selection(true, true);
        buffer.insert_at_cursor("New");
        buffer.end_user_action();
        assert_eq!(source(), "New world\nSecond line\nUntouched");
        assert!(!ui.slash.popover.is_visible());
        ui.undo_redo(true);
        select(0, 7);
        ui.editor.set_editable(false);
        ui.refresh_slash(false);
        assert!(!ui.slash.popover.is_visible());
        let before = source();
        ui.run_menu_action(MenuAction::Block(Block::Heading(1)));
        assert_eq!(source(), before, "read-only selection can't change text");
        ui.editor.set_editable(true);
        select(0, 7);
        ui.search.grab_focus();
        assert!(!ui.slash.popover.is_visible());
        ui.editor.grab_focus();
        ui.refresh_slash(false);
        assert!(
            !ui.slash.popover.is_visible(),
            "returning focus doesn't reopen it"
        );
        select(0, 7);
        if let Some(directory) = std::env::var_os("NOTEBOOK_TEST_SCREENSHOTS") {
            let wait = Instant::now() + Duration::from_millis(150);
            pump_until(|| Instant::now() >= wait);
            let paintable = gtk::WidgetPaintable::new(Some(&ui.slash.popover));
            let snapshot = gtk::Snapshot::new();
            paintable.snapshot(
                &snapshot,
                ui.slash.popover.width() as f64,
                ui.slash.popover.height() as f64,
            );
            ui.window
                .renderer()
                .unwrap()
                .render_texture(snapshot.to_node().unwrap(), None)
                .save_to_png(std::path::Path::new(&directory).join("selection-menu.png"))
                .unwrap();
        }
        ui.new_note();
        assert!(!ui.slash.popover.is_visible());
        ui.open_note(&id);
        assert!(
            !ui.slash.popover.is_visible(),
            "switching notes never restores a stale menu"
        );
        assert_eq!(source(), draft(&ui, &id).text());
        ui.flush_drafts();
        pump_until(|| !draft(&ui, &id).has_unsaved());
        let saved = source();
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
        let mut repo = storage::Repository::open(&path).unwrap();
        assert_eq!(repo.bootstrap().unwrap().1.body, saved);
    }

    // Exercise native pointer activation when running under the test X server.
    fn link_workflow(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let ui = test_ui(app, dir.path().join("links.db"));
        pump_until(|| ui.state.borrow().ready && ui.editor.has_focus());
        let buffer = ui.editor.buffer();
        let source = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };
        let visible = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string()
        };
        let type_text = |text: &str| {
            buffer.begin_user_action();
            buffer.insert_at_cursor(text);
            buffer.end_user_action();
        };
        let paste = |text: &str| {
            let before = source();
            ui.editor.clipboard().set_text(text);
            ui.editor.emit_by_name::<()>("paste-clipboard", &[]);
            pump_until(|| source() != before);
        };

        type_text("See ");
        paste("https://example.org/🌿");
        assert_eq!(source(), "See <https://example.org/🌿>");
        assert_eq!(visible(), "See https://example.org/🌿");
        assert_eq!(
            ui.state.borrow().document.link_at(6),
            Some("https://example.org/🌿")
        );
        type_text(" and ");
        paste("plain text");
        assert!(source().ends_with("> and plain text"));
        ui.undo_redo(true);
        assert_eq!(source(), "See <https://example.org/🌿>");
        ui.undo_redo(true);
        assert_eq!(source(), "See ");

        // Pasting a URL over selected text links that text.
        buffer.set_text("");
        type_text("Read the docs today");
        buffer.select_range(&buffer.iter_at_offset(9), &buffer.iter_at_offset(13));
        paste("https://docs.rs");
        assert_eq!(source(), "Read the [docs](https://docs.rs) today");
        assert_eq!(visible(), "Read the docs today");
        assert!(!buffer.has_selection());

        // The slash command asks for the URL to insert at the caret.
        buffer.set_text("");
        type_text("Go to /lin");
        assert_eq!(ui.slash.selected(), Some(MenuAction::Link));
        assert!(ui.slash_key(gdk::Key::Return));
        assert!(ui.link.popover.is_visible());
        assert_eq!(source(), "Go to ");
        ui.link.entry.set_text("example.org");
        ui.link.entry.emit_activate();
        assert!(!ui.link.popover.is_visible());
        assert_eq!(source(), "Go to <https://example.org>");
        assert_eq!(buffer.cursor_position(), buffer.char_count());
        pump_until(|| ui.editor.has_focus());

        // Ctrl+K links the selection. Cancelling leaves the text alone.
        buffer.set_text("");
        type_text("one two");
        buffer.select_range(&buffer.iter_at_offset(4), &buffer.iter_at_offset(7));
        ui.prompt_link();
        ui.link.popover.popdown();
        assert_eq!(source(), "one two");
        pump_until(|| ui.editor.has_focus());
        ui.prompt_link();
        ui.link.entry.set_text("me@example.org");
        ui.link.entry.emit_activate();
        assert_eq!(source(), "one [two](mailto:me@example.org)");
        assert_eq!(visible(), "one two");
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
    }

    fn activate_menu_row(ui: &Ui, action: MenuAction) {
        for _ in 0..slash_menu::COMMANDS.len() {
            if ui.slash.selected() == Some(action) {
                break;
            }
            ui.slash.move_selection(1);
        }
        assert_eq!(ui.slash.selected(), Some(action));
        let row = ui.slash.list.selected_row().unwrap();
        if std::env::var_os("NOTEBOOK_TEST_POINTER").is_some() {
            let wait = Instant::now() + Duration::from_millis(150);
            pump_until(|| Instant::now() >= wait);
            let output = std::process::Command::new("xdotool")
                .args(["search", "--onlyvisible", "--name", ".*"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let windows = String::from_utf8(output.stdout).unwrap();
            let surface = ui.slash.popover.surface().unwrap();
            let geometry = windows
                .lines()
                .find_map(|window| {
                    let output = std::process::Command::new("xdotool")
                        .args(["getwindowgeometry", "--shell", window])
                        .output()
                        .unwrap();
                    let values: HashMap<_, _> = String::from_utf8(output.stdout)
                        .unwrap()
                        .lines()
                        .filter_map(|line| {
                            let (key, value) = line.split_once('=')?;
                            Some((key.to_owned(), value.parse::<i32>().ok()?))
                        })
                        .collect();
                    (values.get("WIDTH") == Some(&surface.width())
                        && values.get("HEIGHT") == Some(&surface.height()))
                    .then_some(values)
                })
                .expect("the popup's native X11 window");
            let bounds = row.compute_bounds(&ui.slash.popover).unwrap();
            let (dx, dy) = ui.slash.popover.surface_transform();
            let x = geometry["X"] + (bounds.x() + bounds.width() / 2.0) as i32 - dx as i32;
            let y = geometry["Y"] + (bounds.y() + bounds.height() / 2.0) as i32 - dy as i32;
            assert!(
                std::process::Command::new("xdotool")
                    .args(["mousemove", &x.to_string(), &y.to_string(), "click", "1"])
                    .status()
                    .unwrap()
                    .success()
            );
            pump_until(|| !ui.slash.popover.is_visible());
        } else {
            ui.slash.list.emit_by_name::<()>("row-activated", &[&row]);
        }
    }

    fn task_checkbox_workflow(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.db");
        let ui = test_ui(app, path.clone());
        pump_until(|| ui.state.borrow().ready);
        let id = ui.state.borrow().active.clone().unwrap();
        let buffer = ui.editor.buffer();
        let original = "# Tasks 🌿\n\n- [ ] Buy groceries\n  * [x] Nested task\n+ [X] Completed task\n\n1. [ ] Numbered task\n\n`- [ ] inline code`\n\n```\n- [ ] fenced code\n```\n\n[ ] Ordinary text";
        buffer.begin_user_action();
        buffer.insert_at_cursor(original);
        buffer.end_user_action();
        let source = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };
        let buttons = || ui.task_checkboxes.buttons();
        let wait = Instant::now() + Duration::from_millis(100);
        pump_until(|| Instant::now() >= wait);
        let controls = buttons();
        assert_eq!(controls.len(), 4, "code and plain text are not task items");
        assert_eq!(
            controls.iter().map(|b| b.is_active()).collect::<Vec<_>>(),
            [false, true, true, false]
        );
        assert_eq!(source(), original, "rendering preserves Markdown");
        assert_eq!(draft(&ui, &id).text(), original);
        let first = controls[0].clone();
        let cursor = buffer.cursor_position();
        first.grab_focus();
        let checked = original.replacen("[ ]", "[x]", 1);
        if std::env::var_os("NOTEBOOK_TEST_POINTER").is_some() {
            let output = std::process::Command::new("xdotool")
                .args(["search", "--onlyvisible", "--name", "^Notebook$"])
                .output()
                .unwrap();
            assert!(output.status.success());
            let windows = String::from_utf8(output.stdout).unwrap();
            let window = windows.lines().last().unwrap();
            let bounds = first.compute_bounds(&ui.window).unwrap();
            let x = (bounds.x() + bounds.width() / 2.0) as i32;
            let y = (bounds.y() + bounds.height() / 2.0) as i32;
            assert!(
                std::process::Command::new("xdotool")
                    .args([
                        "mousemove",
                        "--window",
                        window,
                        &x.to_string(),
                        &y.to_string(),
                        "click",
                        "1"
                    ])
                    .status()
                    .unwrap()
                    .success()
            );
            pump_until(|| source() == checked);
        } else {
            first.emit_by_name::<()>("activate", &[]);
        }
        assert_eq!(source(), checked);
        assert_eq!(draft(&ui, &id).text(), checked);
        assert_eq!(buffer.cursor_position(), cursor);
        assert!(first.has_focus(), "re-rendering retains keyboard focus");
        assert!(first.is_active());
        ui.undo_redo(true);
        assert_eq!(source(), original, "a toggle has its own undo step");
        assert!(!first.is_active());
        ui.undo_redo(false);
        assert_eq!(source(), checked);
        assert!(first.is_active());

        // A widget follows source edits, including non-ASCII character offsets.
        buffer.begin_user_action();
        buffer.insert(&mut buffer.start_iter(), "🌿\n\n");
        buffer.end_user_action();
        if std::env::var_os("NOTEBOOK_TEST_POINTER").is_some() {
            first.grab_focus();
            assert!(
                std::process::Command::new("xdotool")
                    .args(["key", "space"])
                    .status()
                    .unwrap()
                    .success()
            );
            pump_until(|| !first.is_active());
        } else {
            first.emit_by_name::<()>("activate", &[]);
        }
        assert_eq!(source(), format!("🌿\n\n{original}"));
        if std::env::var_os("NOTEBOOK_TEST_POINTER").is_some() {
            let line_end = source().find("\n  *").unwrap();
            let at = source()[..line_end].chars().count() as i32;
            buffer.place_cursor(&buffer.iter_at_offset(at));
            first.grab_focus();
            for checked in [true, false] {
                assert!(
                    std::process::Command::new("xdotool")
                        .args(["key", "Return"])
                        .status()
                        .unwrap()
                        .success()
                );
                pump_until(|| first.is_active() == checked);
            }
            assert_eq!(source(), format!("🌿\n\n{original}"));
            assert_eq!(buffer.cursor_position(), at);
        }
        // Preserve a backwards selection when checking a different item.
        buffer.select_range(&buffer.iter_at_offset(4), &buffer.iter_at_offset(9));
        controls[2].emit_by_name::<()>("activate", &[]);
        assert_eq!(buffer.cursor_position(), 4);
        assert_eq!(buffer.iter_at_mark(&buffer.selection_bound()).offset(), 9);
        assert!(source().contains("+ [ ] Completed task"));
        ui.flush_drafts();
        pump_until(|| !draft(&ui, &id).has_unsaved());
        let saved = source();

        ui.new_note();
        assert!(buttons().is_empty(), "controls belong to the active note");
        assert!(controls.iter().all(|button| button.parent().is_none()));
        first.emit_by_name::<()>("activate", &[]);
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
        ui.open_note(&id);
        pump_until(|| ui.state.borrow().active.as_deref() == Some(&id));
        assert_eq!(buttons().len(), 4);
        ui.trash_or_restore();
        pump_until(|| ui.state.borrow().active.as_deref() != Some(&id));
        ui.open_note(&id);
        pump_until(|| ui.state.borrow().active.as_deref() == Some(&id));
        assert!(buttons().iter().all(|button| !button.is_sensitive()));
        ui.trash_or_restore();
        pump_until(|| ui.editor.is_editable());
        assert!(buttons().iter().all(|button| button.is_sensitive()));
        if let Some(directory) = std::env::var_os("NOTEBOOK_TEST_SCREENSHOTS") {
            let settings = gtk::Settings::default().unwrap();
            for (dark, name) in [(false, "tasks-light.png"), (true, "tasks-dark.png")] {
                settings.set_gtk_application_prefer_dark_theme(dark);
                let wait = Instant::now() + Duration::from_millis(100);
                pump_until(|| Instant::now() >= wait);
                screenshot(&ui, &std::path::Path::new(&directory).join(name));
            }
            settings.set_gtk_application_prefer_dark_theme(false);
        }
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
        let repo = storage::Repository::open(&path).unwrap();
        assert_eq!(repo.load(&id).unwrap().unwrap().body, saved);
    }

    fn task_checkbox_deletion_workflow(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task-deletion.db");
        let ui = test_ui(app, path.clone());
        pump_until(|| ui.state.borrow().ready);
        let id = ui.state.borrow().active.clone().unwrap();
        let buffer = ui.editor.buffer();
        let source = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };
        let completed = "- [x] Tune PID gains for new actuator firmware";
        buffer.begin_user_action();
        buffer.insert_at_cursor(completed);
        buffer.end_user_action();
        assert!(ui.continue_list());
        let before = format!("{completed}\n- [ ] ");
        assert_eq!(source(), before);
        assert_eq!(ui.task_checkboxes.buttons().len(), 2);
        ui.state
            .borrow_mut()
            .drafts
            .get_mut(&id)
            .unwrap()
            .undo_boundary();
        ui.editor.grab_focus();
        if std::env::var_os("NOTEBOOK_TEST_POINTER").is_some() {
            assert!(
                std::process::Command::new("xdotool")
                    .args(["key", "BackSpace"])
                    .status()
                    .unwrap()
                    .success()
            );
            pump_until(|| source() != before);
        } else {
            ui.editor.emit_by_name::<()>("backspace", &[]);
        }
        let after = format!("{completed}\n");
        assert_eq!(
            source(),
            after,
            "one Backspace removes the entire empty checkbox"
        );
        assert_eq!(draft(&ui, &id).text(), after);
        assert_eq!(buffer.cursor_position() as usize, after.chars().count());
        assert_eq!(ui.task_checkboxes.buttons().len(), 1);
        ui.undo_redo(true);
        assert_eq!(source(), before);
        assert_eq!(ui.task_checkboxes.buttons().len(), 2);
        ui.undo_redo(false);
        assert_eq!(source(), after);

        buffer.begin_user_action();
        buffer.insert_at_cursor("- [ ] 🌿");
        buffer.end_user_action();
        ui.editor.emit_by_name::<()>("backspace", &[]);
        assert_eq!(source(), format!("{after}- [ ] "));
        ui.editor.emit_by_name::<()>("backspace", &[]);
        assert_eq!(source(), after);

        // Forward Delete and cutting a partial marker also remove it as a unit.
        for cut in [false, true] {
            let prefix = source();
            let start = buffer.char_count();
            buffer.begin_user_action();
            buffer.insert_at_cursor("- [X] Keep this text");
            buffer.end_user_action();
            if cut {
                buffer.select_range(
                    &buffer.iter_at_offset(start + 5),
                    &buffer.iter_at_offset(start + 4),
                );
                ui.editor.emit_by_name::<()>("cut-clipboard", &[]);
            } else {
                buffer.place_cursor(&buffer.iter_at_offset(start));
                ui.editor
                    .emit_by_name::<()>("delete-from-cursor", &[&gtk::DeleteType::Chars, &1i32]);
            }
            assert_eq!(source(), format!("{prefix}Keep this text"));
            assert_eq!(ui.task_checkboxes.buttons().len(), 1);
            assert_eq!(draft(&ui, &id).text(), source());
            buffer.place_cursor(&buffer.end_iter());
            buffer.begin_user_action();
            buffer.insert_at_cursor("\n");
            buffer.end_user_action();
        }
        let saved = source();
        ui.flush_drafts();
        pump_until(|| !draft(&ui, &id).has_unsaved());
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
        let repo = storage::Repository::open(&path).unwrap();
        assert_eq!(repo.load(&id).unwrap().unwrap().body, saved);
    }

    fn rich_text_workflow(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rich.db");
        let ui = test_ui(app, path.clone());
        pump_until(|| ui.state.borrow().ready);
        let id = ui.state.borrow().active.clone().unwrap();
        let buffer = ui.editor.buffer();
        let source = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };
        let visible = || {
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), false)
                .to_string()
        };
        buffer.begin_user_action();
        buffer.insert_at_cursor("Hello 🌿 world");
        buffer.end_user_action();
        buffer.select_range(&buffer.iter_at_offset(7), &buffer.iter_at_offset(0));
        ui.format_selection(Format::Bold);
        assert_eq!(source(), "**Hello 🌿** world");
        assert_eq!(visible(), "Hello 🌿 world");
        assert!(
            buffer
                .iter_at_offset(2)
                .has_tag(&buffer.tag_table().lookup("strong").unwrap())
        );
        ui.undo_redo(true);
        assert_eq!(
            source(),
            "Hello 🌿 world",
            "formatting has its own undo step"
        );
        ui.undo_redo(false);
        assert_eq!(source(), "**Hello 🌿** world");
        buffer.place_cursor(&buffer.iter_at_offset(9));
        buffer.begin_user_action();
        buffer.insert_at_cursor("!");
        buffer.end_user_action();
        assert_eq!(visible(), "Hello 🌿! world");
        assert_eq!(source(), "**Hello 🌿!** world");
        ui.editor.emit_by_name::<()>("backspace", &[]);
        assert_eq!(visible(), "Hello 🌿 world");
        assert_eq!(source(), "**Hello 🌿** world");
        buffer.place_cursor(&buffer.iter_at_offset(9));
        buffer.begin_user_action();
        buffer.insert_at_cursor(" ");
        buffer.end_user_action();
        assert_eq!(visible(), "Hello 🌿  world");
        assert_eq!(source(), "**Hello 🌿**  world");
        ui.editor.emit_by_name::<()>("backspace", &[]);
        assert_eq!(source(), "**Hello 🌿** world");
        // A selection starting outside bold and ending inside it must leave
        // the unselected text formatted, with no unmatched delimiters.
        buffer.select_range(&buffer.iter_at_offset(8), &buffer.start_iter());
        buffer.begin_user_action();
        buffer.delete_selection(true, true);
        buffer.insert_at_cursor("New ");
        buffer.end_user_action();
        assert_eq!(visible(), "New 🌿 world");
        assert!(!visible().contains('*'));
        assert_eq!(draft(&ui, &id).text(), source());
        buffer.select_range(&buffer.end_iter(), &buffer.start_iter());
        buffer.begin_user_action();
        buffer.delete_selection(true, true);
        buffer.end_user_action();
        assert_eq!(source(), "");
        buffer.begin_user_action();
        buffer.insert_at_cursor("**🌿**");
        buffer.end_user_action();
        buffer.place_cursor(&buffer.end_iter());
        ui.editor.emit_by_name::<()>("backspace", &[]);
        assert_eq!(
            source(),
            "",
            "backspace after hidden closing syntax deletes the visible character"
        );
        ui.format_selection(Format::Bullets);
        assert_eq!(source(), "- ");
        buffer.begin_user_action();
        buffer.insert_at_cursor("First");
        buffer.end_user_action();
        assert!(ui.continue_list());
        assert_eq!(source(), "- First\n- ");
        assert!(ui.continue_list());
        assert_eq!(source(), "- First\n");
        buffer.select_range(&buffer.iter_at_offset(7), &buffer.iter_at_offset(2));
        ui.format_selection(Format::Italic);
        assert_eq!(source(), "- *First*\n");
        assert_eq!(visible(), "- First\n");
        ui.flush_drafts();
        pump_until(|| !draft(&ui, &id).has_unsaved());
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
        let mut repo = storage::Repository::open(&path).unwrap();
        assert_eq!(repo.bootstrap().unwrap().1.body, "- *First*\n");
    }

    fn sync_with_another_device(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let ui = test_ui(app, dir.path().join("window.db"));
        pump_until(|| ui.state.borrow().ready);

        let laptop_db = dir.path().join("laptop.db");
        let laptop_config = dir.path().join("laptop.json");
        let (laptop, _laptop_events) = storage::spawn_worker(laptop_db.clone());
        laptop.send(Command::Initialize).unwrap();
        let (statuses, laptop_statuses) = mpsc::channel();
        let laptop_sync = sync::spawn(
            laptop_config,
            laptop_db.clone(),
            "Laptop".into(),
            laptop.clone(),
            move |status| {
                let _ = statuses.send(status);
            },
            sync::Options {
                mdns: false,
                ..Default::default()
            },
        );
        let laptop_status = RefCell::new(sync::Status::default());
        let laptop_now = |check: &dyn Fn(&sync::Status) -> bool| {
            if let Some(status) = laptop_statuses.try_iter().last() {
                *laptop_status.borrow_mut() = status;
            }
            check(&laptop_status.borrow())
        };
        laptop_sync.send(Control::SetEnabled(true));

        gtk::prelude::WidgetExt::activate_action(&ui.window, "win.sync", None).unwrap();
        let dialog = || ui.sync_dialog.borrow().as_ref().map(|d| d.window.clone());
        assert_eq!(dialog().unwrap().title().as_deref(), Some("Sync"));
        assert!(!sync_dialog(&ui, |d| d.pairing_box.is_sensitive()));
        sync_dialog(&ui, |d| d.enabled.set_active(true));
        let reachable = |s: &sync::Status| {
            s.addr
                .as_ref()
                .is_some_and(|a| a.ip_addrs().next().is_some())
        };
        pump_until(|| reachable(&ui.sync_status.borrow()) && laptop_now(&reachable));
        assert!(sync_dialog(&ui, |d| d.pairing_box.is_sensitive()));
        // Stand in for the local network.
        ui.sync.send(Control::Introduce(
            laptop_status.borrow().addr.clone().unwrap(),
        ));
        laptop_sync.send(Control::Introduce(
            ui.sync_status.borrow().addr.clone().unwrap(),
        ));

        ui.sync.send(Control::StartPairing);
        pump_until(|| matches!(ui.sync_status.borrow().pairing, Pairing::Showing(_)));
        let code = sync_dialog(&ui, |d| d.code.text().to_string());
        assert_eq!(code.len(), 7, "{code:?}");
        assert_eq!(
            sync_dialog(&ui, |d| d.pairing.visible_child_name()).as_deref(),
            Some("showing")
        );
        let shots = std::env::var_os("NOTEBOOK_TEST_SCREENSHOTS").map(std::path::PathBuf::from);
        let shoot = |name: &str| {
            if let Some(directory) = &shots {
                let wait = Instant::now() + Duration::from_millis(150);
                pump_until(|| Instant::now() >= wait);
                screenshot_window(&dialog().unwrap(), &directory.join(name));
            }
        };
        shoot("sync-code.png");
        laptop_sync.send(Control::JoinPairing(code));
        pump_until(|| {
            ui.sync_status
                .borrow()
                .devices
                .iter()
                .any(|d| d.name == "Laptop" && d.connected)
                && laptop_now(&|s| s.devices.iter().any(|d| d.connected))
        });
        assert_eq!(
            sync_dialog(&ui, |d| d.pairing_note.text()),
            "Paired with Laptop."
        );
        assert!(ui.sync_button.has_css_class("sync-active"));
        shoot("sync-paired.png");

        let note = Note::blank();
        laptop.send(Command::Create(note.clone())).unwrap();
        let save = |body: &str| {
            laptop
                .send(Command::Save {
                    id: note.id.clone(),
                    body: body.into(),
                    sequence: 1,
                })
                .unwrap()
        };
        save("From the laptop");
        pump_until(|| {
            ui.state
                .borrow()
                .notes
                .iter()
                .any(|n| n.label == "From the laptop")
        });
        dialog().unwrap().close();
        ui.open_note(&note.id);
        let text = || {
            let buffer = ui.editor.buffer();
            buffer
                .text(&buffer.start_iter(), &buffer.end_iter(), true)
                .to_string()
        };
        pump_until(|| text() == "From the laptop");
        save("From the laptop, edited there");
        pump_until(|| text() == "From the laptop, edited there");
        let buffer = ui.editor.buffer();
        buffer.place_cursor(&buffer.end_iter());
        buffer.insert_at_cursor(" and here");
        let laptop_body = || {
            storage::Repository::open(&laptop_db)
                .unwrap()
                .load(&note.id)
                .unwrap()
                .map(|n| n.body)
        };
        pump_until(|| laptop_body().as_deref() == Some("From the laptop, edited there and here"));
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
    }

    fn sync_dialog<T>(ui: &Ui, read: impl FnOnce(&SyncDialog) -> T) -> T {
        read(
            ui.sync_dialog
                .borrow()
                .as_ref()
                .expect("the Sync window is open"),
        )
    }

    fn click_below_hidden_last_line(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("click.db");
        let mut repo = storage::Repository::open(&path).unwrap();
        let mut note = Note::blank();
        note.body = "Intro\n\nVisit [the site](https://example.org) today".into();
        repo.create_note(&note).unwrap();
        repo.save_preferences(&Preferences {
            selected_note: Some(note.id.clone()),
            ..Default::default()
        })
        .unwrap();
        drop(repo);
        let ui = test_ui(app, path);
        pump_until(|| {
            ui.state.borrow().ready
                && ui.state.borrow().parsed_generation > 0
                && ui.state.borrow().parsed_generation == ui.state.borrow().parse_generation
        });
        let buffer = ui.editor.buffer();
        buffer.place_cursor(&buffer.start_iter());
        let hidden = buffer.tag_table().lookup("hidden").unwrap();
        assert!(
            buffer
                .iter_at_offset(buffer.char_count() - 8)
                .has_tag(&hidden)
        );
        let (x, y) = ui.editor.window_to_buffer_coords(
            gtk::TextWindowType::Widget,
            ui.editor.width() - 1,
            ui.editor.height() - 1,
        );
        let _ = ui.editor.iter_at_location(x, y);
        ui.window.close();
        pump_until(|| !ui.window.is_visible());
    }

    fn deletion_navigation(app: &gtk::Application) {
        let dir = tempfile::tempdir().unwrap();
        let ui = test_ui(app, dir.path().join("deletion.db"));
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
        let wrapped = || {
            (content..content + source[item..].find('\n').unwrap() as i32)
                .map(|offset| ui.editor.iter_location(&buffer.iter_at_offset(offset)))
                .find(|location| location.y() > first.y())
        };
        pump_until(|| wrapped().is_some());
        let wrapped = wrapped().unwrap();
        if let Some(directory) = std::env::var_os("NOTEBOOK_TEST_SCREENSHOTS") {
            let wait = Instant::now() + Duration::from_millis(100);
            pump_until(|| Instant::now() >= wait);
            screenshot(
                ui,
                &std::path::Path::new(&directory).join("tasks-wrapping.png"),
            );
        }
        assert_eq!(wrapped.x(), first.x(), "wrapped task line is not aligned");
        assert_eq!(x("- [ ] Record"), prose, "list item is indented past prose");
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
        let ui = test_ui(app, path);
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
        screenshot_window(ui.window.upcast_ref(), path);
    }

    fn screenshot_window(window: &gtk::Window, path: &std::path::Path) {
        let paintable = gtk::WidgetPaintable::new(Some(window));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
        let node = snapshot.to_node().unwrap();
        let texture = window.renderer().unwrap().render_texture(&node, None);
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
        let ui = test_ui(app, path);
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
