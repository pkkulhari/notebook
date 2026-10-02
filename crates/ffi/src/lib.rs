//! The Notebook core for Kotlin, through UniFFI.
//!
//! This layer holds no logic of its own. It wires the storage worker, the sync
//! thread and the editor's drafts together, converts types, and keeps these
//! threading rules:
//!
//! 1. Every call that reads or changes a draft's text comes from the Android
//!    main thread, in the same order as the `EditText` changes.
//! 2. Storage changes to an open note reach Kotlin as `NoteDelta` events.
//!    Kotlin imports them on the main thread, so the draft never gets ahead of
//!    the text widget.
//! 3. Bookkeeping that doesn't change text happens here as events arrive:
//!    saves acknowledged, notes created, moved or trashed, and failed
//!    commands kept for retry.
//! 4. Listener callbacks run on Rust threads. Kotlin posts them to the main
//!    thread before touching the UI or calling back into `Core`.
uniffi::setup_scaffolding!();

#[cfg(target_os = "android")]
mod android;
mod markdown;
mod types;

pub use markdown::*;
pub use types::*;

use notebook_core::{
    editor::{Draft, Drafts, Failures, OutOfSync},
    model::{DEFAULT_NOTEBOOK, Note, Preferences, Units},
    storage::{self, Command, Event},
    sync::{self, SyncHandle},
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex, MutexGuard, mpsc::Sender},
    time::Instant,
};

/// Receives storage events and sync statuses, on Rust threads.
#[uniffi::export(with_foreign)]
pub trait CoreListener: Send + Sync {
    fn on_event(&self, event: CoreEvent);
    fn on_sync_status(&self, status: SyncStatus);
}

#[derive(uniffi::Object)]
pub struct Core {
    commands: Sender<Command>,
    shared: Arc<Shared>,
    sync: SyncHandle,
}

struct Shared {
    drafts: Mutex<Drafts>,
    failed: Mutex<Failures>,
    default_notebook: Mutex<String>,
}

#[uniffi::export]
impl Core {
    /// Starts the storage worker, the sync thread and the event thread. They
    /// stop when the `Core` is dropped. Call once per process.
    #[uniffi::constructor]
    pub fn start(config: CoreConfig, listener: Arc<dyn CoreListener>) -> Arc<Self> {
        #[cfg(all(target_os = "android", feature = "logcat"))]
        android::init_logging();
        let database = PathBuf::from(config.database_path);
        let (commands, events) = storage::spawn_worker(database.clone());
        let statuses = listener.clone();
        let sync = sync::spawn(
            config.sync_config_path.into(),
            database,
            config.device_name,
            commands.clone(),
            move |status| statuses.on_sync_status(status.into()),
            sync::Options::default(),
        );
        let shared = Arc::new(Shared {
            drafts: Mutex::new(Drafts::new(Units::Utf16)),
            failed: Mutex::default(),
            default_notebook: Mutex::new(DEFAULT_NOTEBOOK.into()),
        });
        let state = shared.clone();
        std::thread::Builder::new()
            .name("notebook-events".into())
            .spawn(move || {
                // Ends when the storage worker does, after the last `Core` is gone.
                for event in events {
                    listener.on_event(state.receive(event));
                }
            })
            .expect("could not start the event thread");
        Arc::new(Self {
            commands,
            shared,
            sync,
        })
    }

    /// Opens the database. `Ready` follows.
    pub fn initialize(&self) {
        self.send(Command::Initialize);
    }

    /// Lists notes matching `query`. `Listed` follows with the same
    /// `generation`, so a stale answer can be told apart.
    pub fn list(&self, filter: Filter, query: String, generation: i64) {
        self.send(Command::List {
            filter: filter.into(),
            query,
            generation: generation as u64,
        });
    }

    /// `Loaded` follows with the same `generation`.
    pub fn load(&self, id: String, generation: i64) {
        self.send(Command::Load {
            id,
            generation: generation as u64,
        });
    }

