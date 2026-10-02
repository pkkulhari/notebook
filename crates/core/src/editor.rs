//! The editor's copy of each open note, independent of any UI toolkit.
//!
//! The UI's text widget mirrors a draft's document: typing is written into the
//! document, and the document's imports and undos come back as [`TextEdit`]s
//! for the UI to apply. Drafts decide when to save and what each save holds,
//! and return storage commands for the UI to send.
//!
//! Every position is in the `Units` the drafts were created with.
use crate::{
    crdt,
    model::{Note, Units},
    storage::Command,
};
use loro::{
    ContainerTrait, ExportMode, LoroDoc, TextDelta, UndoManager, VersionVector, cursor::PosType,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Typing within this many milliseconds of the last edit undoes together.
pub const UNDO_MERGE_MS: i64 = 500;
const UNDO_STEPS: usize = 500;
/// The origin of commits made by typing, which the UI already shows.
const TYPING: &str = "typing";
/// Unsent edits are saved once typing pauses this long,
const SAVE_PAUSE: Duration = Duration::from_millis(300);
/// and at least this often while typing continues.
const SAVE_INTERVAL: Duration = Duration::from_secs(2);
/// At most this many drafts stay open; see [`Drafts::trim`].
const CACHE_LIMIT: usize = 12;

/// A change the UI applies to its text widget, in order. Each position
/// refers to the text after the previous edit, and `len` is in the same units.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextEdit {
    Insert { at: usize, text: String },
    Delete { at: usize, len: usize },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    pub edits: Vec<TextEdit>,
    /// Just after the last edit: where an undo or redo leaves the cursor.
    pub cursor: Option<usize>,
}

/// A position outside the draft's text: the UI and the draft no longer agree
/// on what the text is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutOfSync;

impl std::fmt::Display for OutOfSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the editor's text no longer matches the note")
    }
}

impl std::error::Error for OutOfSync {}

type Incoming = Arc<Mutex<Vec<Vec<TextDelta>>>>;

pub struct Draft {
    /// The note's ID, notebook and trash state. Its `body` is left empty;
    /// the text is in the document.
    note: Note,
    doc: LoroDoc,
    /// Undoes only this device's edits, even after others' edits arrive.
    undo: UndoManager,
    /// Text changes from imports and undo, waiting to reach the UI.
    incoming: Incoming,
    _subscription: loro::Subscription,
    units: Units,
    /// The version storage has confirmed saving, and the versions of saves
    /// still in flight, by sequence.
    acked: VersionVector,
    in_flight: Vec<(u64, VersionVector)>,
    sequence: u64,
    saved: u64,
    queued: u64,
    changed: Instant,
    last_queued: Instant,
    last_opened: Instant,
    created: bool,
}

impl Draft {
    /// A new note passes an empty `snapshot`.
    fn open(mut note: Note, snapshot: &[u8], created: bool, peer: u64, units: Units) -> Self {
        let doc = LoroDoc::from_snapshot(snapshot).unwrap_or_else(|_| LoroDoc::new());
        // Set before any edit and before the undo manager binds to the peer.
        let _ = doc.set_peer_id(peer);
        let mut undo = UndoManager::new(&doc);
        undo.set_max_undo_steps(UNDO_STEPS);
        undo.set_merge_interval(UNDO_MERGE_MS);
        let incoming = Incoming::default();
        let queue = incoming.clone();
        let subscription = doc.subscribe(
            &crdt::text(&doc).id(),
            Arc::new(move |event| {
                // The UI already shows what was typed into it.
                if event.origin == TYPING {
                    return;
                }
                let mut queue = queue.lock().unwrap();
                for diff in event.events {
                    if let loro::event::Diff::Text(delta) = diff.diff {
                        queue.push(delta);
                    }
                }
            }),
        );
        note.body.clear();
        let now = Instant::now();
        Self {
            note,
            acked: doc.oplog_vv(),
            in_flight: vec![],
            doc,
            undo,
            incoming,
            _subscription: subscription,
            units,
            sequence: 0,
            saved: 0,
            queued: 0,
            changed: now,
            last_queued: now,
            last_opened: now,
            created,
        }
    }

