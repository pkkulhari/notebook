# 8. App shell

**Status:** Done (tested on an x86_64 emulator; the arm64 phone wasn't connected)
**Needs:** step 7

## Goal

A working Android app without Markdown styling or sync. You can browse notebooks, list and search notes, create, move, trash and restore notes, and edit plain text. Everything is saved through the core. Step 9 adds Markdown to the editor, and step 10 adds sync.

## Dependencies

Use framework Views and as few AndroidX libraries as possible:

- `androidx.recyclerview` for the note list, with `ListAdapter` and `DiffUtil`.
- Nothing else by default. Add a library only when the framework version is clearly worse, and write down why in Notes. `androidx.core` is a common case, for inset helpers on older APIs if `minSdk` is lowered.
- No Fragments, Navigation, ViewModel, LiveData, coroutines or DI. There's one Activity, and state lives in one plain class.

**Theme:** start from a platform theme such as `@android:style/Theme.DeviceDefault.DayNight`, with dynamic colours from `android.R.color.system_*` on API 31+. Match the desktop's quiet look: plain surfaces, a bold title, and no accent-heavy chrome.

## Structure

```
app/src/main/java/com/pkkulhari/notebook/
  NotebookApp.kt        Application: AndroidContext.install, starts Core once per process
  MainActivity.kt       the only Activity; shows the list screen or the editor screen
  Store.kt              main-thread copy of app state, updated from CoreEvent
  NoteListAdapter.kt    RecyclerView adapter for NoteSummary rows
  NotebookPicker.kt     choosing, creating, renaming and deleting notebooks
  EditorController.kt   ties an EditText to the core's draft (plain text here; Markdown in step 9)
  Dates.kt              list dates, matching note_date() in the desktop app
```

`Store` mirrors the desktop's `State` (`crates/gtk/src/ui.rs:51`):

- `defaultNotebookId`, `notebooks`, `notes` and `counts`
- `filter` and `query`
- `active`, the ID of the open note
- `listGeneration` and `loadGeneration`: stale `Listed` and `Loaded` events are ignored the same way
- `selectFirst`, used when the open note is trashed
- `failed`, plus the error message shown to the user

## Screens

On a phone, one screen is visible at a time. Leave room for a two-pane layout on tablets later, but don't build it yet.

**Note list**
- Title: the notebook name, "All notes", "Trash" or "Search results".
- Search field: searches across all notebooks, or across trashed notes in Trash. Send a query 150 ms after the last keystroke.
- Rows show the label, preview and date. The empty state says "No notes here", or "No matching notes" while searching.
- A **New note** button creates a note in Default, as on the desktop, and opens it.

**Notebooks**
- Tapping the title opens the list: Default, each notebook with its count, All notes and Trash, plus **New notebook**.
- Rename and delete are offered for notebooks other than Default.
- Name errors, such as "A notebook with that name already exists", come back as `CoreEvent.Error` and are shown inline.

**Editor**
- Full-screen `EditText` with the hint "Start writing".
- Top bar: back, the notebook the note is in (tap to move it), undo, redo, and trash or restore.
- Trashed notes open read-only, with a **Restore** action.

Back navigation goes from the editor to the list. Use `OnBackInvokedCallback` (API 33+) so predictive back works.

**Edge-to-edge:** Android enforces edge-to-edge for apps targeting API 35 and later. Every screen must apply window insets, and the editor must also apply IME insets so the cursor line stays above the keyboard.

## Event handling

These mirror `Ui::handle` in `crates/gtk/src/ui.rs:1612`.

| Event | What happens |
| --- | --- |
| `Ready` | Store notebooks. Open the draft with `core.openDraft`. Show the list, or the editor if the last session ended in the editor. Request the list. |
| `Listed` | Ignore it if its generation is stale. Otherwise update rows (`DiffUtil`) and counts. If `selectFirst` is set, open the neighbour of the trashed note. |
| `Loaded` | Ignore it if stale. Otherwise call `core.openDraft(note, snapshot)` and show the editor with the returned text. |
| `Created` / `Saved` | Refresh the list, so labels, previews and ordering follow the text. |
| `Mutated` | Update notebooks and the open note's notebook or trashed state. Leave a notebook view whose notebook was deleted. |
| `NoteDelta` | If the note is open or cached: `core.importDelta` on the main thread, and apply the edits (step 9). |
| `Remote` | Update notebooks and note states. If the open note was trashed elsewhere, move to its neighbour. Refresh the list. |
| `Error` | Show the message with **Retry** (`core.retry()`) when it can be retried, or **Dismiss** otherwise. Never discard the draft. |

## Plain-text editor bridge

Step 9 builds on this.

- A `TextWatcher` sends `core.replace(id, start, before, inserted)` from `onTextChanged`. It skips changes made while the `fromDoc` flag is set.
- Edits returned by `undo`, `redo` and `importDelta` are applied to the `Editable` with `fromDoc = true`.
- After each edit, call `core.tick()` and schedule the next call with `Handler.postDelayed` using the delay it returns. When it returns -1 there is nothing to save, so no timer runs.

## Lifecycle

| Moment | Action |
| --- | --- |
| `Application.onCreate` | `AndroidContext.install(this)`, then `Core.start(config, listener)` with paths from `filesDir` and `noBackupFilesDir`, then `initialize()`. The core lives as long as the process. |
| `Activity.onPause` | `core.flush()` sends every unsaved edit, then a `Flush`. Save preferences: the selected note and the cursor. |
| `Activity.onStop` | Nothing more here; step 10 suspends sync. |
| Process death | Anything storage acknowledged is on disk (`PRAGMA synchronous=FULL`). `onPause` hands the last keystrokes to the storage thread, which writes them within milliseconds. |
| Configuration changes | Handle rotation, dark mode and keyboard changes in `onConfigurationChanged` (`android:configChanges`), so the editor keeps its text, spans, scroll and IME state instead of being rebuilt. Refresh colours on a `uiMode` change. |

## Tests (instrumented)

1. Create a note, type, background the app, kill the process, reopen: the text is there.
2. Search finds a note by word prefix. Search in Trash finds only trashed notes.
3. Trash the open note: its neighbour opens. Restore it from Trash.
4. Create, rename and delete a notebook. Its notes move to Default.
5. Rotate the screen and switch dark mode while editing: no text, cursor or undo history is lost.

## Notes

- **Running instrumented tests uninstalls the app afterwards**, notes included. Run them on an emulator, or on a phone with no notes you care about. The tests themselves use `TestRunner`, which starts `TestApp`, an application that keeps its database in a fresh cache directory.
- **No AndroidX at all.** The note list is a framework `ListView` with a small `BaseAdapter`, not RecyclerView: the list is one simple row type, and that keeps JNA as the only runtime dependency.
- **Trashing the open note returns to the list**, rather than opening its neighbour as the desktop does. The same happens when another device trashes it. On a phone, an unexpected different note in the editor is more confusing than the list.
- **Startup:** `Ready` opens the editor when the last session ended there (`selected_note`). It also opens when the starting note is empty, so a first start goes straight to writing, as on the desktop. Otherwise the list shows.
- **Dark mode recreates the activity.** `uiMode` isn't in `configChanges`, because the platform theme's colours only refresh on recreation. Text, cursor and undo history survive, because `Store` belongs to the `Application` and the history lives in the core. Rotation and keyboard changes don't recreate it. `MainActivityTest` covers both.
- **Colours come from the desktop, not the wallpaper.** In place of dynamic colours, the theme uses the desktop's accent, Yaru orange `#E95420`, with neutral grey surfaces like the desktop's. It fills the New note button, the switch, the caret and selection, and the selected notebook. Text in the accent needs more contrast, so it uses a deeper orange in light mode and the logo's lighter orange in dark mode. The platform's alert dialog theme sets its own colours, so `Theme.Notebook.Dialog` sets them again. The colours are in `res/values/colors.xml` and `res/values-night/colors.xml`.
- **The list refreshes on `Saved` only while it's showing.** Otherwise it's marked stale and refreshes when the editor closes, so typing never triggers list queries.
- **Notebook name errors** such as "A notebook with that name already exists" show in the problem bar, since the name dialog has already closed.
- **Shortcuts:** with a hardware keyboard, Ctrl+Z, Ctrl+Shift+Z and Ctrl+Y undo and redo through the core, and Ctrl+N starts a note.
- **Tests:** `StoreTest` covers doc tests 1–4 through `Store` and a real `EditText`, and `MainActivityTest` covers test 5. Their cores stay open until the process ends, because an editor's scheduled save can still run after a test finishes.
- **Manual check on the emulator:** typed, pressed Home, force-stopped, and relaunched. The text was there, back in the editor. The debug cold start took 454 ms; step 11 measures release builds.
