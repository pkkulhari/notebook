use notebook::{
    model::*,
    storage::{Command, Event, Mutation, Repository, spawn_worker},
};
use rusqlite::Connection;
use std::time::Duration;

#[test]
fn first_launch_and_restart_preserve_markdown_and_cursor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notebook.db");
    let mut repo = Repository::open(&path).unwrap();
    let (books, note, _) = repo.bootstrap().unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(note.notebook_id, DEFAULT_NOTEBOOK);
    assert!(note.body.is_empty());
    let body = "# नमस्ते 🌿\n\n**Keep** _every_ `byte`.\n";
    repo.save(&note.id, body).unwrap();
    let summary = repo.list(&Filter::All, "").unwrap().remove(0);
    assert!(summary.updated_at > 0 && summary.updated_at <= now_millis());
    assert_eq!(
        repo.list(&Filter::All, "Keep").unwrap()[0].updated_at,
        summary.updated_at
    );
    repo.save_preferences(&Preferences {
        selected_note: Some(note.id.clone()),
        cursor: 12,
        ..Default::default()
    })
    .unwrap();
    drop(repo);
    let mut repo = Repository::open(&path).unwrap();
    let (_, restored, prefs) = repo.bootstrap().unwrap();
    assert_eq!(restored.id, note.id);
    assert_eq!(restored.body, body);
    assert_eq!(prefs.cursor, 12);
    assert_eq!(repo.list(&Filter::All, "").unwrap().len(), 1);
}

#[test]
fn notebook_deletion_moves_live_and_trashed_notes_and_protects_default() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(&dir.path().join("notes.db")).unwrap();
    let book = uuid::Uuid::new_v4().to_string();
    repo.mutate(&Mutation::CreateNotebook {
        id: book.clone(),
        name: "Work".into(),
    })
    .unwrap();
    let a = Note::blank();
    let b = Note::blank();
    for note in [&a, &b] {
        repo.create_note(note).unwrap();
        repo.mutate(&Mutation::Move {
            id: note.id.clone(),
            notebook_id: book.clone(),
        })
        .unwrap();
    }
    repo.mutate(&Mutation::Trash { id: b.id.clone() }).unwrap();
    assert_eq!(
        repo.list(&Filter::Notebook(book.clone()), "")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(repo.list(&Filter::Trash, "").unwrap().len(), 1);
    let counts = repo.note_counts().unwrap();
    assert_eq!(counts.get(&Filter::Notebook(book.clone())), Some(&1));
    assert_eq!(counts.get(&Filter::All), Some(&1));
    assert_eq!(counts.get(&Filter::Trash), Some(&1));
    assert_eq!(
        counts
            .get(&Filter::Notebook(DEFAULT_NOTEBOOK.into()))
            .copied()
            .unwrap_or(0),
        0
    );
    repo.mutate(&Mutation::DeleteNotebook { id: book }).unwrap();
    assert_eq!(
        repo.note_counts()
            .unwrap()
            .get(&Filter::Notebook(DEFAULT_NOTEBOOK.into())),
        Some(&1)
    );
    assert_eq!(
        repo.load(&a.id).unwrap().unwrap().notebook_id,
        DEFAULT_NOTEBOOK
    );
    let trashed = repo.load(&b.id).unwrap().unwrap();
    assert_eq!(trashed.notebook_id, DEFAULT_NOTEBOOK);
    assert!(trashed.deleted);
    repo.mutate(&Mutation::Restore { id: b.id }).unwrap();
    assert_eq!(repo.list(&Filter::All, "").unwrap().len(), 2);
    let counts = repo.note_counts().unwrap();
    assert_eq!(counts.get(&Filter::All), Some(&2));
    assert_eq!(
        counts.get(&Filter::Notebook(DEFAULT_NOTEBOOK.into())),
        Some(&2)
    );
    assert_eq!(counts.get(&Filter::Trash).copied().unwrap_or(0), 0);
    assert!(
        repo.mutate(&Mutation::DeleteNotebook {
            id: DEFAULT_NOTEBOOK.into()
        })
        .is_err()
    );
    assert!(
        repo.mutate(&Mutation::RenameNotebook {
            id: DEFAULT_NOTEBOOK.into(),
            name: "Other".into()
        })
        .is_err()
    );
}

#[test]
fn search_is_literal_global_and_updates_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(&dir.path().join("notes.db")).unwrap();
    let note = Note::blank();
    repo.create_note(&note).unwrap();
    repo.save(&note.id, "# Garden\n\nWild flowers and trees")
        .unwrap();
    assert_eq!(repo.list(&Filter::All, "flowers").unwrap().len(), 1);
    assert_eq!(repo.list(&Filter::All, "flow").unwrap().len(), 1);
    for query in ["\"", "*", "a OR b", "(hello)", "name:thing", "NEAR(a b)"] {
        assert!(repo.list(&Filter::All, query).is_ok(), "{query}");
    }
    repo.save(&note.id, "Only mountains").unwrap();
    assert!(repo.list(&Filter::All, "flowers").unwrap().is_empty());
    repo.mutate(&Mutation::Trash { id: note.id }).unwrap();
    assert!(repo.list(&Filter::All, "mountains").unwrap().is_empty());
    assert_eq!(repo.list(&Filter::Trash, "mountains").unwrap().len(), 1);
}