    /// Starts a note in the Default notebook with an empty draft, and asks
    /// storage to create it. `Created` follows.
    pub fn create_note(&self) -> NoteInfo {
        let mut note = Note::blank();
        note.notebook_id = self.shared.default_notebook.lock().unwrap().clone();
        self.drafts().open(note.clone(), &[], false);
        let info = NoteInfo::from(&note);
        self.send(Command::Create(note));
        info
    }

    pub fn mutate(&self, mutation: Mutation) {
        self.send(Command::Mutate(mutation.into()));
    }

    /// `cursor` is in UTF-16 units; Android's preferences are its own.
    pub fn save_preferences(&self, selected_note: Option<String>, cursor: i32) {
        self.send(Command::Preferences(Preferences {
            selected_note,
            cursor,
            ..Preferences::default()
        }));
    }

    /// Sends every unsaved edit, then asks storage to report `Flushed` once
    /// everything sent so far is done.
    pub fn flush(&self) {
        self.send_drafts();
        self.send(Command::Flush);
    }

    /// Sends the failed commands again, then flushes. A failed save resends
    /// everything since the last confirmed one, including edits made after it.
    pub fn retry(&self) {
        let commands = self.shared.failed.lock().unwrap().retry(&mut self.drafts());
        for command in commands {
            self.send(command);
        }
        self.flush();
    }

    pub fn has_failures(&self) -> bool {
        !self.shared.failed.lock().unwrap().is_empty()
    }

    // Editor: main thread only.

    /// Opens a note's draft from its snapshot, replacing any open one, and
    /// returns its text.
    pub fn open_draft(&self, note: NoteInfo, snapshot: Vec<u8>) -> String {
        self.drafts().open(note.into(), &snapshot, true).text()
    }

    /// An open draft's text, or `None` if the note has no draft.
    pub fn draft_text(&self, id: String) -> Option<String> {
        self.drafts().get(&id).map(Draft::text)
    }

    /// An open draft's notebook and trash state, as of the latest events.
    pub fn draft_note(&self, id: String) -> Option<NoteInfo> {
        self.drafts().get(&id).map(|d| d.note().into())
    }

    /// Records that the note is showing, so `trim_drafts` closes it last.
    pub fn show_draft(&self, id: String) {
        if let Some(draft) = self.drafts().get_mut(&id) {
            draft.touch();
        }
    }

    /// Typing that the text widget already shows: replaces `len` units at
    /// `at`, recording only what actually differs, which suits a
    /// `TextWatcher`'s `onTextChanged`.
    pub fn replace(&self, id: String, at: i32, len: i32, text: String) -> Result<(), CoreError> {
        let mut drafts = self.drafts();
        let draft = drafts.get_mut(&id).ok_or_else(|| CoreError::OutOfSync {
            reason: format!("No draft is open for note {id}"),
        })?;
        Ok(draft.replace(unit(at)?, unit(len)?, &text)?)
    }

    /// Whether the draft's line that ends at `at` is in a code block, by its
    /// current text. Enter there shouldn't continue a list.
    pub fn line_in_code_block(&self, id: String, at: i32) -> bool {
        let before = self
            .drafts()
            .get(&id)
            .and_then(|d| d.text_before(unit(at).ok()?).ok());
        before.is_some_and(|text| notebook_core::markdown::ends_in_code_block(&text))
    }

    /// `None` if there's nothing to undo.
    pub fn undo(&self, id: String) -> Option<Applied> {
        self.drafts().get_mut(&id)?.undo().map(Into::into)
    }

    pub fn redo(&self, id: String) -> Option<Applied> {
        self.drafts().get_mut(&id)?.redo().map(Into::into)
    }

    /// Imports a `NoteDelta` into the note's draft, if it's open, and returns
    /// the edits that bring the text widget up to date.
    pub fn import_delta(&self, id: String, delta: Vec<u8>) -> Applied {
        self.drafts()
            .get_mut(&id)
            .map(|d| d.import(&delta).into())
            .unwrap_or_default()
    }

