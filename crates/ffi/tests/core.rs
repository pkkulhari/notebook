use notebook_core::storage::{self, Repository};
use notebook_ffi::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<CoreEvent>>,
    statuses: Mutex<Vec<SyncStatus>>,
}

impl CoreListener for Recorder {
    fn on_event(&self, event: CoreEvent) {
        self.events.lock().unwrap().push(event);
    }

    fn on_sync_status(&self, status: SyncStatus) {
        self.statuses.lock().unwrap().push(status);
    }
}

impl Recorder {
    fn wait<T>(&self, what: &str, find: impl Fn(&CoreEvent) -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(found) = self.events.lock().unwrap().iter().find_map(&find) {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "no {what}: {:?}",
                self.events.lock().unwrap()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn position(&self, matches: impl Fn(&CoreEvent) -> bool) -> Option<usize> {
        self.events.lock().unwrap().iter().position(matches)
    }

    fn saved(&self, id: &str) {
        self.wait("save", |e| {
            matches!(e, CoreEvent::Saved { id: saved } if saved == id).then_some(())
        });
    }
}

struct Phone {
    dir: tempfile::TempDir,
    core: Arc<Core>,
    events: Arc<Recorder>,
}

impl Phone {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Recorder::default());
        let core = Core::start(
            CoreConfig {
                database_path: dir.path().join("notebook.db").to_string_lossy().into(),
                sync_config_path: dir.path().join("sync.json").to_string_lossy().into(),
                device_name: "Test phone".into(),
            },
            events.clone(),
        );
        Self { dir, core, events }
    }

    fn ready(&self) -> String {
        self.core.initialize();
        let (note, snapshot) = self.events.wait("Ready", |e| match e {
            CoreEvent::Ready { note, snapshot, .. } => Some((note.clone(), snapshot.clone())),
            _ => None,
        });
        let id = note.id.clone();
        assert_eq!(self.core.open_draft(note, snapshot), "");
        id
    }

    fn save_all(&self) {
        loop {
            let wait = self.core.tick();
            if wait < 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(wait as u64));
        }
    }

    /// Imports the note's storage changes in order, as Kotlin does.
    fn import_deltas(&self, id: &str) {
        let deltas: Vec<Vec<u8>> = self
            .events
            .events
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                CoreEvent::NoteDelta { id: of, delta } if of == id => Some(delta.clone()),
                _ => None,
            })
            .collect();
        for delta in deltas {
            assert_eq!(self.core.import_delta(id.into(), delta), Applied::default());
        }
    }

    fn stored(&self, id: &str) -> String {
        Repository::open(&self.dir.path().join("notebook.db"))
            .unwrap()
            .load(id)
            .unwrap()
            .unwrap()
            .body
    }
}

/// Applies edits the way an `Editable` does, in UTF-16 units.
fn apply(text: &str, applied: &Applied) -> String {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    for edit in &applied.edits {
        match edit {
            TextEdit::Insert { at, text } => {
                let at = *at as usize;
                units.splice(at..at, text.encode_utf16());
            }
            TextEdit::Delete { at, len } => {
                units.drain(*at as usize..(*at + *len) as usize);
            }
        }
    }
    String::from_utf16(&units).unwrap()
}

#[test]
fn ready_brings_a_note_and_sync_reports_its_status() {
    let phone = Phone::start();
    phone.ready();
    let deadline = Instant::now() + Duration::from_secs(10);
    while phone.events.statuses.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "no sync status");
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = phone.events.statuses.lock().unwrap()[0].clone();
    assert_eq!(status.device_name, "Test phone");
    assert!(!status.enabled && !status.running && !status.suspended);
    assert_eq!(status.pairing, Pairing::Idle);
}

#[test]
fn typing_after_an_emoji_is_saved_after_a_pause() {
    let phone = Phone::start();
    let id = phone.ready();
    phone
        .core
        .replace(id.clone(), 0, 0, "🌿 tea".into())
        .unwrap();
    phone.core.replace(id.clone(), 2, 0, "!".into()).unwrap();
    assert_eq!(phone.core.draft_text(id.clone()).unwrap(), "🌿! tea");
    let first = phone.core.tick();
    assert!((1..=300).contains(&first), "{first}");
    phone.save_all();
    phone.events.saved(&id);
    assert_eq!(phone.stored(&id), "🌿! tea");
}

