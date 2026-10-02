//! The records and enums Kotlin sees, and their conversions from core.
//!
//! Text positions are `i32` UTF-16 units, as Java counts them. Generations,
//! counts and timestamps are `i64`. Kotlin's unsigned types are awkward, so
//! none appear here.
use notebook_core::{editor, model, storage, sync};

#[derive(Clone, Debug, uniffi::Record)]
pub struct CoreConfig {
    /// `filesDir/notebook.db`
    pub database_path: String,
    /// `noBackupFilesDir/sync.json`: it holds this device's private key, so a
    /// restored backup must never bring it to another device.
    pub sync_config_path: String,
    /// What other devices call this one until someone picks a name.
    pub device_name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Error)]
pub enum CoreError {
    /// Kotlin passed a position outside the draft, or a note with no draft.
    /// It should never happen; if it does, reload the note from storage. The
    /// field isn't called `message`, which clashes with `Throwable.message`.
    OutOfSync { reason: String },
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::OutOfSync { reason } => f.write_str(reason),
        }
    }
}

impl std::error::Error for CoreError {}

impl From<editor::OutOfSync> for CoreError {
    fn from(error: editor::OutOfSync) -> Self {
        CoreError::OutOfSync {
            reason: error.to_string(),
        }
    }
}

/// A note without its text, which comes from the draft.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NoteInfo {
    pub id: String,
    pub notebook_id: String,
    pub deleted: bool,
}

impl From<&model::Note> for NoteInfo {
    fn from(note: &model::Note) -> Self {
        Self {
            id: note.id.clone(),
            notebook_id: note.notebook_id.clone(),
            deleted: note.deleted,
        }
    }
}

