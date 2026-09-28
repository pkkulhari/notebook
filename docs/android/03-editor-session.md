# 3. Move the editor session into core

**Status:** Not started
**Needs:** step 2
**Behaviour change:** none on desktop

## Why

The GTK UI owns a copy of each open note as a CRDT document, and the code around that copy decides how notes merge and save. If Android had its own copy of this logic, the two apps would slowly diverge in subtle ways, such as undo reverting another device's edit or a retry losing keystrokes. This step moves the logic into `notebook-core` as a UI-free `editor` module, and switches the GTK UI to use it.

## What moves

Line numbers are from `src/ui.rs` before step 2.

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

pub struct Draft { /* note, doc, undo, incoming, subscription, acked,
                     in_flight, sequence, saved, queued, changed,
                     last_queued, last_opened, created */ }

impl Draft {
    pub fn note(&self) -> &Note;            // id, notebook_id, deleted
    pub fn note_mut(&mut self) -> &mut Note;
    pub fn text(&self) -> String;

    // Typing: the UI's text already shows these changes.
    pub fn insert(&mut self, at: usize, text: &str);
    pub fn delete(&mut self, at: usize, len: usize);
    /// Replaces `len` characters at `at`, recording only the part that
    /// actually differs. Android keyboards rewrite the whole word they're
    /// composing on every keystroke; trimming the common prefix and suffix
    /// keeps edits small and merges with other devices clean.
    pub fn replace(&mut self, at: usize, len: usize, text: &str);

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
    pub fn trim(&mut self, active: Option<&str>);
}
```

Notes on the API:

- `Drafts` returns `Command` values instead of sending them itself. That keeps it free of channels and easy to test, and lets GTK keep its `send` wrapper with its error handling.
- The subscription callback runs synchronously inside `commit`, `import` and `undo`, just as `apply_incoming` relies on today. So `undo`, `redo` and `import` can drain the queue and return the edits in the same call.
- The constants `UNDO_MERGE_MS`, `TYPING`, the 300 ms pause, the 2 s maximum and the 12-draft cache limit move with the code.
- `next_due` exists for Android, where the app should sleep instead of polling (see step 6). GTK can keep its 16 ms tick for now.

## GTK migration

- `Ui` holds `drafts: RefCell<Drafts>` instead of `State::drafts: HashMap<String, Draft>`. It keeps a separate `HashMap<String, gtk::TextBuffer>` for the buffers.
- `mirror_typing` calls `draft.insert` or `draft.delete`.
- `apply_incoming(buffer, applied)` applies each `TextEdit` inside the `from_doc` guard, then calls `text_changed`.
- `tick` sends `drafts.due(now)`. `flush_drafts` sends `drafts.flush()`.
- `Event::Saved`, `Event::Created`, the retry button and `trim_cache` call the matching `Drafts` methods.
- `ui.rs` should no longer import `loro` at all. That's a quick check that nothing was missed.

## Tests (in `crates/core`)

Use a real `Repository` in a temp directory, as `tests/storage.rs` does.

1. Type, then call `due` after a 300 ms pause. `Repository::edit` with the resulting command stores the same text.
2. Type continuously with gaps under 300 ms. A save still falls due every 2 s.
3. Two saves in flight: acknowledging the second moves `acked` forward and drops both.
4. A failed save followed by `resend` includes both the failed edits and those made after them.
5. `import` of a delta from another document returns edits that, applied to a `String` copy of the old text, give `draft.text()`. Run this with random edits as a property test.
6. After a remote import, `undo` reverts only local edits and returns the cursor after the last edit.
7. `replace` of a composing word ("hel" → "hell") records a single one-character insert.
8. `trim` never evicts a draft with unacknowledged local edits, or the active draft.

The existing GTK tests (`desktop_workflow`, `sync_with_another_device` and `deletion_navigation`) must still pass unchanged.

## Notes

_Anything surprising goes here._