    pub fn note(&self) -> &Note {
        &self.note
    }

    pub fn note_mut(&mut self) -> &mut Note {
        &mut self.note
    }

    pub fn text(&self) -> String {
        crdt::text(&self.doc).to_string()
    }

    /// Whether storage has created the note. A new note's draft exists first.
    pub fn is_created(&self) -> bool {
        self.created
    }

    /// Edits not yet handed to storage.
    pub fn has_unsent(&self) -> bool {
        self.sequence > self.queued
    }

    /// Edits storage hasn't confirmed saving.
    pub fn has_unsaved(&self) -> bool {
        self.sequence > self.saved
    }

    /// Records that the UI showed this draft, so it is among the last to be
    /// closed.
    pub fn touch(&mut self) {
        self.last_opened = Instant::now();
    }

    // Typing: the UI's text already shows these changes.

    pub fn insert(&mut self, at: usize, text: &str) -> Result<(), OutOfSync> {
        let at = self.char_position(at)?;
        if !text.is_empty() {
            crdt::text(&self.doc)
                .insert(at, text)
                .map_err(|_| OutOfSync)?;
            self.typed();
        }
        Ok(())
    }

    pub fn delete(&mut self, at: usize, len: usize) -> Result<(), OutOfSync> {
        let (start, end) = self.char_range(at, len)?;
        if end > start {
            crdt::text(&self.doc)
                .delete(start, end - start)
                .map_err(|_| OutOfSync)?;
            self.typed();
        }
        Ok(())
    }