#[test]
fn imported_changes_come_back_in_utf16_units() {
    let phone = Phone::start();
    let id = phone.ready();
    phone
        .core
        .replace(id.clone(), 0, 0, "🌿! tea".into())
        .unwrap();
    phone.save_all();
    phone.events.saved(&id);
    phone.import_deltas(&id);
    let reader = storage::open_reader(&phone.dir.path().join("notebook.db")).unwrap();
    let shared = storage::export_since(&reader, &id, None).unwrap().unwrap();
    let mut other = Repository::open(&phone.dir.path().join("other.db")).unwrap();
    other.apply_remote(&[shared], "phone").unwrap();
    other.take_changes();
    other.save(&id, "🌿 green! tea").unwrap();
    let delta = other.take_changes().pop().unwrap().delta;
    let applied = phone.core.import_delta(id.clone(), delta);
    assert_eq!(
        applied.edits,
        // Just after the emoji, which is two UTF-16 units.
        [TextEdit::Insert {
            at: 2,
            text: " green".into()
        }]
    );
    assert_eq!(apply("🌿! tea", &applied), "🌿 green! tea");
    assert_eq!(phone.core.draft_text(id).unwrap(), "🌿 green! tea");
}

#[test]
fn positions_outside_the_draft_are_out_of_sync() {
    let phone = Phone::start();
    let id = phone.ready();
    phone
        .core
        .replace(id.clone(), 0, 0, "🌿 ok".into())
        .unwrap();
    let out_of_sync = |result: Result<(), CoreError>| {
        assert!(
            matches!(result, Err(CoreError::OutOfSync { .. })),
            "{result:?}"
        );
    };
    out_of_sync(phone.core.replace(id.clone(), 6, 0, "x".into()));
    out_of_sync(phone.core.replace(id.clone(), -1, 0, "x".into()));
    out_of_sync(phone.core.replace(id.clone(), 1, 0, "x".into()));
    out_of_sync(phone.core.replace(id.clone(), 3, 9, String::new()));
    out_of_sync(phone.core.replace(id.clone(), 4, -1, "x".into()));
    out_of_sync(phone.core.replace("no-such-note".into(), 0, 0, "x".into()));
    assert_eq!(phone.core.draft_text(id).unwrap(), "🌿 ok");
}

#[test]
fn lines_in_code_blocks_are_found_in_the_current_text() {
    let phone = Phone::start();
    let id = phone.ready();
    let text = "🌿\n```\n- code";
    phone.core.replace(id.clone(), 0, 0, text.into()).unwrap();
    let end = text.encode_utf16().count() as i32;
    assert!(phone.core.line_in_code_block(id.clone(), end));
    // The first line, before the fence, isn't in it.
    assert!(!phone.core.line_in_code_block(id.clone(), 2));
    // A position inside the emoji's surrogate pair, or without a draft.
    assert!(!phone.core.line_in_code_block(id, 1));
    assert!(!phone.core.line_in_code_block("no-such-note".into(), 0));
}

#[test]
fn flushed_follows_every_earlier_save() {
    let phone = Phone::start();
    let id = phone.ready();
    let new = phone.core.create_note();
    phone
        .core
        .replace(id.clone(), 0, 0, "first".into())
        .unwrap();
    phone
        .core
        .replace(new.id.clone(), 0, 0, "second".into())
        .unwrap();
    phone.core.flush();
    phone
        .events
        .wait("Flushed", |e| matches!(e, CoreEvent::Flushed).then_some(()));
    let flushed = phone.events.position(|e| matches!(e, CoreEvent::Flushed));
    for id in [&id, &new.id] {
        let saved = phone
            .events
            .position(|e| matches!(e, CoreEvent::Saved { id: saved } if saved == id));
        assert!(saved < flushed, "{:?}", phone.events.events.lock().unwrap());
    }
    assert_eq!(phone.stored(&id), "first");
    assert_eq!(phone.stored(&new.id), "second");
    assert_eq!(new.notebook_id, notebook_core::model::DEFAULT_NOTEBOOK);
    assert_eq!(phone.core.tick(), -1);
}