    /// Sends the saves that are due. Returns the milliseconds until the next
    /// one, or -1 when nothing is waiting, so the caller can sleep till then.
    pub fn tick(&self) -> i64 {
        let now = Instant::now();
        let (commands, next) = {
            let mut drafts = self.drafts();
            (drafts.due(now), drafts.next_due(now))
        };
        for command in commands {
            self.send(command);
        }
        next.map_or(-1, |wait| wait.as_micros().div_ceil(1000) as i64)
    }

    /// Closes the least recently shown drafts beyond the cache limit, never
    /// `active` or one with unsaved edits.
    pub fn trim_drafts(&self, active: Option<String>) {
        self.drafts().trim(active.as_deref());
    }

    pub fn sync(&self, control: SyncControl) {
        self.sync.send(control.into());
    }
}

impl Core {
    fn send(&self, command: Command) {
        // The worker only stops after the last sender is gone.
        let _ = self.commands.send(command);
    }

    fn send_drafts(&self) {
        let commands = self.drafts().flush();
        for command in commands {
            self.send(command);
        }
    }

    fn drafts(&self) -> MutexGuard<'_, Drafts> {
        self.shared.drafts.lock().unwrap()
    }
}

fn unit(value: i32) -> Result<usize, OutOfSync> {
    usize::try_from(value).map_err(|_| OutOfSync)
}

impl Shared {
    /// Does the bookkeeping an event calls for (rule 3), and converts it.
    fn receive(&self, event: Event) -> CoreEvent {
        match event {
            Event::Ready {
                default_notebook_id,
                notebooks,
                note,
                snapshot,
                preferences,
            } => {
                default_notebook_id.clone_into(&mut self.default_notebook.lock().unwrap());
                CoreEvent::Ready {
                    default_notebook_id,
                    notebooks: types::notebooks(notebooks),
                    note: (&note).into(),
                    snapshot,
                    selected_note: preferences.selected_note,
                    cursor: preferences.cursor,
                }
            }
            Event::Listed {
                notes,
                counts,
                generation,
            } => CoreEvent::Listed {
                notes: notes.into_iter().map(Into::into).collect(),
                counts: counts
                    .into_iter()
                    .map(|(filter, count)| NoteCount {
                        filter: filter.into(),
                        count: count as i64,
                    })
                    .collect(),
                generation: generation as i64,
            },
            Event::Loaded {
                note,
                snapshot,
                generation,
            } => CoreEvent::Loaded {
                note: note.as_ref().map(Into::into),
                snapshot,
                generation: generation as i64,
            },
            Event::Created(id) => {
                self.drafts.lock().unwrap().created(&id);
                CoreEvent::Created { id }
            }
            Event::Saved { id, sequence } => {
                self.drafts.lock().unwrap().saved(&id, sequence);
                self.failed.lock().unwrap().saved(&id, sequence);
                CoreEvent::Saved { id }
            }
            Event::Mutated {
                mutation,
                notebooks,
            } => {
                let default_notebook = self.default_notebook.lock().unwrap().clone();
                self.drafts
                    .lock()
                    .unwrap()
                    .mutated(&mutation, &default_notebook);
                CoreEvent::Mutated {
                    mutation: mutation.into(),
                    notebooks: types::notebooks(notebooks),
                }
            }
            Event::NoteDelta { id, delta } => CoreEvent::NoteDelta { id, delta },
            Event::Remote { notes, notebooks } => CoreEvent::Remote {
                trashed: self.drafts.lock().unwrap().remote(&notes),
                notebooks: notebooks.map(types::notebooks),
            },
            Event::Flushed => CoreEvent::Flushed,
            Event::Error { command, message } => CoreEvent::Error {
                operation: (&command).into(),
                message,
                retryable: self.failed.lock().unwrap().record(command),
            },
        }
    }
}