#[test]
fn failed_transaction_cannot_leave_content_without_a_change_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.db");
    let mut repo = Repository::open(&path).unwrap();
    let note = Note::blank();
    repo.create_note(&note).unwrap();
    repo.save(&note.id, "Before").unwrap();
    let inspect = Connection::open(&path).unwrap();
    let before: i64 = inspect
        .query_row("SELECT count(*) FROM changes", [], |r| r.get(0))
        .unwrap();
    inspect.execute_batch("CREATE TRIGGER reject_changes BEFORE INSERT ON changes BEGIN SELECT RAISE(ABORT, 'simulated disk failure'); END;").unwrap();
    assert!(repo.save(&note.id, "After").is_err());
    assert_eq!(repo.load(&note.id).unwrap().unwrap().body, "Before");
    let after: i64 = inspect
        .query_row("SELECT count(*) FROM changes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(before, after);
    inspect
        .execute_batch("DROP TRIGGER reject_changes")
        .unwrap();
    repo.save(&note.id, "After").unwrap();
    assert_eq!(repo.load(&note.id).unwrap().unwrap().body, "After");
}

#[test]
fn worker_orders_saves_across_switches_and_flushes_before_shutdown() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.db");
    let (commands, events) = spawn_worker(path.clone());
    let a = Note::blank();
    let b = Note::blank();
    commands.send(Command::Create(a.clone())).unwrap();
    commands.send(Command::Create(b.clone())).unwrap();
    for (id, body, sequence) in [
        (&a.id, "first", 1),
        (&b.id, "second", 1),
        (&a.id, "latest", 2),
    ] {
        commands
            .send(Command::Save {
                id: id.clone(),
                body: body.into(),
                sequence,
            })
            .unwrap();
    }
    commands.send(Command::Flush).unwrap();
    let mut acknowledgements = vec![];
    loop {
        match events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Saved { id, sequence } => acknowledgements.push((id, sequence)),
            Event::Flushed => break,
            Event::Error { message, .. } => panic!("{message}"),
            _ => {}
        }
    }
    assert_eq!(
        acknowledgements,
        vec![(a.id.clone(), 1), (b.id.clone(), 1), (a.id.clone(), 2)]
    );
    let repo = Repository::open(&path).unwrap();
    assert_eq!(repo.load(&a.id).unwrap().unwrap().body, "latest");
    assert_eq!(repo.load(&b.id).unwrap().unwrap().body, "second");
}

#[test]
fn schema_starts_at_version_one_and_reopening_preserves_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.db");
    drop(Repository::open(&path).unwrap());
    drop(Repository::open(&path).unwrap());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM notebooks", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM changes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}

#[test]
fn future_schema_versions_are_rejected_without_changing_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.db");
    let mut repo = Repository::open(&path).unwrap();
    let note = Note::blank();
    repo.create_note(&note).unwrap();
    repo.save(&note.id, "Keep this note").unwrap();
    drop(repo);
    let db = Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA user_version=2").unwrap();
    let error = Repository::open(&path)
        .err()
        .expect("must reject version 2");
    assert!(error.to_string().contains("newer version"));
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        db.query_row("SELECT body FROM notes WHERE id=?1", [&note.id], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "Keep this note"
    );
}

#[test]
fn notebook_names_are_trimmed_unique_and_reusable_after_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let mut repo = Repository::open(&dir.path().join("notes.db")).unwrap();
    repo.mutate(&Mutation::CreateNotebook {
        id: "a".into(),
        name: "  Work  ".into(),
    })
    .unwrap();
    assert_eq!(repo.notebooks().unwrap()[1].name, "Work");
    assert!(
        repo.mutate(&Mutation::CreateNotebook {
            id: "b".into(),
            name: "work".into()
        })
        .unwrap_err()
        .to_string()
        .contains("already exists")
    );
    assert!(
        repo.mutate(&Mutation::RenameNotebook {
            id: "a".into(),
            name: "default".into()
        })
        .is_err()
    );
    assert!(
        repo.mutate(&Mutation::CreateNotebook {
            id: "c".into(),
            name: "  ".into()
        })
        .is_err()
    );
    repo.mutate(&Mutation::DeleteNotebook { id: "a".into() })
        .unwrap();
    repo.mutate(&Mutation::CreateNotebook {
        id: "b".into(),
        name: "Work".into(),
    })
    .unwrap();
}
