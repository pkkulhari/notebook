use notebook_core::{
    crdt::{self, Kind},
    model::*,
    storage::{Mutation, RemoteDoc, Repository},
};
use rusqlite::Connection;
use std::path::Path;

/// Sends `to` everything `from` has that it lacks, like one sync session direction.
fn reconcile(from: &Repository, to: &mut Repository, from_name: &str) -> usize {
    let buckets = crdt::differing_buckets(&from.digest().unwrap(), &to.digest().unwrap());
    let docs = from
        .missing(&buckets, &to.versions(&buckets).unwrap())
        .unwrap();
    let sent = docs.len();
    to.apply_remote(&docs, from_name).unwrap();
    sent
}

fn sync(a: &mut Repository, b: &mut Repository) {
    reconcile(a, b, "a");
    reconcile(b, a, "b");
}

/// A notebook's ID and name, and a note's ID, notebook, text, and trash state.
type Books = Vec<(String, String)>;
type Notes = Vec<(String, String, String, bool)>;

fn visible(repo: &Repository) -> (Books, Notes) {
    let books = repo
        .notebooks()
        .unwrap()
        .into_iter()
        .map(|b| (b.id, b.name))
        .collect();
    let mut notes: Vec<_> = repo
        .list(&Filter::All, "")
        .unwrap()
        .into_iter()
        .chain(repo.list(&Filter::Trash, "").unwrap())
        .map(|s| {
            let note = repo.load(&s.id).unwrap().unwrap();
            (note.id, note.notebook_id, note.body, note.deleted)
        })
        .collect();
    notes.sort();
    (books, notes)
}

fn open(dir: &Path, name: &str) -> Repository {
    Repository::open(&dir.join(name)).unwrap()
}

#[test]
fn concurrent_edits_to_one_note_keep_both_sides() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = (open(dir.path(), "a.db"), open(dir.path(), "b.db"));
    let note = Note::blank();
    a.create_note(&note).unwrap();
    a.save(&note.id, "Hello world").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(b.load(&note.id).unwrap().unwrap().body, "Hello world");

    a.save(&note.id, "Hello brave world").unwrap();
    b.save(&note.id, "Hello world!").unwrap();
    sync(&mut a, &mut b);
    let merged = a.load(&note.id).unwrap().unwrap().body;
    assert_eq!(merged, "Hello brave world!");
    assert_eq!(visible(&a), visible(&b));
    assert_eq!(a.digest().unwrap(), b.digest().unwrap());
    assert_eq!(reconcile(&a, &mut b, "a"), 0);
    assert_eq!(reconcile(&b, &mut a, "b"), 0);
}

#[test]
fn notes_can_arrive_before_their_notebook() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = (open(dir.path(), "a.db"), open(dir.path(), "b.db"));
    let book = uuid::Uuid::new_v4().to_string();
    a.mutate(&Mutation::CreateNotebook {
        id: book.clone(),
        name: "Garden".into(),
    })
    .unwrap();
    let note = Note::blank();
    a.create_note(&note).unwrap();
    a.mutate(&Mutation::Move {
        id: note.id.clone(),
        notebook_id: book.clone(),
    })
    .unwrap();
    let note_doc = a.export_since(&note.id, None).unwrap().unwrap();
    b.apply_remote(&[note_doc], "a").unwrap();
    assert_eq!(
        b.load(&note.id).unwrap().unwrap().notebook_id,
        DEFAULT_NOTEBOOK
    );
    let book_doc = a.export_since(&book, None).unwrap().unwrap();
    b.apply_remote(&[book_doc], "a").unwrap();
    assert_eq!(b.load(&note.id).unwrap().unwrap().notebook_id, book);
}

