# 3. Move the editor session into core

**Status:** Done
**Needs:** step 2
**Behaviour change:** none on desktop

## Why

The GTK UI owns a copy of each open note as a CRDT document, and the code around that copy decides how notes merge and save. If Android had its own copy of this logic, the two apps would slowly diverge in subtle ways, such as undo reverting another device's edit or a retry losing keystrokes. This step moves the logic into `notebook-core` as a UI-free `editor` module, and switches the GTK UI to use it.

## What moves

Line numbers are from `crates/gtk/src/ui.rs` as of step 2.

| Behaviour | Now | Moves to |
| --- | --- | --- |
| The editor's `LoroDoc` built from a snapshot, with the window's peer ID set before any edit | `install_draft`, line 1027 | `Draft::open` |
| `UndoManager` with 500 steps and a 500 ms merge interval, undoing only this device's edits | `install_draft` | `Draft` |
| A subscription that queues text deltas whose origin isn't `"typing"` | `install_draft` | `Draft` |
| Typing written into the document with origin `"typing"`, then `sequence += 1` | `mirror_typing`, line 1256 | `Draft::insert`, `delete`, `replace` |
| Queued deltas turned into buffer edits, returning the cursor position | `apply_incoming`, line 1299 | the `Applied` value returned by `undo`, `redo` and `import` |
| Undo and redo; an undo that leaves the text unchanged still counts as an edit | `undo_redo`, line 1336 | `Draft::undo`, `redo` |
| Importing a `NoteDelta` from storage | `handle` → `Event::NoteDelta`, line 1835 | `Draft::import` |
| Save timing: after a 300 ms pause, and at least every 2 s while typing | `tick`, line 1549 | `Drafts::due` |
| An `Edit` command holding every change since the last acknowledged version, with in-flight versions tracked | `edit_command`, line 2397 | `Draft::save_command` |
| Saving every draft before switching notes or closing | `flush_drafts`, line 1245 | `Drafts::flush` |
| `Saved` moving `acked` forward and dropping finished in-flight saves | `handle` → `Event::Saved` | `Drafts::saved` |
| Resending everything since `acked` after a failure | the retry handler, line 786 | `Drafts::resend` |
| Keeping at most 12 drafts, and evicting only those with every own edit acknowledged | `trim_cache`, line 1167 | `Drafts::trim` |
| One writer identity per app instance, from `crdt::random_peer()` | `Ui::peer` | `Drafts::peer` |

What stays in the UI: the text widget, the guard flag that stops the UI's own edits from echoing back (`from_doc`), Markdown parsing and tags, the note list, and error display.

## New module: `crates/core/src/editor.rs`

Here is a sketch of the API. Positions are in characters in this step; step 4 adds UTF-16.

```rust
/// A change the UI applies to its text widget, in order. Each position
/// refers to the text after the previous edit.
pub enum TextEdit {
    Insert { at: usize, text: String },
    Delete { at: usize, len: usize },
}

pub struct Applied {
    pub edits: Vec<TextEdit>,
    /// Where to place the cursor after an undo or redo: just after the last edit.
    pub cursor: Option<usize>,
}

/// A position outside the draft's text.
pub struct OutOfSync;

pub struct Draft { /* note, doc, undo, incoming, subscription, acked,
                     in_flight, sequence, saved, queued, changed,
                     last_queued, last_opened, created */ }

impl Draft {
    pub fn note(&self) -> &Note;            // id, notebook_id, deleted; body is left empty
    pub fn note_mut(&mut self) -> &mut Note;
    pub fn text(&self) -> String;
    pub fn is_created(&self) -> bool;
    pub fn touch(&mut self);                 // the UI showed it; trim closes it last

    // Typing: the UI's text already shows these changes. A position past the
    // end returns OutOfSync and changes nothing.
    pub fn insert(&mut self, at: usize, text: &str) -> Result<(), OutOfSync>;
    pub fn delete(&mut self, at: usize, len: usize) -> Result<(), OutOfSync>;
    /// Replaces `len` characters at `at`, recording only the part that
    /// actually differs. Android keyboards rewrite the whole word they're
    /// composing on every keystroke; trimming the common prefix and suffix
    /// keeps edits small and merges with other devices clean.
    pub fn replace(&mut self, at: usize, len: usize, text: &str) -> Result<(), OutOfSync>;

    // Changes coming from the document: the UI must apply `edits`.
    pub fn undo(&mut self) -> Option<Applied>;   // None if nothing to undo
    pub fn redo(&mut self) -> Option<Applied>;
    pub fn import(&mut self, delta: &[u8]) -> Applied;

    pub fn has_unsent(&self) -> bool;          // sequence > queued
    pub fn has_unsaved(&self) -> bool;         // sequence > saved
}

pub struct Drafts { peer: u64, drafts: HashMap<String, Draft> }

impl Drafts {
    pub fn new() -> Self;                                   // random peer
    pub fn open(&mut self, note: Note, snapshot: &[u8], created: bool) -> &mut Draft;
    pub fn get(&self, id: &str) -> Option<&Draft>;
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Draft>;
    pub fn iter(&self) -> impl Iterator<Item = &Draft>;
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Draft>;

    /// Edit commands for drafts whose save is due at `now`.
    pub fn due(&mut self, now: Instant) -> Vec<Command>;
    /// How long until the next save falls due, so the UI can sleep until then.
    pub fn next_due(&self, now: Instant) -> Option<Duration>;
    /// Edit commands for every draft with unsent edits, whatever the timing.
    pub fn flush(&mut self) -> Vec<Command>;

    pub fn created(&mut self, id: &str);
    pub fn saved(&mut self, id: &str, sequence: u64);
    /// A command that resends everything since the last confirmed save.
    pub fn resend(&mut self, id: &str) -> Option<Command>;
    /// Returns the IDs it closed, so the UI can drop their text widgets.
    pub fn trim(&mut self, active: Option<&str>) -> Vec<String>;
}
```