    /// Replaces `len` units at `at`, recording only the part that actually
    /// differs. Android keyboards rewrite the whole word they're composing on
    /// every keystroke; trimming the common prefix and suffix keeps edits
    /// small and merges with other devices clean.
    pub fn replace(&mut self, at: usize, len: usize, text: &str) -> Result<(), OutOfSync> {
        let (start, end) = self.char_range(at, len)?;
        let body = crdt::text(&self.doc);
        let old: Vec<char> = if end == start {
            vec![]
        } else {
            body.slice(start, end)
                .map_err(|_| OutOfSync)?
                .chars()
                .collect()
        };
        let new: Vec<char> = text.chars().collect();
        let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
        let suffix = old[prefix..]
            .iter()
            .rev()
            .zip(new[prefix..].iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let removed = old.len() - prefix - suffix;
        let inserted: String = new[prefix..new.len() - suffix].iter().collect();
        if removed == 0 && inserted.is_empty() {
            return Ok(());
        }
        if removed > 0 {
            body.delete(start + prefix, removed)
                .map_err(|_| OutOfSync)?;
        }
        if !inserted.is_empty() {
            body.insert(start + prefix, &inserted)
                .map_err(|_| OutOfSync)?;
        }
        self.typed();
        Ok(())
    }

    /// The character position of `at`, which is in the draft's units.
    fn char_position(&self, at: usize) -> Result<usize, OutOfSync> {
        let body = crdt::text(&self.doc);
        match self.units {
            Units::Chars => (at <= body.len_unicode()).then_some(at),
            Units::Utf16 => body
                .convert_pos(at, PosType::Utf16, PosType::Unicode)
                // A position inside a surrogate pair splits a character.
                .filter(|&pos| body.convert_pos(pos, PosType::Unicode, PosType::Utf16) == Some(at)),
        }
        .ok_or(OutOfSync)
    }

    /// The character positions of `len` units at `at`.
    fn char_range(&self, at: usize, len: usize) -> Result<(usize, usize), OutOfSync> {
        let end = at.checked_add(len).ok_or(OutOfSync)?;
        Ok((self.char_position(at)?, self.char_position(end)?))
    }

    fn typed(&mut self) {
        self.doc.set_next_commit_origin(TYPING);
        self.doc.commit();
        self.sequence += 1;
        self.changed = Instant::now();
    }

    // Changes coming from the document: the UI must apply the returned edits.

    /// `None` if there was nothing to undo.
    pub fn undo(&mut self) -> Option<Applied> {
        self.undo_or_redo(true)
    }

    /// `None` if there was nothing to redo.
    pub fn redo(&mut self) -> Option<Applied> {
        self.undo_or_redo(false)
    }

    fn undo_or_redo(&mut self, undo: bool) -> Option<Applied> {
        let before = self.before_change();
        let done = if undo {
            self.undo.undo()
        } else {
            self.undo.redo()
        };
        if !done.unwrap_or(false) {
            return None;
        }
        // Undo can record edits that leave the text as it was; they still
        // need saving.
        self.sequence += 1;
        self.changed = Instant::now();
        Some(self.take_incoming(before))
    }

    /// Imports changes from storage, such as another device's edits. A delta
    /// that can't be imported changes nothing.
    pub fn import(&mut self, delta: &[u8]) -> Applied {
        let before = self.before_change();
        if self.doc.import(delta).is_err() {
            self.incoming.lock().unwrap().clear();
            return Applied::default();
        }
        self.take_incoming(before)
    }

    /// The text as it is before an import or undo. Measuring their changes
    /// in UTF-16 units needs it, because a deletion's characters are gone
    /// from the document afterwards.
    fn before_change(&self) -> Option<Vec<char>> {
        (self.units == Units::Utf16).then(|| self.text().chars().collect())
    }

    /// Turns the text changes queued by an import or undo into edits. Loro's
    /// deltas count characters; in UTF-16 they're measured against `before`.
    fn take_incoming(&self, before: Option<Vec<char>>) -> Applied {
        let deltas = std::mem::take(&mut *self.incoming.lock().unwrap());
        let mut text = before;
        let mut applied = Applied::default();
        for delta in deltas {
            // `pos` counts characters in `text`, and `at` the draft's units.
            let (mut pos, mut at) = (0, 0);
            for item in delta {
                match item {
                    TextDelta::Retain { retain, .. } => {
                        at += measure(text.as_deref(), pos, retain);
                        pos += retain;
                    }
                    TextDelta::Insert { insert, .. } => {
                        let len = self.units.count(&insert);
                        if let Some(text) = &mut text {
                            text.splice(pos..pos, insert.chars());
                        }
                        pos += insert.chars().count();
                        applied.edits.push(TextEdit::Insert { at, text: insert });
                        at += len;
                        applied.cursor = Some(at);
                    }
                    TextDelta::Delete { delete } => {
                        let len = measure(text.as_deref(), pos, delete);
                        if let Some(text) = &mut text {
                            text.drain(pos..pos + delete);
                        }
                        applied.edits.push(TextEdit::Delete { at, len });
                        applied.cursor = Some(at);
                    }
                }
            }
        }
        applied
    }

    fn is_due(&self, now: Instant) -> bool {
        self.has_unsent()
            && (now.saturating_duration_since(self.changed) >= SAVE_PAUSE
                || now.saturating_duration_since(self.last_queued) >= SAVE_INTERVAL)
    }

    /// Saves every edit the draft has made since storage last confirmed a save.
    fn save_command(&mut self, now: Instant) -> Command {
        let updates = self
            .doc
            .export(ExportMode::updates(&self.acked))
            .unwrap_or_default();
        self.in_flight.push((self.sequence, self.doc.oplog_vv()));
        self.queued = self.sequence;
        self.last_queued = now;
        Command::Edit {
            id: self.note.id.clone(),
            updates: vec![updates],
            sequence: self.sequence,
        }
    }
}

/// The width of `n` characters at `pos` in `text`, in UTF-16 units. Without
/// a `text`, positions count characters, so the width is `n`.
fn measure(text: Option<&[char]>, pos: usize, n: usize) -> usize {
    text.map_or(n, |text| {
        text[pos..pos + n].iter().map(|c| c.len_utf16()).sum()
    })
}

/// Every open draft, written with one writer identity.
pub struct Drafts {
    peer: u64,
    units: Units,
    drafts: HashMap<String, Draft>,
}

impl Drafts {
    pub fn new(units: Units) -> Self {
        Self {
            peer: crdt::random_peer(),
            units,
            drafts: HashMap::new(),
        }
    }