#[test]
fn deleting_a_notebook_wins_over_a_concurrent_move_into_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = (open(dir.path(), "a.db"), open(dir.path(), "b.db"));
    let book = uuid::Uuid::new_v4().to_string();
    a.mutate(&Mutation::CreateNotebook {
        id: book.clone(),
        name: "Drafts".into(),
    })
    .unwrap();
    let note = Note::blank();
    a.create_note(&note).unwrap();
    sync(&mut a, &mut b);
    a.mutate(&Mutation::DeleteNotebook { id: book.clone() })
        .unwrap();
    b.mutate(&Mutation::Move {
        id: note.id.clone(),
        notebook_id: book.clone(),
    })
    .unwrap();
    sync(&mut a, &mut b);
    for repo in [&a, &b] {
        assert_eq!(
            repo.load(&note.id).unwrap().unwrap().notebook_id,
            DEFAULT_NOTEBOOK
        );
        assert_eq!(repo.notebooks().unwrap().len(), 1);
    }
    assert_eq!(visible(&a), visible(&b));
}

#[test]
fn notebooks_created_apart_with_one_name_are_both_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = (open(dir.path(), "a.db"), open(dir.path(), "b.db"));
    for repo in [&mut a, &mut b] {
        repo.mutate(&Mutation::CreateNotebook {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Work".into(),
        })
        .unwrap();
    }
    sync(&mut a, &mut b);
    let names: Vec<_> = a.notebooks().unwrap().into_iter().map(|b| b.name).collect();
    assert_eq!(names, ["Default", "Work", "Work (2)"]);
    assert_eq!(visible(&a), visible(&b));
    let second = a.notebooks().unwrap()[2].id.clone();
    a.mutate(&Mutation::RenameNotebook {
        id: second,
        name: "Work at home".into(),
    })
    .unwrap();
    assert!(
        a.mutate(&Mutation::CreateNotebook {
            id: "x".into(),
            name: "work".into()
        })
        .is_err()
    );
}

#[test]
fn a_copied_database_merges_without_duplicating_notes() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = open(dir.path(), "a.db");
    let note = Note::blank();
    a.create_note(&note).unwrap();
    a.save(&note.id, "Shared line").unwrap();
    drop(a);
    std::fs::copy(dir.path().join("a.db"), dir.path().join("b.db")).unwrap();
    let (mut a, mut b) = (open(dir.path(), "a.db"), open(dir.path(), "b.db"));
    a.save(&note.id, "Shared line\nFrom a").unwrap();
    b.save(&note.id, "From b\nShared line").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(
        a.load(&note.id).unwrap().unwrap().body,
        "From b\nShared line\nFrom a"
    );
    assert_eq!(visible(&a), visible(&b));
}

/// Writes a database exactly as Notebook 0.2 did, before documents existed.
fn version_one(path: &Path, notes: &[(&str, &str)]) {
    let db = Connection::open(path).unwrap();
    db.execute_batch(
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
         INSERT INTO notebooks VALUES ('00000000-0000-0000-0000-000000000000', 'Default', 1, 1, 1, NULL);
         INSERT INTO notebooks VALUES ('trips', 'Trips', 2, 2, 1, NULL);
         PRAGMA user_version=1;",
    )
    .unwrap();
    for (id, body) in notes {
        db.execute(
            "INSERT INTO notes VALUES (?1, 'trips', ?2, 'label', '', 3, 4, 1, NULL)",
            [id, body],
        )
        .unwrap();
    }
}

#[test]
fn version_one_databases_migrate_and_merge_with_copies() {
    let dir = tempfile::tempdir().unwrap();
    let body = "# Lisbon 🌿\n\nनमस्ते and trams";
    version_one(&dir.path().join("a.db"), &[("n1", body), ("n2", "Porto")]);
    std::fs::copy(dir.path().join("a.db"), dir.path().join("b.db")).unwrap();
    // Each copy migrates on its own device, as after restoring a backup.
    let (mut a, mut b) = (open(dir.path(), "a.db"), open(dir.path(), "b.db"));
    assert_eq!(a.load("n1").unwrap().unwrap().body, body);
    assert_eq!(a.load("n1").unwrap().unwrap().notebook_id, "trips");
    assert_eq!(a.list(&Filter::All, "trams").unwrap().len(), 1);
    assert_eq!(visible(&a), visible(&b));
    assert_eq!(a.digest().unwrap(), b.digest().unwrap());
    b.save("n2", "Porto and Braga").unwrap();
    sync(&mut a, &mut b);
    assert_eq!(a.load("n1").unwrap().unwrap().body, body);
    assert_eq!(a.load("n2").unwrap().unwrap().body, "Porto and Braga");
    assert_eq!(visible(&a), visible(&b));
    // The name index no longer blocks names that arrive from other devices.
    a.mutate(&Mutation::DeleteNotebook { id: "trips".into() })
        .unwrap();
    assert_eq!(a.load("n1").unwrap().unwrap().notebook_id, DEFAULT_NOTEBOOK);
}