Notes on the API:

- `Drafts` returns `Command` values instead of sending them itself. That keeps it free of channels and easy to test, and lets GTK keep its `send` wrapper with its error handling.
- The subscription callback runs synchronously inside `commit`, `import` and `undo`, just as `apply_incoming` relies on today. So `undo`, `redo` and `import` can drain the queue and return the edits in the same call.
- The constants `UNDO_MERGE_MS`, `TYPING`, the 300 ms pause, the 2 s maximum and the 12-draft cache limit move with the code.
- `next_due` exists for Android, where the app should sleep instead of polling (see step 6). GTK can keep its 16 ms tick for now.

## GTK migration

- `State` holds `drafts: Drafts` instead of `HashMap<String, Draft>`, plus `pages: HashMap<String, Page>` for each draft's buffer and its text as of the last change (used for the word count, the placeholder and parsing).
- `mirror_typing` calls `draft.insert` or `draft.delete`.
- `apply_incoming(buffer, applied)` applies each `TextEdit` inside the `from_doc` guard, then calls `text_changed`.
- `tick` sends `drafts.due(now)`. `flush_drafts` sends `drafts.flush()`.
- `Event::Saved`, `Event::Created`, the retry button and `trim_cache` call the matching `Drafts` methods.
- `ui.rs` should no longer import `loro` at all. That's a quick check that nothing was missed.

## Tests (in `crates/core/src/editor.rs`)

Use a real `Repository` in a temp directory, as `crates/core/tests/storage.rs` does. The tests are unit tests, so the timing tests can set a draft's private timestamps instead of sleeping.

1. Type, then call `due` after a 300 ms pause. `Repository::edit` with the resulting command stores the same text.
2. Type continuously with gaps under 300 ms. A save still falls due every 2 s.
3. Two saves in flight: acknowledging the second moves `acked` forward and drops both.
4. A failed save followed by `resend` includes both the failed edits and those made after them.
5. `import` of a delta from another document returns edits that, applied to a `String` copy of the old text, give `draft.text()`. Run this with random edits as a property test.
6. After a remote import, `undo` reverts only local edits and returns the cursor after the last edit.
7. `replace` of a composing word ("hel" → "hell") records a single one-character insert.
8. `trim` never evicts a draft with unacknowledged local edits, or the active draft.

The GTK `desktop_workflow` test must still pass. It covers editing beside another device and navigating after deletion.

## Notes

- **The API grew a little.** Typing returns `Result<(), OutOfSync>`, which step 6 maps to `CoreError::OutOfSync`. `trim` returns the closed IDs, and `touch`, `is_created`, `iter` and `iter_mut` replace direct field access. GTK still ignores typing errors, as it did before.
- **`Draft::note().body` is always empty.** The text lives in the document, and a cached copy would go stale. GTK keeps its own copy in `Page`, read from the buffer exactly as before.
- **Every `NoteDelta` must be imported, including storage's echo of the draft's own saves.**
  - Each save makes storage add a metadata change (`updated_at`) of its own. A later delta depends on that change.
  - If a draft skips an echo, Loro holds the later imports as pending: no error, and no edits. The first version of the remote-edit test did exactly this.
  - Kotlin must import every `NoteDelta` for an open or cached note, in order (step 8). An echo returns no edits.
- **GTK tests:** `desktop_workflow`'s assertions are unchanged. Only its reads of draft internals now go through accessors: `saved == sequence` became `!has_unsaved()`, and `UNDO_MERGE_MS` comes from `notebook_core::editor`. The other two tests named in the original plan never existed as separate tests; `desktop_workflow` covers both scenarios.
- **`loro`** is now a dependency of `notebook-core` only. `ui.rs` no longer imports it, or `crdt` outside its tests.
- **Results:** `ui.rs` lost about 140 lines. `cargo test` passes with 9 new editor tests (42 passed, 4 ignored), and `desktop_workflow` passes under Xvfb. The core still builds for arm64 Android.