    /// This app instance's writer identity in every note it edits.
    pub fn peer(&self) -> u64 {
        self.peer
    }

    /// Opens a note from its snapshot, replacing any draft of it. A new note
    /// that storage hasn't created yet passes an empty `snapshot` and
    /// `created: false`.
    pub fn open(&mut self, note: Note, snapshot: &[u8], created: bool) -> &mut Draft {
        let id = note.id.clone();
        let draft = Draft::open(note, snapshot, created, self.peer, self.units);
        self.drafts.entry(id).insert_entry(draft).into_mut()
    }

    pub fn get(&self, id: &str) -> Option<&Draft> {
        self.drafts.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Draft> {
        self.drafts.get_mut(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Draft> {
        self.drafts.values()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Draft> {
        self.drafts.values_mut()
    }

    /// Edit commands for drafts whose save is due at `now`.
    pub fn due(&mut self, now: Instant) -> Vec<Command> {
        self.drafts
            .values_mut()
            .filter(|d| d.is_due(now))
            .map(|d| d.save_command(now))
            .collect()
    }

    /// How long until the next save falls due, so the UI can sleep until then.
    /// `None` when nothing is waiting to be saved.
    pub fn next_due(&self, now: Instant) -> Option<Duration> {
        self.drafts
            .values()
            .filter(|d| d.has_unsent())
            .map(|d| (d.changed + SAVE_PAUSE).min(d.last_queued + SAVE_INTERVAL))
            .min()
            .map(|due| due.saturating_duration_since(now))
    }

    /// Edit commands for every draft with unsent edits, whatever the timing.
    pub fn flush(&mut self) -> Vec<Command> {
        let now = Instant::now();
        self.drafts
            .values_mut()
            .filter(|d| d.has_unsent())
            .map(|d| d.save_command(now))
            .collect()
    }

    /// Storage created the note.
    pub fn created(&mut self, id: &str) {
        if let Some(d) = self.drafts.get_mut(id) {
            d.created = true;
        }
    }

    /// Storage saved the edit command with this `sequence`.
    pub fn saved(&mut self, id: &str, sequence: u64) {
        if let Some(d) = self.drafts.get_mut(id) {
            d.saved = d.saved.max(sequence);
            if let Some(done) = d.in_flight.iter().position(|(seq, _)| *seq == sequence) {
                d.acked = d.in_flight[done].1.clone();
                d.in_flight.drain(..=done);
            }
        }
    }

    /// A command that resends everything since the last confirmed save, which
    /// includes the failed edits and any made after them.
    pub fn resend(&mut self, id: &str) -> Option<Command> {
        let now = Instant::now();
        self.drafts.get_mut(id).map(|d| d.save_command(now))
    }

    /// Closes the least recently shown drafts beyond the cache limit, and
    /// returns their IDs. Never closes the `active` draft or one with edits
    /// storage hasn't confirmed.
    pub fn trim(&mut self, active: Option<&str>) -> Vec<String> {
        let excess = self.drafts.len().saturating_sub(CACHE_LIMIT);
        if excess == 0 {
            return vec![];
        }
        let mut candidates: Vec<_> = self
            .drafts
            .iter()
            .filter(|(id, d)| {
                active != Some(id.as_str())
                    && d.created
                    && d.sequence == d.saved
                    // Reopening reuses this writer identity, so every edit it
                    // made must already be saved.
                    && d.doc.oplog_vv().get(&self.peer) == d.acked.get(&self.peer)
            })
            .map(|(id, d)| (d.last_opened, id.clone()))
            .collect();
        candidates.sort();
        candidates
            .into_iter()
            .take(excess)
            .map(|(_, id)| {
                self.drafts.remove(&id);
                id
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Rng, storage::Repository};

    /// A repository holding one new note, and an open draft of it.
    fn setup(units: Units) -> (tempfile::TempDir, Repository, Drafts, String) {
        let dir = tempfile::tempdir().unwrap();
        let mut repo = Repository::open(&dir.path().join("notes.db")).unwrap();
        let note = Note::blank();
        repo.create_note(&note).unwrap();
        let snapshot = repo.snapshot(&note.id).unwrap().unwrap();
        let id = note.id.clone();
        let mut drafts = Drafts::new(units);
        drafts.open(note, &snapshot, true);
        (dir, repo, drafts, id)
    }

    /// Saves like the storage worker, and acknowledges like the UI.
    fn store(repo: &mut Repository, drafts: &mut Drafts, command: Command) {
        let Command::Edit {
            id,
            updates,
            sequence,
        } = command
        else {
            panic!("expected an edit: {command:?}");
        };
        repo.edit(&id, &updates).unwrap();
        drafts.saved(&id, sequence);
    }

    fn body(repo: &Repository, id: &str) -> String {
        repo.load(id).unwrap().unwrap().body
    }

    /// Applies edits the way a UI does, to a copy of the old text: as
    /// characters, like GTK, or as UTF-16 units, like a Java `Editable`.
    fn apply(text: &str, applied: &Applied, units: Units) -> String {
        fn edit<T>(mut buffer: Vec<T>, applied: &Applied, encode: fn(&str) -> Vec<T>) -> Vec<T> {
            for edit in &applied.edits {
                match edit {
                    TextEdit::Insert { at, text } => {
                        buffer.splice(*at..*at, encode(text));
                    }
                    TextEdit::Delete { at, len } => {
                        buffer.drain(*at..*at + len);
                    }
                }
            }
            buffer
        }
        match units {
            Units::Chars => edit(text.chars().collect(), applied, |s| s.chars().collect())
                .into_iter()
                .collect(),
            Units::Utf16 => {
                String::from_utf16(&edit(text.encode_utf16().collect(), applied, |s| {
                    s.encode_utf16().collect()
                }))
                .unwrap()
            }
        }
    }

    #[test]
    fn a_pause_in_typing_saves_the_text() {
        let (_dir, mut repo, mut drafts, id) = setup(Units::Chars);
        let d = drafts.get_mut(&id).unwrap();
        d.insert(0, "Hello 🌿").unwrap();
        d.insert(7, " world").unwrap();
        let typed = d.changed;
        assert_eq!(drafts.next_due(typed), Some(SAVE_PAUSE));
        assert!(
            drafts
                .due(typed + SAVE_PAUSE - Duration::from_millis(1))
                .is_empty()
        );
        let commands = drafts.due(typed + SAVE_PAUSE);
        assert_eq!(commands.len(), 1);
        for command in commands {
            store(&mut repo, &mut drafts, command);
        }
        assert_eq!(body(&repo, &id), "Hello 🌿 world");
        assert!(!drafts.get(&id).unwrap().has_unsaved());
        assert_eq!(drafts.next_due(typed), None);
        assert!(drafts.due(typed + SAVE_INTERVAL).is_empty());
    }

    #[test]
    fn continuous_typing_still_saves_every_two_seconds() {
        let (_dir, mut repo, mut drafts, id) = setup(Units::Chars);
        let start = drafts.get(&id).unwrap().last_queued;
        let mut saved_at = vec![];
        for step in 1..=20 {
            let now = start + Duration::from_millis(250) * step;
            let d = drafts.get_mut(&id).unwrap();
            let end = d.text().chars().count();
            d.insert(end, "a").unwrap();
            d.changed = now;
            for command in drafts.due(now) {
                saved_at.push(step);
                store(&mut repo, &mut drafts, command);
            }
        }
        assert_eq!(saved_at, [8, 16]);
        assert_eq!(body(&repo, &id), "a".repeat(16));
    }

    #[test]
    fn acknowledging_a_later_save_settles_earlier_ones() {
        let (_dir, mut repo, mut drafts, id) = setup(Units::Chars);
        drafts.get_mut(&id).unwrap().insert(0, "one").unwrap();
        let first = drafts.flush();
        drafts.get_mut(&id).unwrap().insert(3, " two").unwrap();
        let second = drafts.flush();
        assert_eq!(drafts.get(&id).unwrap().in_flight.len(), 2);
        // Storage saves both, but the first acknowledgement hasn't arrived.
        for command in first.into_iter().chain(second) {
            let Command::Edit { id, updates, .. } = command else {
                unreachable!()
            };
            repo.edit(&id, &updates).unwrap();
        }
        drafts.saved(&id, 2);
        let d = drafts.get(&id).unwrap();
        assert!(d.in_flight.is_empty());
        assert_eq!(d.acked, d.doc.oplog_vv());
        assert!(!d.has_unsaved());
        assert_eq!(body(&repo, &id), "one two");
        // A late acknowledgement of the first changes nothing.
        let acked = d.acked.clone();
        drafts.saved(&id, 1);
        assert_eq!(drafts.get(&id).unwrap().acked, acked);
    }

    #[test]
    fn resending_after_a_failure_includes_later_edits() {
        let (_dir, mut repo, mut drafts, id) = setup(Units::Chars);
        drafts.get_mut(&id).unwrap().insert(0, "lost").unwrap();
        // Storage reports this save as failed.
        assert_eq!(drafts.flush().len(), 1);
        drafts
            .get_mut(&id)
            .unwrap()
            .insert(4, " and found")
            .unwrap();
        let command = drafts.resend(&id).unwrap();
        store(&mut repo, &mut drafts, command);
        assert_eq!(body(&repo, &id), "lost and found");
        let d = drafts.get(&id).unwrap();
        assert!(!d.has_unsent() && !d.has_unsaved());
        assert!(d.in_flight.is_empty());
    }

    #[test]
    fn imports_and_undos_return_edits_that_rebuild_the_text() {
        let pieces = ["a", "xyz", "世界", "🌿", "é", "e\u{301}", "👩‍💻", "\n", "  "];
        for units in [Units::Chars, Units::Utf16] {
            for seed in 1..=20u64 {
                let mut rng = Rng(seed);
                let other = LoroDoc::new();
                other.set_peer_id(1).unwrap();
                crdt::text(&other).insert(0, "Start 🌿 text").unwrap();
                other.commit();
                let mut drafts = Drafts::new(units);
                let note = Note::blank();
                let id = note.id.clone();
                drafts.open(note, &other.export(ExportMode::Snapshot).unwrap(), true);
                let mut sent = other.oplog_vv();
                for step in 0..40 {
                    let context = format!("{units:?}, seed {seed}, step {step}");
                    let d = drafts.get_mut(&id).unwrap();
                    // Edits on both sides, so imports have to merge.
                    let text = crdt::text(&other);
                    let len = text.len_unicode();
                    let at = rng.below(len + 1);
                    if at < len && rng.below(3) == 0 {
                        text.delete(at, 1 + rng.below((len - at).min(4))).unwrap();
                    } else {
                        text.insert(at, pieces[rng.below(pieces.len())]).unwrap();
                    }
                    other.commit();
                    // Typing, at positions in the draft's units.
                    let body = crdt::text(&d.doc);
                    let unit = |pos| match units {
                        Units::Chars => pos,
                        Units::Utf16 => body
                            .convert_pos(pos, PosType::Unicode, PosType::Utf16)
                            .unwrap(),
                    };
                    let len = body.len_unicode();
                    let start = rng.below(len + 1);
                    let end = start + rng.below((len - start).min(4) + 1);
                    let (at, n) = (unit(start), unit(end) - unit(start));
                    let piece = pieces[rng.below(pieces.len())];
                    match rng.below(3) {
                        0 => d.insert(at, piece),
                        1 => d.delete(at, n),
                        _ => d.replace(at, n, piece),
                    }
                    .unwrap();
                    let before = d.text();
                    let delta = other.export(ExportMode::updates(&sent)).unwrap();
                    sent = other.oplog_vv();
                    let applied = d.import(&delta);
                    assert_eq!(apply(&before, &applied, units), d.text(), "{context}");
                    if rng.below(4) == 0 {
                        let before = d.text();
                        let undone = if rng.below(2) == 0 {
                            d.undo()
                        } else {
                            d.redo()
                        };
                        if let Some(applied) = undone {
                            assert_eq!(apply(&before, &applied, units), d.text(), "{context}");
                        }
                    }
                    // Sometimes the other side catches up with this one.
                    if rng.below(3) == 0 {
                        other
                            .import(
                                &d.doc
                                    .export(ExportMode::updates(&other.oplog_vv()))
                                    .unwrap(),
                            )
                            .unwrap();
                        sent = other.oplog_vv();
                    }
                }
            }
        }
    }

    #[test]
    fn undo_after_a_remote_edit_reverts_only_local_typing() {
        let (_dir, mut repo, mut drafts, id) = setup(Units::Chars);
        drafts.get_mut(&id).unwrap().insert(0, "Hello").unwrap();
        for command in drafts.flush() {
            store(&mut repo, &mut drafts, command);
        }
        // Storage's record of the save comes back, with no text the draft lacks.
        for change in repo.take_changes() {
            let d = drafts.get_mut(&id).unwrap();
            assert_eq!(d.import(&change.delta), Applied::default());
        }
        // Another device's edit reaches the draft through storage.
        repo.save(&id, "> From the laptop\nHello").unwrap();
        let delta = repo.take_changes().pop().unwrap().delta;
        let d = drafts.get_mut(&id).unwrap();
        let applied = d.import(&delta);
        assert_eq!(
            apply("Hello", &applied, Units::Chars),
            "> From the laptop\nHello"
        );
        assert_eq!(d.text(), "> From the laptop\nHello");
        let prefix = "> From the laptop\n".chars().count();
        let sequence = d.sequence;
        let undone = d.undo().unwrap();
        assert_eq!(d.text(), "> From the laptop\n");
        assert_eq!(undone.cursor, Some(prefix));
        assert_eq!(d.sequence, sequence + 1);
        let redone = d.redo().unwrap();
        assert_eq!(d.text(), "> From the laptop\nHello");
        assert_eq!(redone.cursor, Some(prefix + 5));
        assert!(d.undo().is_some());
        assert!(d.undo().is_none());
    }

    #[test]
    fn replacing_a_composing_word_records_only_the_change() {
        let (_dir, _repo, mut drafts, id) = setup(Units::Chars);
        let peer = drafts.peer();
        let ops = |d: &Draft| d.doc.oplog_vv().get(&peer).copied().unwrap_or(0);
        let d = drafts.get_mut(&id).unwrap();
        d.insert(0, "say hel").unwrap();
        let (before, sequence) = (ops(d), d.sequence);
        d.replace(4, 3, "hell").unwrap();
        assert_eq!(d.text(), "say hell");
        assert_eq!(ops(d) - before, 1);
        assert_eq!(d.sequence, sequence + 1);
        // Rewriting the same word records nothing.
        d.replace(4, 4, "hell").unwrap();
        assert_eq!(ops(d) - before, 1);
        assert_eq!(d.sequence, sequence + 1);
        // One changed letter is one deletion and one insertion.
        d.replace(4, 4, "help").unwrap();
        assert_eq!(d.text(), "say help");
        assert_eq!(ops(d) - before, 3);
        d.replace(0, 8, "").unwrap();
        assert_eq!(d.text(), "");
    }

    #[test]
    fn positions_past_the_end_change_nothing() {
        let (_dir, _repo, mut drafts, id) = setup(Units::Chars);
        let d = drafts.get_mut(&id).unwrap();
        d.insert(0, "🌿 ok").unwrap();
        let sequence = d.sequence;
        assert_eq!(d.insert(5, "x"), Err(OutOfSync));
        assert_eq!(d.delete(3, 2), Err(OutOfSync));
        assert_eq!(d.delete(usize::MAX, 2), Err(OutOfSync));
        assert_eq!(d.replace(4, 1, "x"), Err(OutOfSync));
        assert_eq!(d.text(), "🌿 ok");
        assert_eq!(d.sequence, sequence);
        d.insert(4, "!").unwrap();
        assert_eq!(d.text(), "🌿 ok!");
    }

    #[test]
    fn trimming_keeps_the_active_draft_and_unsaved_edits() {
        let mut drafts = Drafts::new(Units::Chars);
        let start = Instant::now();
        let mut ids = vec![];
        for i in 0..CACHE_LIMIT + 3 {
            let note = Note::blank();
            ids.push(note.id.clone());
            drafts.open(note, &[], true).last_opened = start + Duration::from_secs(i as u64);
        }
        // Saved edits don't keep a draft open.
        drafts.get_mut(&ids[3]).unwrap().insert(0, "saved").unwrap();
        for command in drafts.flush() {
            let Command::Edit { id, sequence, .. } = command else {
                unreachable!()
            };
            drafts.saved(&id, sequence);
        }
        drafts
            .get_mut(&ids[1])
            .unwrap()
            .insert(0, "unsaved")
            .unwrap();
        drafts.get_mut(&ids[2]).unwrap().created = false;
        let closed = drafts.trim(Some(&ids[0]));
        assert_eq!(closed, ids[3..6]);
        assert_eq!(drafts.iter().count(), CACHE_LIMIT);
        for id in &ids[..3] {
            assert!(drafts.get(id).is_some());
        }
        assert!(drafts.trim(Some(&ids[0])).is_empty());
    }

    #[test]
    fn utf16_typing_and_imports_count_emoji_as_two_units() {
        let (_dir, mut repo, mut drafts, id) = setup(Units::Utf16);
        let d = drafts.get_mut(&id).unwrap();
        d.insert(0, "🌿 tea").unwrap();
        d.insert(2, "!").unwrap();
        assert_eq!(d.text(), "🌿! tea");
        // Inside the emoji's surrogate pair is no place for an edit.
        assert_eq!(d.insert(1, "x"), Err(OutOfSync));
        assert_eq!(d.delete(1, 2), Err(OutOfSync));
        assert_eq!(d.replace(0, 1, "x"), Err(OutOfSync));
        assert_eq!(d.insert(8, "x"), Err(OutOfSync));
        d.delete(0, 3).unwrap();
        assert_eq!(d.text(), " tea");
        d.insert(0, "🌿🌿").unwrap();
        d.replace(2, 2, "🌱").unwrap();
        assert_eq!(d.text(), "🌿🌱 tea");
        for command in drafts.flush() {
            store(&mut repo, &mut drafts, command);
        }
        for change in repo.take_changes() {
            let d = drafts.get_mut(&id).unwrap();
            assert_eq!(d.import(&change.delta), Applied::default());
        }
        // Another device removes the first emoji and adds to the end.
        repo.save(&id, "🌱 tea, hot").unwrap();
        let delta = repo.take_changes().pop().unwrap().delta;
        let d = drafts.get_mut(&id).unwrap();
        let applied = d.import(&delta);
        assert_eq!(
            applied.edits,
            [
                TextEdit::Delete { at: 0, len: 2 },
                TextEdit::Insert {
                    at: 6,
                    text: ", hot".into()
                },
            ]
        );
        assert_eq!(apply("🌿🌱 tea", &applied, Units::Utf16), "🌱 tea, hot");
        assert_eq!(d.text(), "🌱 tea, hot");
    }

    #[test]
    fn utf16_undo_places_the_cursor_after_emoji() {
        let (_dir, _repo, mut drafts, id) = setup(Units::Utf16);
        let d = drafts.get_mut(&id).unwrap();
        // Each edit undoes on its own.
        d.undo.set_merge_interval(0);
        d.insert(0, "🌿🌿 ").unwrap();
        d.insert(5, "tea").unwrap();
        let undone = d.undo().unwrap();
        assert_eq!(d.text(), "🌿🌿 ");
        assert_eq!(undone.edits, [TextEdit::Delete { at: 5, len: 3 }]);
        assert_eq!(undone.cursor, Some(5));
        let redone = d.redo().unwrap();
        assert_eq!(d.text(), "🌿🌿 tea");
        assert_eq!(redone.cursor, Some(8));
    }
}