#[test]
fn editor_updates_apply_in_order_or_not_at_all() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open(dir.path(), "a.db");
    let note = Note::blank();
    repo.create_note(&note).unwrap();
    let editor = loro::LoroDoc::from_snapshot(&repo.snapshot(&note.id).unwrap().unwrap()).unwrap();
    editor.set_peer_id(crdt::random_peer()).unwrap();
    let text = crdt::text(&editor);
    let vv = editor.oplog_vv();
    text.insert(0, "first").unwrap();
    editor.commit();
    let first = editor.export(loro::ExportMode::updates(&vv)).unwrap();
    let vv = editor.oplog_vv();
    text.insert(5, " second").unwrap();
    editor.commit();
    let second = editor.export(loro::ExportMode::updates(&vv)).unwrap();
    // The second edit depends on the first, so it can't be saved alone.
    assert!(repo.edit(&note.id, std::slice::from_ref(&second)).is_err());
    assert_eq!(repo.load(&note.id).unwrap().unwrap().body, "");
    repo.edit(&note.id, &[first, second]).unwrap();
    assert_eq!(repo.load(&note.id).unwrap().unwrap().body, "first second");
    // The editor must also receive the storage-side change it lacks.
    let changes = repo.take_changes();
    let saved = changes.last().unwrap();
    editor.import(&saved.delta).unwrap();
    assert!(
        editor.oplog_vv().includes_vv(
            &crdt::decode_version(&repo.versions(&[crdt::bucket(&note.id)]).unwrap()[0].version)
                .unwrap()
        )
    );
}

#[test]
fn devices_reject_malformed_documents() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = open(dir.path(), "a.db");
    let doc = loro::LoroDoc::new();
    crdt::meta(&doc).insert("name", "Sneaky").unwrap();
    doc.commit();
    let bytes = doc.export(loro::ExportMode::all_updates()).unwrap();
    for id in ["", DEFAULT_NOTEBOOK, "../etc", &"x".repeat(65)] {
        let remote = RemoteDoc {
            id: id.into(),
            kind: Kind::Notebook,
            bytes: bytes.clone(),
        };
        assert!(repo.apply_remote(&[remote], "peer").is_err(), "{id:?}");
    }
    let note = Note::blank();
    repo.create_note(&note).unwrap();
    let wrong_kind = RemoteDoc {
        id: note.id.clone(),
        kind: Kind::Notebook,
        bytes,
    };
    assert!(repo.apply_remote(&[wrong_kind], "peer").is_err());
    assert_eq!(repo.notebooks().unwrap().len(), 1);
}