impl From<NoteInfo> for model::Note {
    fn from(note: NoteInfo) -> Self {
        Self {
            id: note.id,
            notebook_id: note.notebook_id,
            body: String::new(),
            deleted: note.deleted,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Notebook {
    pub id: String,
    pub name: String,
}

impl From<model::Notebook> for Notebook {
    fn from(book: model::Notebook) -> Self {
        Self {
            id: book.id,
            name: book.name,
        }
    }
}

pub fn notebooks(books: Vec<model::Notebook>) -> Vec<Notebook> {
    books.into_iter().map(Into::into).collect()
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NoteSummary {
    pub id: String,
    pub label: String,
    pub preview: String,
    /// Milliseconds since the Unix epoch.
    pub updated_at: i64,
}

impl From<model::NoteSummary> for NoteSummary {
    fn from(note: model::NoteSummary) -> Self {
        Self {
            id: note.id,
            label: note.label,
            preview: note.preview,
            updated_at: note.updated_at,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum Filter {
    Notebook { id: String },
    All,
    Trash,
}

impl From<Filter> for model::Filter {
    fn from(filter: Filter) -> Self {
        match filter {
            Filter::Notebook { id } => model::Filter::Notebook(id),
            Filter::All => model::Filter::All,
            Filter::Trash => model::Filter::Trash,
        }
    }
}

impl From<model::Filter> for Filter {
    fn from(filter: model::Filter) -> Self {
        match filter {
            model::Filter::Notebook(id) => Filter::Notebook { id },
            model::Filter::All => Filter::All,
            model::Filter::Trash => Filter::Trash,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct NoteCount {
    pub filter: Filter,
    pub count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Mutation {
    CreateNotebook { id: String, name: String },
    RenameNotebook { id: String, name: String },
    DeleteNotebook { id: String },
    Move { id: String, notebook_id: String },
    Trash { id: String },
    Restore { id: String },
}

impl From<Mutation> for storage::Mutation {
    fn from(mutation: Mutation) -> Self {
        match mutation {
            Mutation::CreateNotebook { id, name } => storage::Mutation::CreateNotebook { id, name },
            Mutation::RenameNotebook { id, name } => storage::Mutation::RenameNotebook { id, name },
            Mutation::DeleteNotebook { id } => storage::Mutation::DeleteNotebook { id },
            Mutation::Move { id, notebook_id } => storage::Mutation::Move { id, notebook_id },
            Mutation::Trash { id } => storage::Mutation::Trash { id },
            Mutation::Restore { id } => storage::Mutation::Restore { id },
        }
    }
}

impl From<storage::Mutation> for Mutation {
    fn from(mutation: storage::Mutation) -> Self {
        match mutation {
            storage::Mutation::CreateNotebook { id, name } => Mutation::CreateNotebook { id, name },
            storage::Mutation::RenameNotebook { id, name } => Mutation::RenameNotebook { id, name },
            storage::Mutation::DeleteNotebook { id } => Mutation::DeleteNotebook { id },
            storage::Mutation::Move { id, notebook_id } => Mutation::Move { id, notebook_id },
            storage::Mutation::Trash { id } => Mutation::Trash { id },
            storage::Mutation::Restore { id } => Mutation::Restore { id },
        }
    }
}

/// What storage reports, mirroring `storage::Event`.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreEvent {
    /// The database is open. `note` is the note to show first; open its draft
    /// with `snapshot`. `selected_note` and `cursor` are the preferences saved
    /// last time.
    Ready {
        default_notebook_id: String,
        notebooks: Vec<Notebook>,
        note: NoteInfo,
        snapshot: Vec<u8>,
        selected_note: Option<String>,
        cursor: i32,
    },
    Listed {
        notes: Vec<NoteSummary>,
        counts: Vec<NoteCount>,
        generation: i64,
    },
    /// `note` is `None` if it no longer exists.
    Loaded {
        note: Option<NoteInfo>,
        snapshot: Vec<u8>,
        generation: i64,
    },
    Created {
        id: String,
    },
    Saved {
        id: String,
    },
    Mutated {
        mutation: Mutation,
        notebooks: Vec<Notebook>,
    },
    /// Changes to a note. If its draft is open, call `import_delta` on the
    /// main thread and apply the returned edits. Import every one, in order,
    /// including the echoes of the draft's own saves.
    NoteDelta {
        id: String,
        delta: Vec<u8>,
    },
    /// Another device moved, trashed or restored notes, and maybe changed
    /// the notebooks. `trashed` lists the open drafts it moved to the trash.
    Remote {
        trashed: Vec<String>,
        notebooks: Option<Vec<Notebook>>,
    },
    /// Every command sent before `flush` is done.
    Flushed,
    /// `retryable` errors are kept, and `retry` sends them again. The open
    /// drafts keep their text either way.
    Error {
        operation: Operation,
        message: String,
        retryable: bool,
    },
}

/// What storage was doing when it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Operation {
    Open,
    List,
    Load,
    Create,
    Save,
    /// A `Mutation`, whose message is already fit to show.
    Change,
    Preferences,
    Sync,
    Flush,
}

impl From<&storage::Command> for Operation {
    fn from(command: &storage::Command) -> Self {
        use storage::Command;
        match command {
            Command::Initialize => Operation::Open,
            Command::List { .. } => Operation::List,
            Command::Load { .. } => Operation::Load,
            Command::Create(_) => Operation::Create,
            Command::Save { .. } | Command::Edit { .. } => Operation::Save,
            Command::Mutate(_) => Operation::Change,
            Command::Preferences(_) => Operation::Preferences,
            Command::Remote { .. } | Command::Attach(_) => Operation::Sync,
            Command::Flush => Operation::Flush,
        }
    }
}

/// A change to apply to the text widget, in order. Each position refers to the
/// text after the previous edit.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum TextEdit {
    Insert { at: i32, text: String },
    Delete { at: i32, len: i32 },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct Applied {
    pub edits: Vec<TextEdit>,
    /// Where an undo or redo leaves the cursor.
    pub cursor: Option<i32>,
}

impl From<editor::Applied> for Applied {
    fn from(applied: editor::Applied) -> Self {
        Self {
            edits: applied
                .edits
                .into_iter()
                .map(|edit| match edit {
                    editor::TextEdit::Insert { at, text } => TextEdit::Insert {
                        at: at as i32,
                        text,
                    },
                    editor::TextEdit::Delete { at, len } => TextEdit::Delete {
                        at: at as i32,
                        len: len as i32,
                    },
                })
                .collect(),
            cursor: applied.cursor.map(|cursor| cursor as i32),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncStatus {
    pub enabled: bool,
    pub suspended: bool,
    /// Bound and reachable; false while off, suspended, or failing to start.
    pub running: bool,
    pub device_name: String,
    pub relay_url: Option<String>,
    pub relay_connected: bool,
    pub devices: Vec<DeviceStatus>,
    pub pairing: Pairing,
    pub problem: Option<String>,
}

impl From<sync::Status> for SyncStatus {
    fn from(status: sync::Status) -> Self {
        Self {
            running: status.running(),
            enabled: status.enabled,
            suspended: status.suspended,
            device_name: status.device_name,
            relay_url: status.relay_url,
            relay_connected: status.relay_connected,
            devices: status
                .devices
                .into_iter()
                .map(|device| DeviceStatus {
                    id: device.id,
                    name: device.name,
                    connected: device.connected,
                    last_synced: device.last_synced,
                })
                .collect(),
            pairing: status.pairing.into(),
            problem: status.problem,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DeviceStatus {
    pub id: String,
    pub name: String,
    pub connected: bool,
    /// Milliseconds since the Unix epoch.
    pub last_synced: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum Pairing {
    Idle,
    /// The code to show, as "123 456".
    Showing {
        code: String,
    },
    Searching,
    Paired {
        name: String,
    },
    Failed {
        message: String,
    },
}

impl From<sync::Pairing> for Pairing {
    fn from(pairing: sync::Pairing) -> Self {
        match pairing {
            sync::Pairing::Idle => Pairing::Idle,
            sync::Pairing::Showing(code) => Pairing::Showing { code },
            sync::Pairing::Searching => Pairing::Searching,
            sync::Pairing::Paired(name) => Pairing::Paired { name },
            sync::Pairing::Failed(message) => Pairing::Failed { message },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SyncControl {
    SetEnabled {
        enabled: bool,
    },
    SetDeviceName {
        name: String,
    },
    SetRelay {
        url: Option<String>,
    },
    StartPairing,
    JoinPairing {
        code: String,
    },
    CancelPairing,
    Forget {
        id: String,
    },
    /// Closes connections while the app is in the background, without
    /// turning sync off.
    Suspend {
        suspended: bool,
    },
    /// Send from a `ConnectivityManager` callback; iroh can't see network
    /// changes on Android.
    NetworkChanged,
}

impl From<SyncControl> for sync::Control {
    fn from(control: SyncControl) -> Self {
        match control {
            SyncControl::SetEnabled { enabled } => sync::Control::SetEnabled(enabled),
            SyncControl::SetDeviceName { name } => sync::Control::SetDeviceName(name),
            SyncControl::SetRelay { url } => sync::Control::SetRelay(url),
            SyncControl::StartPairing => sync::Control::StartPairing,
            SyncControl::JoinPairing { code } => sync::Control::JoinPairing(code),
            SyncControl::CancelPairing => sync::Control::CancelPairing,
            SyncControl::Forget { id } => sync::Control::Forget(id),
            SyncControl::Suspend { suspended } => sync::Control::Suspend(suspended),
            SyncControl::NetworkChanged => sync::Control::NetworkChanged,
        }
    }
}