#[test]
fn a_failed_save_is_kept_and_retried_with_later_edits() {
    let phone = Phone::start();
    let id = phone.ready();
    let inspection = rusqlite::Connection::open(phone.dir.path().join("notebook.db")).unwrap();
    inspection
        .execute_batch("CREATE TRIGGER fail_save BEFORE UPDATE OF body ON notes BEGIN SELECT RAISE(ABORT, 'test failure'); END")
        .unwrap();
    phone
        .core
        .replace(id.clone(), 0, 0, "unsaved".into())
        .unwrap();
    phone.core.flush();
    let retryable = phone.events.wait("an error", |e| match e {
        CoreEvent::Error {
            operation,
            retryable,
            ..
        } => Some((*operation, *retryable)),
        _ => None,
    });
    assert_eq!(retryable, (Operation::Save, true));
    assert!(phone.core.has_failures());
    phone
        .core
        .replace(id.clone(), 7, 0, " newest".into())
        .unwrap();
    inspection.execute_batch("DROP TRIGGER fail_save").unwrap();
    phone.core.retry();
    phone.events.saved(&id);
    let deadline = Instant::now() + Duration::from_secs(10);
    while phone.stored(&id) != "unsaved newest" || phone.core.has_failures() {
        assert!(Instant::now() < deadline, "{:?}", phone.stored(&id));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn dropping_the_core_stops_its_threads() {
    let phone = Phone::start();
    phone.ready();
    let Phone {
        dir: _dir,
        core,
        events,
    } = phone;
    drop(core);
    // The event thread and the sync thread each held the listener.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Arc::strong_count(&events) > 1 {
        assert!(Instant::now() < deadline, "threads still hold the listener");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn markdown_ranges_are_utf16_and_links_are_filtered() {
    let text =
        "# 🌿 Title\n\n[site](https://example.org) [x](file:///etc) `🌿`\n\n```\n- code\n```";
    let document = parse_markdown(text.into());
    let units: Vec<u16> = text.encode_utf16().collect();
    let decode =
        |start: i32, end: i32| String::from_utf16(&units[start as usize..end as usize]).unwrap();
    let span = |style| {
        let span = document
            .spans()
            .into_iter()
            .find(|s| s.style == style)
            .unwrap();
        decode(span.start, span.end)
    };
    assert_eq!(span(Style::H1).trim_end(), "# 🌿 Title");
    assert_eq!(span(Style::Code), "`🌿`");
    let site = text.encode_utf16().count() as i32
        - "[site](https://example.org) [x](file:///etc) `🌿`\n\n```\n- code\n```"
            .encode_utf16()
            .count() as i32;
    assert_eq!(
        document.link_at(site + 1).as_deref(),
        Some("https://example.org")
    );
    let file = site + "[site](https://example.org) ".len() as i32;
    assert_eq!(document.link_at(file + 1), None);
    assert_eq!(
        list_enter("- [ ] 🌿".into()),
        Some(ListEnter::Continue {
            marker: 6,
            next: "\n- [ ] ".into()
        })
    );
    assert_eq!(list_enter("- ".into()), Some(ListEnter::End));
}

#[test]
fn hidden_ranges_never_cross_a_line_break() {
    let text = "Title\n===\n\n**bold**\n\nNext";
    let document = parse_markdown(text.into());
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = units.len() as i32;
    let hidden: Vec<String> = document
        .hidden_outside(end, end)
        .iter()
        .map(|r| String::from_utf16(&units[r.start as usize..r.end as usize]).unwrap())
        .collect();
    assert_eq!(hidden, ["===", "**", "**"]);
    // The cursor in "Next" makes only that paragraph active.
    let next = text.encode_utf16().count() as i32 - 4;
    assert_eq!(
        document.active_blocks(end, end),
        [TextRange {
            start: next,
            end: end + 1
        }]
    );
}