/// A small deterministic generator, so failures reproduce from the seed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn random_edits_on_three_devices_converge() {
    for seed in 1..=12u64 {
        let dir = tempfile::tempdir().unwrap();
        let mut devices: Vec<Repository> = (0..3)
            .map(|i| open(dir.path(), &format!("{i}.db")))
            .collect();
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        for _ in 0..60 {
            let device = rng.below(3);
            let repo = &mut devices[device];
            let notes: Vec<String> = repo
                .list(&Filter::All, "")
                .unwrap()
                .into_iter()
                .chain(repo.list(&Filter::Trash, "").unwrap())
                .map(|s| s.id)
                .collect();
            let books: Vec<String> = repo
                .notebooks()
                .unwrap()
                .into_iter()
                .map(|b| b.id)
                .collect();
            match rng.below(9) {
                0 | 1 => repo.create_note(&Note::blank()).unwrap(),
                2..=4 if !notes.is_empty() => {
                    let id = &notes[rng.below(notes.len())];
                    let body = repo.load(id).unwrap().unwrap().body;
                    let chars: Vec<char> = body.chars().collect();
                    let at = rng.below(chars.len() + 1);
                    let cut = rng.below(3).min(chars.len() - at);
                    let word = ["fern ", "moss ", "🌿", "é", "\n"][rng.below(5)];
                    let edited: String = chars[..at]
                        .iter()
                        .chain(word.chars().collect::<Vec<_>>().iter())
                        .chain(chars[at + cut..].iter())
                        .collect();
                    repo.save(id, &edited).unwrap();
                }
                5 => {
                    let _ = repo.mutate(&Mutation::CreateNotebook {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: format!("Book {}", rng.below(4)),
                    });
                }
                6 if books.len() > 1 => {
                    let id = books[1 + rng.below(books.len() - 1)].clone();
                    let _ = if rng.below(2) == 0 {
                        repo.mutate(&Mutation::DeleteNotebook { id })
                    } else {
                        repo.mutate(&Mutation::RenameNotebook {
                            id,
                            name: format!("Renamed {}", rng.below(4)),
                        })
                    };
                }
                7 if !notes.is_empty() && !books.is_empty() => {
                    repo.mutate(&Mutation::Move {
                        id: notes[rng.below(notes.len())].clone(),
                        notebook_id: books[rng.below(books.len())].clone(),
                    })
                    .unwrap();
                }
                8 if !notes.is_empty() => {
                    let id = notes[rng.below(notes.len())].clone();
                    let deleted = repo.load(&id).unwrap().unwrap().deleted;
                    repo.mutate(&if deleted {
                        Mutation::Restore { id }
                    } else {
                        Mutation::Trash { id }
                    })
                    .unwrap();
                }
                _ => {}
            }
            if rng.below(3) == 0 {
                let (x, y) = (rng.below(3), rng.below(3));
                if let Ok([from, to]) = devices.get_disjoint_mut([x, y]) {
                    reconcile(from, to, "peer");
                }
            }
        }
        // Everyone meets once, around the ring twice, so changes pass through.
        for _ in 0..2 {
            for pair in [[0, 1], [1, 2], [2, 0]] {
                let [from, to] = devices.get_disjoint_mut(pair).unwrap();
                sync(from, to);
            }
        }
        let first = visible(&devices[0]);
        assert!(
            first.1.iter().any(|(_, _, body, _)| !body.is_empty()),
            "seed {seed}: no text was written"
        );
        for (i, repo) in devices.iter().enumerate().skip(1) {
            assert_eq!(visible(repo), first, "seed {seed}: device {i} diverged");
            assert_eq!(
                repo.digest().unwrap(),
                devices[0].digest().unwrap(),
                "seed {seed}"
            );
        }
        drop(devices);
        let reopened = open(dir.path(), "0.db");
        assert_eq!(visible(&reopened), first, "seed {seed}: reopened");
    }
}

#[test]
fn changes_that_arrive_before_their_history_wait_for_it() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = (open(dir.path(), "a.db"), open(dir.path(), "b.db"));
    let note = Note::blank();
    a.create_note(&note).unwrap();
    a.save(&note.id, "one").unwrap();
    let before = a.versions(&[crdt::bucket(&note.id)]).unwrap();
    let before = &before.iter().find(|v| v.id == note.id).unwrap().version;
    a.save(&note.id, "one two").unwrap();
    // Only the newest edit, without the history it builds on.
    let newest = a.export_since(&note.id, Some(before)).unwrap().unwrap();
    b.apply_remote(std::slice::from_ref(&newest), "a").unwrap();
    assert!(b.load(&note.id).unwrap().is_none());
    assert_eq!(b.versions(&[crdt::bucket(&note.id)]).unwrap(), []);
    sync(&mut a, &mut b);
    assert_eq!(b.load(&note.id).unwrap().unwrap().body, "one two");
    b.apply_remote(&[newest], "a").unwrap();
    assert_eq!(visible(&a), visible(&b));
}
