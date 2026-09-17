use crate::markdown::summary;
use crate::model::*;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub struct Repository {
    connection: Connection,
    default_notebook_id: String,
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(3))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 1 {
            return Err("This database was created by a newer version of Notebook".into());
        }
        connection.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
        )?;
        if version == 0 {
            let tx = connection.transaction()?;
            tx.execute_batch(
                "CREATE TABLE notebooks (
                    id TEXT PRIMARY KEY, name TEXT NOT NULL,
                    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                    revision INTEGER NOT NULL, deleted_at INTEGER);
                 CREATE UNIQUE INDEX notebook_names ON notebooks(name COLLATE NOCASE) WHERE deleted_at IS NULL;
                 CREATE TABLE notes (
                    id TEXT PRIMARY KEY, notebook_id TEXT NOT NULL REFERENCES notebooks(id),
                    body TEXT NOT NULL, label TEXT NOT NULL, preview TEXT NOT NULL,
                    created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
                    revision INTEGER NOT NULL, deleted_at INTEGER);
                 CREATE INDEX notes_listing ON notes(notebook_id, deleted_at, updated_at DESC);
                 CREATE INDEX notes_recent ON notes(deleted_at, updated_at DESC);
                 CREATE VIRTUAL TABLE notes_fts USING fts5(body, content='notes', content_rowid='rowid', tokenize='unicode61');
                 CREATE TRIGGER notes_ai AFTER INSERT ON notes BEGIN
                   INSERT INTO notes_fts(rowid, body) VALUES (new.rowid, new.body); END;
                 CREATE TRIGGER notes_au AFTER UPDATE OF body ON notes BEGIN
                   INSERT INTO notes_fts(notes_fts, rowid, body) VALUES ('delete', old.rowid, old.body);
                   INSERT INTO notes_fts(rowid, body) VALUES (new.rowid, new.body); END;
                 CREATE TABLE changes (
                    sequence INTEGER PRIMARY KEY AUTOINCREMENT, entity TEXT NOT NULL,
                    entity_id TEXT NOT NULL, operation TEXT NOT NULL,
                    revision INTEGER NOT NULL, changed_at INTEGER NOT NULL);
                 CREATE TABLE preferences (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 PRAGMA user_version=1;",
            )?;
            let now = now_millis();
            tx.execute(
                "INSERT INTO notebooks VALUES (?1, 'Default', ?2, ?2, 1, NULL)",
                params![DEFAULT_NOTEBOOK, now],
            )?;
            journal(&tx, "notebook", DEFAULT_NOTEBOOK, "create", 1)?;
            tx.commit()?;
        }
        let default_notebook_id = connection
            .query_row(
                "SELECT id FROM notebooks WHERE id=?1 AND deleted_at IS NULL",
                [DEFAULT_NOTEBOOK],
                |r| r.get(0),
            )
            .optional()?
            .ok_or("The database is missing its Default notebook")?;
        Ok(Self {
            connection,
            default_notebook_id,
        })
    }

    pub fn default_notebook_id(&self) -> &str {
        &self.default_notebook_id
    }

    pub fn notebooks(&self) -> Result<Vec<Notebook>> {
        let mut stmt = self.connection.prepare("SELECT id, name FROM notebooks WHERE deleted_at IS NULL ORDER BY id != ?1, name COLLATE NOCASE")?;
        Ok(stmt
            .query_map([&self.default_notebook_id], |r| {
                Ok(Notebook {
                    id: r.get(0)?,
                    name: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn load(&self, id: &str) -> Result<Option<Note>> {
        Ok(self
            .connection
            .query_row(
                "SELECT id, notebook_id, body, deleted_at IS NOT NULL FROM notes WHERE id=?1",
                [id],
                |r| {
                    Ok(Note {
                        id: r.get(0)?,
                        notebook_id: r.get(1)?,
                        body: r.get(2)?,
                        deleted: r.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn list(&self, filter: &Filter, query: &str) -> Result<Vec<NoteSummary>> {
        let (deleted, notebook) = match filter {
            Filter::Notebook(id) => (false, Some(id.as_str())),
            Filter::All => (false, None),
            Filter::Trash => (true, None),
        };
        // Quote every term so punctuation and FTS operators are ordinary user input.
        let expression = query
            .split_whitespace()
            .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql = if expression.is_empty() {
            "SELECT id, label, preview, updated_at FROM notes WHERE (deleted_at IS NOT NULL)=?1 AND (?2 IS NULL OR notebook_id=?2) ORDER BY updated_at DESC, id"
        } else {
            "SELECT n.id, n.label, n.preview, n.updated_at FROM notes n JOIN notes_fts f ON n.rowid=f.rowid WHERE (n.deleted_at IS NOT NULL)=?1 AND (?2 IS NULL OR n.notebook_id=?2) AND notes_fts MATCH ?3 ORDER BY rank, n.updated_at DESC"
        };
        let mut stmt = self.connection.prepare(sql)?;
        let map = |r: &rusqlite::Row<'_>| {
            Ok(NoteSummary {
                id: r.get(0)?,
                label: r.get(1)?,
                preview: r.get(2)?,
                updated_at: r.get(3)?,
            })
        };
        let rows = if expression.is_empty() {
            stmt.query_map(params![deleted, notebook], map)?
        } else {
            stmt.query_map(params![deleted, notebook, expression], map)?
        };
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn note_counts(&self) -> Result<NoteCounts> {
        let mut counts = NoteCounts::new();
        let mut stmt = self.connection.prepare(
            "SELECT notebook_id, deleted_at IS NOT NULL, COUNT(*) FROM notes GROUP BY notebook_id, deleted_at IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, bool>(1)?,
                r.get::<_, i64>(2)? as u64,
            ))
        })?;
        for row in rows {
            let (notebook, deleted, count) = row?;
            if deleted {
                *counts.entry(Filter::Trash).or_default() += count;
            } else {
                counts.insert(Filter::Notebook(notebook), count);
                *counts.entry(Filter::All).or_default() += count;
            }
        }
        Ok(counts)
    }

    pub fn create_note(&mut self, note: &Note) -> Result<()> {
        let tx = self.connection.transaction()?;
        let now = now_millis();
        let (label, preview) = summary(&note.body);
        tx.execute(
            "INSERT INTO notes(id, notebook_id, body, label, preview, created_at, updated_at, revision, deleted_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, 1, NULL)",
            params![note.id, self.default_notebook_id, note.body, label, preview, now],
        )?;
        journal(&tx, "note", &note.id, "create", 1)?;
        tx.commit()?;
        Ok(())
    }

    pub fn save(&mut self, id: &str, body: &str) -> Result<()> {
        let tx = self.connection.transaction()?;
        let (label, preview) = summary(body);
        let changed = tx.execute("UPDATE notes SET body=?2, label=?3, preview=?4, updated_at=?5, revision=revision+1 WHERE id=?1 AND body != ?2", params![id, body, label, preview, now_millis()])?;
        if changed > 0 {
            journal_note(&tx, id, "update")?;
        } else if !tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id=?1)",
            [id],
            |r| r.get::<_, bool>(0),
        )? {
            return Err("Note is missing; your unsaved text is still in memory".into());
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mutate(&mut self, mutation: &Mutation) -> Result<()> {
        let tx = self.connection.transaction()?;
        let now = now_millis();
        match mutation {
            Mutation::CreateNotebook { id, name } => {
                validate_name(name)?;
                unique_name(&tx, id, name)?;
                tx.execute(
                    "INSERT INTO notebooks VALUES (?1, ?2, ?3, ?3, 1, NULL)",
                    params![id, name.trim(), now],
                )?;
                journal(&tx, "notebook", id, "create", 1)?;
            }
            Mutation::RenameNotebook { id, name } => {
                protect_default(id, &self.default_notebook_id)?;
                validate_name(name)?;
                unique_name(&tx, id, name)?;
                if tx.execute("UPDATE notebooks SET name=?2, updated_at=?3, revision=revision+1 WHERE id=?1 AND deleted_at IS NULL", params![id, name.trim(), now])? == 0 { return Err("Notebook no longer exists".into()); }
                let rev =
                    tx.query_row("SELECT revision FROM notebooks WHERE id=?1", [id], |r| {
                        r.get(0)
                    })?;
                journal(&tx, "notebook", id, "rename", rev)?;
            }
            Mutation::DeleteNotebook { id } => {
                protect_default(id, &self.default_notebook_id)?;
                let ids: Vec<String> = tx
                    .prepare("SELECT id FROM notes WHERE notebook_id=?1")?
                    .query_map([id], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                tx.execute("UPDATE notes SET notebook_id=?2, updated_at=?3, revision=revision+1 WHERE notebook_id=?1", params![id, self.default_notebook_id, now])?;
                for note in ids {
                    journal_note(&tx, &note, "move")?;
                }
                tx.execute("UPDATE notebooks SET deleted_at=?2, updated_at=?2, revision=revision+1 WHERE id=?1", params![id, now])?;
                let rev =
                    tx.query_row("SELECT revision FROM notebooks WHERE id=?1", [id], |r| {
                        r.get(0)
                    })?;
                journal(&tx, "notebook", id, "delete", rev)?;
            }
            Mutation::Move { id, notebook_id } => {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM notebooks WHERE id=?1 AND deleted_at IS NULL)",
                    [notebook_id],
                    |r| r.get(0),
                )?;
                if !exists {
                    return Err("Destination notebook no longer exists".into());
                }
                tx.execute("UPDATE notes SET notebook_id=?2, updated_at=?3, revision=revision+1 WHERE id=?1", params![id, notebook_id, now])?;
                journal_note(&tx, id, "move")?;
            }
            Mutation::Trash { id } | Mutation::Restore { id } => {
                let deleted = matches!(mutation, Mutation::Trash { .. }).then_some(now);
                tx.execute("UPDATE notes SET deleted_at=?2, updated_at=?3, revision=revision+1 WHERE id=?1", params![id, deleted, now])?;
                journal_note(
                    &tx,
                    id,
                    if deleted.is_some() {
                        "delete"
                    } else {
                        "restore"
                    },
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn preferences(&self) -> Result<Preferences> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM preferences WHERE key='window'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(value
            .and_then(|v| serde_json::from_str(&v).ok())
            .unwrap_or_default())
    }

    pub fn save_preferences(&self, prefs: &Preferences) -> Result<()> {
        self.connection.execute("INSERT INTO preferences VALUES ('window', ?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [serde_json::to_string(prefs)?])?;
        Ok(())
    }

    pub fn bootstrap(&mut self) -> Result<(Vec<Notebook>, Note, Preferences)> {
        let prefs = self.preferences()?;
        let last = prefs
            .selected_note
            .as_deref()
            .map(|id| self.load(id))
            .transpose()?
            .flatten()
            .filter(|n| !n.deleted);
        let note = match last {
            Some(note) => note,
            None => match self.list(&Filter::All, "")?.first() {
                Some(n) => self.load(&n.id)?.ok_or("Note disappeared")?,
                None => {
                    let mut note = Note::blank();
                    note.notebook_id = self.default_notebook_id.clone();
                    self.create_note(&note)?;
                    note
                }
            },
        };
        Ok((self.notebooks()?, note, prefs))
    }
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.chars().count() > 100 {
        Err("Use a notebook name between 1 and 100 characters".into())
    } else {
        Ok(())
    }
}

fn unique_name(tx: &Transaction<'_>, id: &str, name: &str) -> Result<()> {
    let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM notebooks WHERE name=?1 COLLATE NOCASE AND id != ?2 AND deleted_at IS NULL)", params![name.trim(), id], |r| r.get(0))?;
    if exists {
        Err("A notebook with that name already exists".into())
    } else {
        Ok(())
    }
}
fn protect_default(id: &str, default_notebook_id: &str) -> Result<()> {
    if id == default_notebook_id {
        Err("Default is the permanent home for new notes".into())
    } else {
        Ok(())
    }
}
fn journal(
    tx: &Transaction<'_>,
    entity: &str,
    id: &str,
    operation: &str,
    revision: i64,
) -> rusqlite::Result<()> {
    tx.execute("INSERT INTO changes(entity, entity_id, operation, revision, changed_at) VALUES (?1,?2,?3,?4,?5)", params![entity, id, operation, revision, now_millis()])?;
    Ok(())
}
fn journal_note(tx: &Transaction<'_>, id: &str, operation: &str) -> rusqlite::Result<()> {
    let rev = tx.query_row("SELECT revision FROM notes WHERE id=?1", [id], |r| r.get(0))?;
    journal(tx, "note", id, operation, rev)
}

pub fn data_path() -> PathBuf {
    let root = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share")
        });
    root.join("notebook/notebook.db")
}

#[derive(Clone, Debug)]
pub enum Mutation {
    CreateNotebook { id: String, name: String },
    RenameNotebook { id: String, name: String },
    DeleteNotebook { id: String },
    Move { id: String, notebook_id: String },
    Trash { id: String },
    Restore { id: String },
}

#[derive(Clone, Debug)]
pub enum Command {
    Initialize,
    List {
        filter: Filter,
        query: String,
        generation: u64,
    },
    Load {
        id: String,
        generation: u64,
    },
    Create(Note),
    Save {
        id: String,
        body: String,
        sequence: u64,
    },
    Mutate(Mutation),
    Preferences(Preferences),
    Flush,
}

pub enum Event {
    Ready {
        default_notebook_id: String,
        notebooks: Vec<Notebook>,
        note: Note,
        preferences: Preferences,
    },
    Listed {
        notes: Vec<NoteSummary>,
        counts: NoteCounts,
        generation: u64,
    },
    Loaded {
        note: Option<Note>,
        generation: u64,
    },
    Created(String),
    Saved {
        id: String,
        sequence: u64,
    },
    Mutated {
        mutation: Mutation,
        notebooks: Vec<Notebook>,
    },
    Flushed,
    Error {
        command: Command,
        message: String,
    },
}

pub fn spawn_worker(path: PathBuf) -> (Sender<Command>, Receiver<Event>) {
    let (commands, incoming) = mpsc::channel();
    let (outgoing, events) = mpsc::channel();
    std::thread::Builder::new()
        .name("notebook-storage".into())
        .spawn(move || {
            let mut repository = None;
            for command in incoming {
                let outcome = (|| -> Result<Option<Event>> {
                    if repository.is_none() {
                        repository = Some(Repository::open(&path)?);
                    }
                    let repo = repository.as_mut().expect("initialized above");
                    Ok(match &command {
                        Command::Initialize => {
                            let (notebooks, note, preferences) = repo.bootstrap()?;
                            Some(Event::Ready {
                                default_notebook_id: repo.default_notebook_id().into(),
                                notebooks,
                                note,
                                preferences,
                            })
                        }
                        Command::List {
                            filter,
                            query,
                            generation,
                        } => Some(Event::Listed {
                            notes: repo.list(filter, query)?,
                            counts: repo.note_counts()?,
                            generation: *generation,
                        }),
                        Command::Load { id, generation } => Some(Event::Loaded {
                            note: repo.load(id)?,
                            generation: *generation,
                        }),
                        Command::Create(note) => {
                            repo.create_note(note)?;
                            Some(Event::Created(note.id.clone()))
                        }
                        Command::Save { id, body, sequence } => {
                            repo.save(id, body)?;
                            Some(Event::Saved {
                                id: id.clone(),
                                sequence: *sequence,
                            })
                        }
                        Command::Mutate(mutation) => {
                            repo.mutate(mutation)?;
                            Some(Event::Mutated {
                                mutation: mutation.clone(),
                                notebooks: repo.notebooks()?,
                            })
                        }
                        Command::Preferences(prefs) => {
                            repo.save_preferences(prefs)?;
                            None
                        }
                        Command::Flush => Some(Event::Flushed),
                    })
                })();
                let event = match outcome {
                    Ok(event) => event,
                    Err(e) => Some(Event::Error {
                        command,
                        message: e.to_string(),
                    }),
                };
                if let Some(event) = event
                    && outgoing.send(event).is_err()
                {
                    break;
                }
            }
        })
        .expect("could not start storage worker");
    (commands, events)
}
