use crate::crdt::{self, Kind};
use crate::markdown::summary;
use crate::model::*;
use loro::{ExportMode, LoroDoc, VersionVector};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};

pub type Result<T> = crdt::Result<T>;

const SCHEMA_VERSION: i64 = 2;
/// Saved updates are folded into a document's snapshot after this many.
const COMPACT_AFTER: i64 = 64;
const CACHED_DOCS: usize = 64;
/// Notes this long diff by line; character diffs get slow on huge texts.
const LINE_DIFF_CHARS: usize = 50_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Local,
    /// The endpoint ID of the paired device the change came from.
    Remote(String),
}

#[derive(Clone, Debug)]
pub struct DocChange {
    pub id: String,
    pub kind: Kind,
    pub delta: Vec<u8>,
    pub origin: Origin,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct RemoteDoc {
    pub id: String,
    pub kind: Kind,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DocVersion {
    pub id: String,
    pub version: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteState {
    pub id: String,
    pub notebook_id: String,
    pub deleted: bool,
}

struct DocCache {
    docs: HashMap<String, (Kind, LoroDoc, u64)>,
    clock: u64,
    peer: u64,
}

impl DocCache {
    /// Returns a shared handle to the document, loading it on first use.
    fn get(&mut self, db: &Connection, id: &str) -> Result<Option<(Kind, LoroDoc)>> {
        self.clock += 1;
        if let Some((kind, doc, used)) = self.docs.get_mut(id) {
            *used = self.clock;
            return Ok(Some((*kind, doc.clone())));
        }
        let Some((kind, doc)) = read_doc(db, id)? else {
            return Ok(None);
        };
        doc.set_peer_id(self.peer)?;
        self.insert(id, kind, doc.clone());
        Ok(Some((kind, doc)))
    }

    fn insert(&mut self, id: &str, kind: Kind, doc: LoroDoc) {
        self.clock += 1;
        self.docs.insert(id.into(), (kind, doc, self.clock));
        if self.docs.len() > CACHED_DOCS
            && let Some(oldest) = self
                .docs
                .iter()
                .min_by_key(|(_, (_, _, used))| *used)
                .map(|(id, _)| id.clone())
        {
            self.docs.remove(&oldest);
        }
    }

    /// Drops a document whose in-memory changes were not saved, so the next
    /// use reloads what is actually on disk.
    fn forget(&mut self, id: &str) {
        self.docs.remove(id);
    }
}

pub struct Repository {
    connection: Connection,
    default_notebook_id: String,
    cache: DocCache,
    changes: Vec<DocChange>,
}

impl Repository {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(3))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > SCHEMA_VERSION {
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
        if version < 2 {
            migrate_to_documents(&mut connection)?;
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
            cache: DocCache {
                docs: HashMap::new(),
                clock: 0,
                peer: crdt::random_peer(),
            },
            changes: Vec::new(),
        })
    }

    pub fn default_notebook_id(&self) -> &str {
        &self.default_notebook_id
    }

    /// Live notebooks, Default first. Devices that each created a notebook with
    /// the same name while apart keep both; later ones read "Name (2)".
    pub fn notebooks(&self) -> Result<Vec<Notebook>> {
        let mut stmt = self.connection.prepare("SELECT id, name FROM notebooks WHERE deleted_at IS NULL ORDER BY id != ?1, name COLLATE NOCASE, created_at, id")?;
        let rows: Vec<Notebook> = stmt
            .query_map([&self.default_notebook_id], |r| {
                Ok(Notebook {
                    id: r.get(0)?,
                    name: r.get(1)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut seen = HashMap::<String, usize>::new();
        Ok(rows
            .into_iter()
            .map(|mut book| {
                let count = seen.entry(book.name.to_ascii_lowercase()).or_default();
                *count += 1;
                if *count > 1 {
                    book.name = format!("{} ({count})", book.name);
                }
                book
            })
            .collect())
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

    pub fn snapshot(&mut self, id: &str) -> Result<Option<Vec<u8>>> {
        Ok(match self.cache.get(&self.connection, id)? {
            Some((_, doc)) => Some(doc.export(ExportMode::Snapshot)?),
            None => None,
        })
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
        let now = now_millis();
        let doc = crdt::new_note(
            self.cache.peer,
            &note.body,
            &self.default_notebook_id,
            now,
            now,
            None,
        )?;
        self.create_doc(&note.id, Kind::Note, doc)
    }

    fn create_doc(&mut self, id: &str, kind: Kind, doc: LoroDoc) -> Result<()> {
        let delta = doc.export(ExportMode::all_updates())?;
        let tx = self.connection.transaction()?;
        persist(
            &tx,
            id,
            kind,
            &doc,
            None,
            "create",
            &self.default_notebook_id,
        )?;
        tx.commit()?;
        self.cache.insert(id, kind, doc);
        self.changes.push(DocChange {
            id: id.into(),
            kind,
            delta,
            origin: Origin::Local,
        });
        Ok(())
    }

    /// Replaces a note's text, recording the difference as edits.
    pub fn save(&mut self, id: &str, body: &str) -> Result<()> {
        self.change_doc(id, Kind::Note, "update", |doc| {
            let text = crdt::text(doc);
            if text.to_string() == body {
                return Ok(false);
            }
            let options = loro::UpdateOptions::default();
            if body.chars().count() > LINE_DIFF_CHARS {
                text.update_by_line(body, options)
            } else {
                text.update(body, options)
            }
            .map_err(|_| "Could not compare the note's text")?;
            Ok(true)
        })
    }

    /// Applies edits made in the editor's copy of the note.
    pub fn edit(&mut self, id: &str, updates: &[Vec<u8>]) -> Result<()> {
        self.change_doc(id, Kind::Note, "update", |doc| {
            let before = doc.oplog_vv();
            let status = doc.import_batch(updates)?;
            if status.pending.is_some() {
                return Err("Earlier edits to this note have not been saved yet".into());
            }
            Ok(doc.oplog_vv() != before)
        })
    }

    /// `apply` returns whether it changed anything.
    fn change_doc(
        &mut self,
        id: &str,
        kind: Kind,
        operation: &str,
        apply: impl FnOnce(&LoroDoc) -> Result<bool>,
    ) -> Result<()> {
        let doc = match self.cache.get(&self.connection, id)? {
            Some((found, doc)) if found == kind => doc,
            _ => {
                return Err(match kind {
                    Kind::Note => "Note is missing; your unsaved text is still in memory",
                    Kind::Notebook => "Notebook no longer exists",
                }
                .into());
            }
        };
        let result = (|| -> Result<Option<DocChange>> {
            let before = doc.oplog_vv();
            if !apply(&doc)? {
                return Ok(None);
            }
            crdt::meta(&doc).insert("updated_at", now_millis())?;
            doc.commit();
            let delta = doc.export(ExportMode::updates(&before))?;
            let tx = self.connection.transaction()?;
            persist(
                &tx,
                id,
                kind,
                &doc,
                Some(&delta),
                operation,
                &self.default_notebook_id,
            )?;
            tx.commit()?;
            Ok(Some(DocChange {
                id: id.into(),
                kind,
                delta,
                origin: Origin::Local,
            }))
        })();
        let change = result.inspect_err(|_| self.cache.forget(id))?;
        self.changes.extend(change);
        Ok(())
    }

    pub fn mutate(&mut self, mutation: &Mutation) -> Result<()> {
        match mutation {
            Mutation::CreateNotebook { id, name } => {
                validate_name(name)?;
                unique_name(&self.connection, id, name)?;
                let now = now_millis();
                let doc = crdt::new_notebook(self.cache.peer, name.trim(), now, now, None)?;
                self.create_doc(id, Kind::Notebook, doc)
            }
            Mutation::RenameNotebook { id, name } => {
                protect_default(id, &self.default_notebook_id)?;
                validate_name(name)?;
                unique_name(&self.connection, id, name)?;
                self.change_doc(id, Kind::Notebook, "rename", |doc| {
                    if crdt::read_notebook(doc).deleted_at.is_some() {
                        return Err("Notebook no longer exists".into());
                    }
                    crdt::meta(doc).insert("name", name.trim())?;
                    Ok(true)
                })
            }
            // Notes keep pointing at a deleted notebook and are shown in Default.
            // Rewriting them would race with a move made on another device.
            Mutation::DeleteNotebook { id } => {
                protect_default(id, &self.default_notebook_id)?;
                self.change_doc(id, Kind::Notebook, "delete", |doc| {
                    crdt::set_deleted(&crdt::meta(doc), Some(now_millis()))?;
                    Ok(true)
                })
            }
            Mutation::Move { id, notebook_id } => {
                if !notebook_live(&self.connection, notebook_id)? {
                    return Err("Destination notebook no longer exists".into());
                }
                self.change_doc(id, Kind::Note, "move", |doc| {
                    crdt::meta(doc).insert("notebook", notebook_id.as_str())?;
                    Ok(true)
                })
            }
            Mutation::Trash { id } | Mutation::Restore { id } => {
                let deleted = matches!(mutation, Mutation::Trash { .. }).then(now_millis);
                let operation = if deleted.is_some() {
                    "delete"
                } else {
                    "restore"
                };
                self.change_doc(id, Kind::Note, operation, |doc| {
                    crdt::set_deleted(&crdt::meta(doc), deleted)?;
                    Ok(true)
                })
            }
        }
    }

    /// Returns the notes whose state changed, and whether any notebook did.
    pub fn apply_remote(
        &mut self,
        docs: &[RemoteDoc],
        from: &str,
    ) -> Result<(Vec<NoteState>, bool)> {
        let mut touched = Vec::new();
        let result = (|| -> Result<(Vec<DocChange>, Vec<NoteState>, bool)> {
            let tx = self.connection.transaction()?;
            let mut changes = Vec::new();
            let mut notes = Vec::new();
            let mut notebooks_changed = false;
            for remote in docs {
                validate_remote(remote, &self.default_notebook_id)?;
                touched.push(remote.id.as_str());
                let (known, doc) = match self.cache.get(&tx, &remote.id)? {
                    Some((kind, doc)) if kind == remote.kind => (true, doc),
                    Some(_) => return Err("A device sent a document of the wrong kind".into()),
                    None => {
                        let doc = LoroDoc::new();
                        doc.set_peer_id(self.cache.peer)?;
                        (false, doc)
                    }
                };
                let before = doc.oplog_vv();
                // Changes whose history hasn't arrived stay pending in memory until
                // it does; reconciling asks for them again if the app restarts.
                doc.import(&remote.bytes)?;
                if doc.oplog_vv() == before {
                    continue;
                }
                let delta = doc.export(ExportMode::updates(&before))?;
                let state = persist(
                    &tx,
                    &remote.id,
                    remote.kind,
                    &doc,
                    known.then_some(&delta),
                    "sync",
                    &self.default_notebook_id,
                )?;
                if !known {
                    // Cached only once saved, so the cache never holds a
                    // document the database lacks.
                    self.cache.insert(&remote.id, remote.kind, doc);
                }
                match state {
                    Some(state) => notes.push(state),
                    None => notebooks_changed = true,
                }
                changes.push(DocChange {
                    id: remote.id.clone(),
                    kind: remote.kind,
                    delta,
                    origin: Origin::Remote(from.into()),
                });
            }
            tx.commit()?;
            Ok((changes, notes, notebooks_changed))
        })();
        let (changes, notes, notebooks_changed) = result.inspect_err(|_| {
            for id in &touched {
                self.cache.forget(id);
            }
        })?;
        self.changes.extend(changes);
        Ok((notes, notebooks_changed))
    }

    pub fn take_changes(&mut self) -> Vec<DocChange> {
        std::mem::take(&mut self.changes)
    }

    pub fn digest(&self) -> Result<Vec<[u8; 32]>> {
        digest(&self.connection)
    }

    pub fn versions(&self, buckets: &[u8]) -> Result<Vec<DocVersion>> {
        versions(&self.connection, buckets)
    }

    pub fn export_since(&self, id: &str, theirs: Option<&[u8]>) -> Result<Option<RemoteDoc>> {
        export_since(&self.connection, id, theirs)
    }

    pub fn missing(&self, buckets: &[u8], theirs: &[DocVersion]) -> Result<Vec<RemoteDoc>> {
        missing(&self.connection, buckets, theirs)
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

fn migrate_to_documents(connection: &mut Connection) -> Result<()> {
    let tx = connection.transaction()?;
    tx.execute_batch(
        "ALTER TABLE notes ADD COLUMN home_id TEXT;
         UPDATE notes SET home_id=notebook_id;
         DROP INDEX notebook_names;
         CREATE INDEX notebook_names ON notebooks(name COLLATE NOCASE) WHERE deleted_at IS NULL;
         CREATE INDEX notes_home ON notes(home_id);
         CREATE TABLE docs (
            id TEXT PRIMARY KEY, kind TEXT NOT NULL,
            snapshot BLOB NOT NULL, version BLOB NOT NULL, updates INTEGER NOT NULL);
         CREATE TABLE doc_updates (
            sequence INTEGER PRIMARY KEY AUTOINCREMENT, doc_id TEXT NOT NULL, bytes BLOB NOT NULL);
         CREATE INDEX doc_updates_by_doc ON doc_updates(doc_id, sequence);",
    )?;
    let books: Vec<(String, String, i64, i64, Option<i64>)> = tx
        .prepare(
            "SELECT id, name, created_at, updated_at, deleted_at FROM notebooks WHERE id != ?1",
        )?
        .query_map([DEFAULT_NOTEBOOK], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    for (id, name, created, updated, deleted) in books {
        let peer = crdt::genesis_peer(&[
            b"notebook",
            id.as_bytes(),
            name.as_bytes(),
            &created.to_le_bytes(),
            &updated.to_le_bytes(),
            &deleted.unwrap_or(-1).to_le_bytes(),
        ]);
        let doc = crdt::new_notebook(peer, &name, created, updated, deleted)?;
        store_new(&tx, &id, Kind::Notebook, &doc)?;
    }
    let mut stmt =
        tx.prepare("SELECT id, notebook_id, body, created_at, updated_at, deleted_at FROM notes")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let notebook: String = row.get(1)?;
        let body: String = row.get(2)?;
        let (created, updated): (i64, i64) = (row.get(3)?, row.get(4)?);
        let deleted: Option<i64> = row.get(5)?;
        let peer = crdt::genesis_peer(&[
            b"note",
            id.as_bytes(),
            notebook.as_bytes(),
            body.as_bytes(),
            &created.to_le_bytes(),
            &updated.to_le_bytes(),
            &deleted.unwrap_or(-1).to_le_bytes(),
        ]);
        let doc = crdt::new_note(peer, &body, &notebook, created, updated, deleted)?;
        store_new(&tx, &id, Kind::Note, &doc)?;
    }
    drop(rows);
    drop(stmt);
    tx.execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION};"))?;
    tx.commit()?;
    Ok(())
}

fn validate_remote(remote: &RemoteDoc, default_notebook_id: &str) -> Result<()> {
    let valid = !remote.id.is_empty()
        && remote.id.len() <= 64
        && remote.id != default_notebook_id
        && remote
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err("A device sent a document with an invalid ID".into())
    }
}

fn read_doc(db: &Connection, id: &str) -> Result<Option<(Kind, LoroDoc)>> {
    let row: Option<(String, Vec<u8>)> = db
        .query_row("SELECT kind, snapshot FROM docs WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    let Some((kind, snapshot)) = row else {
        return Ok(None);
    };
    let updates: Vec<Vec<u8>> = db
        .prepare("SELECT bytes FROM doc_updates WHERE doc_id=?1 ORDER BY sequence")?
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let kind = Kind::parse(&kind).ok_or("Unknown document kind")?;
    Ok(Some((kind, crdt::load(&snapshot, &updates)?)))
}

fn store_new(tx: &Transaction<'_>, id: &str, kind: Kind, doc: &LoroDoc) -> Result<()> {
    tx.execute(
        "INSERT INTO docs(id, kind, snapshot, version, updates) VALUES (?1, ?2, ?3, ?4, 0)",
        params![
            id,
            kind.as_str(),
            doc.export(ExportMode::Snapshot)?,
            crdt::encode_version(&doc.oplog_vv())
        ],
    )?;
    Ok(())
}

/// Appends a document's new changes, folding them into a fresh snapshot
/// once enough have accumulated.
fn store_update(tx: &Transaction<'_>, id: &str, doc: &LoroDoc, delta: &[u8]) -> Result<()> {
    let version = crdt::encode_version(&doc.oplog_vv());
    let pending: i64 = tx.query_row("SELECT updates FROM docs WHERE id=?1", [id], |r| r.get(0))?;
    if pending + 1 >= COMPACT_AFTER {
        tx.execute(
            "UPDATE docs SET snapshot=?2, version=?3, updates=0 WHERE id=?1",
            params![id, doc.export(ExportMode::Snapshot)?, version],
        )?;
        tx.execute("DELETE FROM doc_updates WHERE doc_id=?1", [id])?;
    } else {
        tx.execute(
            "INSERT INTO doc_updates(doc_id, bytes) VALUES (?1, ?2)",
            params![id, delta],
        )?;
        tx.execute(
            "UPDATE docs SET version=?2, updates=updates+1 WHERE id=?1",
            params![id, version],
        )?;
    }
    Ok(())
}

/// `delta` is `None` for a new document. Returns a note's new state.
fn persist(
    tx: &Transaction<'_>,
    id: &str,
    kind: Kind,
    doc: &LoroDoc,
    delta: Option<&[u8]>,
    operation: &str,
    default_notebook_id: &str,
) -> Result<Option<NoteState>> {
    match delta {
        Some(delta) => store_update(tx, id, doc, delta)?,
        None => store_new(tx, id, kind, doc)?,
    }
    Ok(match kind {
        Kind::Note => {
            let state = project_note(tx, id, doc, default_notebook_id)?;
            journal_note(tx, id, operation)?;
            Some(state)
        }
        Kind::Notebook => {
            let rev = project_notebook(tx, id, doc, default_notebook_id)?;
            journal(tx, "notebook", id, operation, rev)?;
            None
        }
    })
}

fn notebook_live(db: &Connection, id: &str) -> Result<bool> {
    Ok(db.query_row(
        "SELECT EXISTS(SELECT 1 FROM notebooks WHERE id=?1 AND deleted_at IS NULL)",
        [id],
        |r| r.get(0),
    )?)
}

/// Where a note is listed: its notebook, or Default once that is deleted or
/// before it has arrived from another device.
fn effective_notebook(db: &Connection, home: &str, default_notebook_id: &str) -> Result<String> {
    Ok(if notebook_live(db, home)? {
        home
    } else {
        default_notebook_id
    }
    .into())
}

/// Writes a note document's current state into the tables the app queries.
fn project_note(
    tx: &Transaction<'_>,
    id: &str,
    doc: &LoroDoc,
    default_notebook_id: &str,
) -> Result<NoteState> {
    let meta = crdt::read_note(doc, default_notebook_id);
    let body = crdt::text(doc).to_string();
    let notebook_id = effective_notebook(tx, &meta.notebook, default_notebook_id)?;
    let existing: Option<String> = tx
        .query_row("SELECT body FROM notes WHERE id=?1", [id], |r| r.get(0))
        .optional()?;
    match existing {
        None => {
            let (label, preview) = summary(&body);
            tx.execute(
                "INSERT INTO notes(id, notebook_id, body, label, preview, created_at, updated_at, revision, deleted_at, home_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9)",
                params![id, notebook_id, body, label, preview, meta.created_at, meta.updated_at, meta.deleted_at, meta.notebook],
            )?;
        }
        Some(old) => {
            tx.execute(
                "UPDATE notes SET notebook_id=?2, home_id=?3, updated_at=?4, deleted_at=?5, revision=revision+1 WHERE id=?1",
                params![id, notebook_id, meta.notebook, meta.updated_at, meta.deleted_at],
            )?;
            // Only touch the body when it changed, so search isn't reindexed.
            if old != body {
                let (label, preview) = summary(&body);
                tx.execute(
                    "UPDATE notes SET body=?2, label=?3, preview=?4 WHERE id=?1",
                    params![id, body, label, preview],
                )?;
            }
        }
    }
    Ok(NoteState {
        id: id.into(),
        notebook_id,
        deleted: meta.deleted_at.is_some(),
    })
}

/// Also relists the notebook's notes. Returns its new revision.
fn project_notebook(
    tx: &Transaction<'_>,
    id: &str,
    doc: &LoroDoc,
    default_notebook_id: &str,
) -> Result<i64> {
    let meta = crdt::read_notebook(doc);
    let revision = tx.query_row(
        "INSERT INTO notebooks(id, name, created_at, updated_at, revision, deleted_at) VALUES (?1, ?2, ?3, ?4, 1, ?5)
         ON CONFLICT(id) DO UPDATE SET name=excluded.name, updated_at=excluded.updated_at, deleted_at=excluded.deleted_at, revision=revision+1
         RETURNING revision",
        params![id, meta.name, meta.created_at, meta.updated_at, meta.deleted_at],
        |r| r.get(0),
    )?;
    let notebook_id = effective_notebook(tx, id, default_notebook_id)?;
    let moved: Vec<String> = tx
        .prepare("UPDATE notes SET notebook_id=?2, revision=revision+1 WHERE home_id=?1 AND notebook_id != ?2 RETURNING id")?
        .query_map(params![id, notebook_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for note in moved {
        journal_note(tx, &note, "move")?;
    }
    Ok(revision)
}

/// XOR of every document's (ID, version) digest in each of 256 buckets. Two
/// devices whose buckets match hold the same changes for those documents.
pub fn digest(db: &Connection) -> Result<Vec<[u8; 32]>> {
    let mut buckets = vec![[0u8; 32]; 256];
    let mut stmt = db.prepare("SELECT id, version FROM docs")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let version: Vec<u8> = row.get(1)?;
        let entry = crdt::entry_digest(&id, &version);
        let bucket = &mut buckets[crdt::bucket(&id) as usize];
        for (byte, other) in bucket.iter_mut().zip(entry) {
            *byte ^= other;
        }
    }
    Ok(buckets)
}

pub fn versions(db: &Connection, buckets: &[u8]) -> Result<Vec<DocVersion>> {
    let wanted: std::collections::HashSet<u8> = buckets.iter().copied().collect();
    let mut stmt = db.prepare("SELECT id, kind, version FROM docs")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (id, kind, version) = row?;
        if Kind::parse(&kind).is_some() && wanted.contains(&crdt::bucket(&id)) {
            found.push(DocVersion { id, version });
        }
    }
    Ok(found)
}

/// The changes to a document that a device at version `theirs` is missing,
/// or `None` when it has them all. `theirs` is `None` for a new document.
pub fn export_since(db: &Connection, id: &str, theirs: Option<&[u8]>) -> Result<Option<RemoteDoc>> {
    let tx = db.unchecked_transaction()?;
    export(&tx, id, &decode_known(theirs)?)
}

pub fn missing(db: &Connection, buckets: &[u8], theirs: &[DocVersion]) -> Result<Vec<RemoteDoc>> {
    let theirs: HashMap<&str, &[u8]> = theirs
        .iter()
        .map(|v| (v.id.as_str(), v.version.as_slice()))
        .collect();
    let tx = db.unchecked_transaction()?;
    let mut missing = vec![];
    for mine in versions(&tx, buckets)? {
        let known = decode_known(theirs.get(mine.id.as_str()).copied())?;
        // Only load documents the stored version says the device lacks.
        if !known.includes_vv(&crdt::decode_version(&mine.version)?) {
            missing.extend(export(&tx, &mine.id, &known)?);
        }
    }
    Ok(missing)
}

fn decode_known(version: Option<&[u8]>) -> Result<VersionVector> {
    version.map_or_else(|| Ok(VersionVector::new()), crdt::decode_version)
}

fn export(db: &Connection, id: &str, theirs: &VersionVector) -> Result<Option<RemoteDoc>> {
    let Some((kind, doc)) = read_doc(db, id)? else {
        return Ok(None);
    };
    if theirs.includes_vv(&doc.oplog_vv()) {
        return Ok(None);
    }
    Ok(Some(RemoteDoc {
        id: id.into(),
        kind,
        bytes: doc.export(ExportMode::updates(theirs))?,
    }))
}

/// A read-only view of the database for the sync thread, so reconciling
/// never waits behind saves in the storage worker.
pub fn open_reader(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(3))?;
    Ok(connection)
}

fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() || name.chars().count() > 100 {
        Err("Use a notebook name between 1 and 100 characters".into())
    } else {
        Ok(())
    }
}

fn unique_name(db: &Connection, id: &str, name: &str) -> Result<()> {
    let exists: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM notebooks WHERE name=?1 COLLATE NOCASE AND id != ?2 AND deleted_at IS NULL)", params![name.trim(), id], |r| r.get(0))?;
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
    xdg_path("XDG_DATA_HOME", ".local/share", "notebook/notebook.db")
}

pub(crate) fn xdg_path(var: &str, fallback: &str, file: &str) -> PathBuf {
    let root = std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
                .join(fallback)
        });
    root.join(file)
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

pub type Outbox = tokio::sync::mpsc::UnboundedSender<DocChange>;

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
    /// Edits from the editor's copy of a note, as Loro updates.
    Edit {
        id: String,
        updates: Vec<Vec<u8>>,
        sequence: u64,
    },
    Mutate(Mutation),
    Preferences(Preferences),
    /// `from` is the sending device's endpoint ID.
    Remote {
        docs: Vec<RemoteDoc>,
        from: String,
    },
    /// Starts or stops forwarding document changes to the sync thread.
    Attach(Option<Outbox>),
    Flush,
}

pub enum Event {
    Ready {
        default_notebook_id: String,
        notebooks: Vec<Notebook>,
        note: Note,
        snapshot: Vec<u8>,
        preferences: Preferences,
    },
    Listed {
        notes: Vec<NoteSummary>,
        counts: NoteCounts,
        generation: u64,
    },
    Loaded {
        note: Option<Note>,
        snapshot: Vec<u8>,
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
    /// Changes to a note that the editor's copy should import.
    NoteDelta {
        id: String,
        delta: Vec<u8>,
    },
    /// Another device changed these notes, and maybe the notebooks.
    Remote {
        notes: Vec<NoteState>,
        notebooks: Option<Vec<Notebook>>,
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
            let mut outbox: Option<Outbox> = None;
            for command in incoming {
                if let Command::Attach(sender) = command {
                    outbox = sender;
                    continue;
                }
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
                                snapshot: repo.snapshot(&note.id)?.unwrap_or_default(),
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
                        Command::Load { id, generation } => {
                            let note = repo.load(id)?;
                            Some(Event::Loaded {
                                snapshot: match note {
                                    Some(_) => repo.snapshot(id)?.unwrap_or_default(),
                                    None => vec![],
                                },
                                note,
                                generation: *generation,
                            })
                        }
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
                        Command::Edit {
                            id,
                            updates,
                            sequence,
                        } => {
                            repo.edit(id, updates)?;
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
                        Command::Remote { docs, from } => {
                            let (notes, notebooks_changed) = repo.apply_remote(docs, from)?;
                            (!notes.is_empty() || notebooks_changed).then(|| Event::Remote {
                                notes,
                                notebooks: notebooks_changed
                                    .then(|| repo.notebooks())
                                    .transpose()
                                    .ok()
                                    .flatten(),
                            })
                        }
                        Command::Preferences(prefs) => {
                            repo.save_preferences(prefs)?;
                            None
                        }
                        Command::Attach(_) => None,
                        Command::Flush => Some(Event::Flushed),
                    })
                })();
                // The editor's copies must see every change before anything acts on them.
                if let Some(repo) = repository.as_mut() {
                    for change in repo.take_changes() {
                        // The editor's copy skips the changes it already has.
                        if change.kind == Kind::Note {
                            let _ = outgoing.send(Event::NoteDelta {
                                id: change.id.clone(),
                                delta: change.delta.clone(),
                            });
                        }
                        if let Some(sender) = &outbox
                            && sender.send(change).is_err()
                        {
                            outbox = None;
                        }
                    }
                }
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
