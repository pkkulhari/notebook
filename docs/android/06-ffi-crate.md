# 6. FFI crate

**Status:** Done
**Needs:** steps 3, 4 and 5
**Adds:** `crates/ffi` (package `notebook-ffi`)

## Goal

A thin layer that exposes `notebook-core` to Kotlin through UniFFI. It holds no business logic of its own: it wires the storage worker, sync thread and `Drafts` together, maps types, and enforces the threading rules below. Everything here should be simple enough to test from Rust on the desktop.

## Crate setup

```toml
[package]
name = "notebook-ffi"

[lib]
crate-type = ["cdylib", "lib"]
name = "notebook_ffi"

[[bin]]
name = "uniffi-bindgen"          # pins the generator to the library's UniFFI version
path = "src/bin/uniffi-bindgen.rs"

[dependencies]
notebook-core = { path = "../core" }
uniffi = "=0.32.2"

# The bindings generator (clap and friends) is only built for the host.
[target.'cfg(not(target_os = "android"))'.dependencies]
uniffi = { version = "=0.32.2", features = ["cli"] }

[target.'cfg(target_os = "android")'.dependencies]
iroh = "1.2"                                          # for install_android_jni_context
jni = { version = "0.22", default-features = false }
```

- `src/bin/uniffi-bindgen.rs` is `fn main() { #[cfg(not(target_os = "android"))] uniffi::uniffi_bindgen_main() }`.
- Every Android profile keeps `panic = "unwind"`. UniFFI turns panics into Kotlin exceptions, and iroh's DNS resolver relies on unwinding to recover from a missing Android context (spike Finding 7).
- `src/lib.rs` starts with `uniffi::setup_scaffolding!();` and uses proc macros only, with no UDL file.
- `uniffi.toml` sets the Kotlin package to `com.pkkulhari.notebook.core` and `cdylib_name` to `notebook_ffi`.
- Add `crates/ffi` to `[workspace] members`, but not to `default-members`, so desktop builds don't compile it.

## Threading rules

These rules keep the text widget and the editor's document identical. Both the Rust and Kotlin sides must follow them.

1. **Every call that changes or reads a draft's text happens on the Android main thread**, in the same order as the `EditText` changes. That covers `openDraft`, `replace`, `undo`, `redo` and `importDelta`.
2. **A storage change to an open note reaches Kotlin as an event.** Kotlin then calls `importDelta` on the main thread and applies the returned edits at once. The FFI layer never imports into a draft on its own thread. If it did, the document would get ahead of the `EditText`, and the next keystroke would land in the wrong place.
3. **Bookkeeping that doesn't change text is handled in Rust as soon as the event arrives:** `Saved` → `drafts.saved`, `Created` → `drafts.created`, `Mutated` and `Remote` → the open drafts' notebooks and trash state, and failed saves recorded for retry. The event is then forwarded to Kotlin.
4. **`Drafts` sits behind a `Mutex`.** The main thread holds it only for the length of a call, and the event thread holds it only for bookkeeping.
5. **Listener callbacks run on a Rust thread.** Kotlin posts them to the main thread before touching any UI or calling back into `Core`.

## API sketch

```rust
#[derive(uniffi::Record)]
pub struct CoreConfig {
    pub database_path: String,       // filesDir/notebook.db
    pub sync_config_path: String,    // noBackupFilesDir/sync.json
    pub device_name: String,         // Settings.Global.DEVICE_NAME or Build.MODEL
}

#[uniffi::export(with_foreign)]
pub trait CoreListener: Send + Sync {
    fn on_event(&self, event: CoreEvent);
    fn on_sync_status(&self, status: SyncStatus);
}

#[derive(uniffi::Object)]
pub struct Core { /* commands, drafts: Mutex<Drafts>, sync: SyncHandle, failed */ }

#[uniffi::export]
impl Core {
    #[uniffi::constructor]
    pub fn start(config: CoreConfig, listener: Arc<dyn CoreListener>) -> Arc<Self>;

    // Storage
    pub fn initialize(&self);
    pub fn list(&self, filter: Filter, query: String, generation: i64);
    pub fn load(&self, id: String, generation: i64);
    pub fn create_note(&self) -> NoteInfo;        // installs an empty draft, sends Create
    pub fn mutate(&self, mutation: Mutation);
    pub fn save_preferences(&self, selected_note: Option<String>, cursor: i32);
    pub fn flush(&self);                            // flush drafts, then Command::Flush
    pub fn retry(&self);                            // resend failed commands

    // Editor (main thread only)
    pub fn open_draft(&self, note: NoteInfo, snapshot: Vec<u8>) -> String;   // returns text
    pub fn replace(&self, id: String, at: i32, len: i32, text: String) -> Result<(), CoreError>;
    pub fn undo(&self, id: String) -> Option<Applied>;
    pub fn redo(&self, id: String) -> Option<Applied>;
    pub fn import_delta(&self, id: String, delta: Vec<u8>) -> Applied;
    /// Sends saves that are due; returns milliseconds until the next one, or -1.
    pub fn tick(&self) -> i64;
    pub fn trim_drafts(&self, active: Option<String>);

    // Sync
    pub fn sync(&self, control: SyncControl);
}

#[uniffi::export] pub fn parse_markdown(text: String) -> Arc<MarkdownDocument>;
#[uniffi::export] pub fn list_enter(line: String) -> Option<ListEnter>;
```

### Events

`CoreEvent` mirrors `storage::Event`:

- `Ready { default_notebook_id, notebooks, note, snapshot, cursor }`
- `Listed { notes, counts, generation }`
- `Loaded { note, snapshot, generation }`
- `Created { id }` and `Saved { id }`
- `Mutated { mutation, notebooks }`
- `NoteDelta { id, delta }` and `Remote { trashed, notebooks }`, where `trashed` lists the open drafts another device moved to the trash
- `Flushed`
- `Error { operation, message, retryable }`

A thread named `notebook-events`, owned by `Core`, blocks on the storage `Receiver<Event>`. It applies rule 3, then calls `listener.on_event`. Sync status comes through the sink added in step 5 and goes straight to `listener.on_sync_status`.

### Type mapping

| Core | FFI |
| --- | --- |
| `Filter` | `uniffi::Enum`: `Notebook { id }`, `All`, `Trash` |
| `NoteCounts` (`HashMap<Filter, u64>`) | `Vec<NoteCount { filter, count: i64 }>` |
| `Note` | `NoteInfo { id, notebook_id, deleted }` (no body; the text comes from the draft) |
| `NoteSummary`, `Notebook`, `Mutation` | records or enums with the same fields |
| `TextEdit`, `Applied` | `uniffi::Enum` and `Record`, positions as `i32` UTF-16 units |
| `sync::Status` | `SyncStatus` without `EndpointAddr`: `running: bool` replaces `addr` |
| `sync::Pairing` | `uniffi::Enum` |
| `Control` | `SyncControl`, without `Introduce` and `Shutdown` |
| `Box<dyn Error>` | `CoreError::OutOfSync { message }` (the only synchronous error) |

- Use `i32` for text positions (Java strings are `int`-indexed) and `i64` for generations, sequences and timestamps. Kotlin's unsigned types are awkward, so keep them out of the API.
- `Drafts` is created with `Units::Utf16`.
- `CoreError::OutOfSync` means Kotlin passed a position outside the draft. That should never happen. If it does, Kotlin logs it, flushes, and reloads the note from storage, because it's safer than guessing.

### Markdown

`MarkdownDocument` is a `uniffi::Object` that wraps `markdown::Document` parsed in UTF-16 units. It has these methods:

- `spans() -> Vec<StyledRange>`, where the `style` field is an enum: `H1`, `H2`, `H3`, `Strong`, `Emphasis`, `Strike`, `Quote`, `Code`, `CodeBlock`, `Link`, `Task`, `Checked` and `ListItemStart`
- `list_markers() -> Vec<TextRange>`
- `hidden_outside(start: i32, end: i32) -> Vec<TextRange>`, called on every selection change without copying the whole document across
- `link_at(position: i32) -> Option<String>`
- `in_code_block(position: i32) -> bool`, used by list continuation

## Android context for DNS

This is the only hand-written JNI in the app. iroh reads the network's DNS servers through JNI, which needs the JVM and the application `Context`, and UniFFI can't pass either. Add one export, as the spike did (`spike/ffi/src/android.rs` on `android-spike`):

```
Java_com_pkkulhari_notebook_AndroidContext_install(env: EnvUnowned, class: JClass, context: JObject)
  → env.get_java_vm(), env.new_global_ref(context)            (never released)
  → iroh::dns::install_android_jni_context(vm.get_raw(), context.into_raw())
```

- Guard it with a `Once`. The underlying `ndk_context` asserts if it's installed twice.
- The Kotlin side is `object AndroidContext { init { System.loadLibrary("notebook_ffi") }; @JvmStatic external fun install(context: Context) }`, called once in `Application.onCreate` before `Core.start`.
- Load the library explicitly as shown: `install` can run before JNA has loaded it.
- Without this call, iroh still works but falls back to 1.1.1.1 and 8.8.8.8 instead of the network's DNS (spike Finding 7).
- No TLS setup is needed. iroh verifies relay certificates with built-in webpki roots and never calls `rustls-platform-verifier`, even though it's in `Cargo.lock` (spike Finding 6).
- Two `jni` versions are linked either way: 0.21 via netdev and 0.22 here. That's expected.

## Logging

Add an optional `logcat` feature, on only in debug builds. It installs a `tracing` subscriber that writes to logcat through `paranoid-android`, as the spike did, and makes iroh's connection logs visible while working on step 10. Release builds don't include it.

- Cap `loro_internal` at WARN, because Loro logs block diagnostics at INFO on every save.
- Show DEBUG for `swarm_discovery`, `iroh_mdns_address_lookup`, `netwatch` and `n0_dns_resolver` when debugging discovery.
- Set a panic hook that logs through `tracing`. Android discards stderr, so panics are otherwise invisible.

## Tests (host, in `crates/ffi/tests`)

1. `start` in a temp directory with a recording listener, then `initialize`: `Ready` arrives with a note.
2. `open_draft`, `replace` after an emoji, then `tick` after 300 ms: `Saved` arrives, and reloading from storage shows the same text.
3. `import_delta` built from a second `Repository`'s change returns edits in UTF-16 units that turn the old text into the new.
4. `replace` with a position past the end returns `OutOfSync` and leaves the draft unchanged.
5. `flush` followed by `Flushed`: every earlier save is acknowledged first.
6. Dropping `Core` stops the event thread and the sync thread; the test must not hang.

## Notes

- **Layout:** `src/lib.rs` (`Core` and the event thread), `src/types.rs` (records, enums and conversions), `src/markdown.rs` (`MarkdownDocument`, `parse_markdown`, `list_enter`), and `src/android.rs` (the JNI `install` and the optional `logcat` logging, copied from the spike).
- **The API grew a little beyond the sketch:**
  - `draft_text(id)`, to show a cached draft again without reloading it.
  - `show_draft(id)`, which records that a draft is showing so `trim_drafts` closes it last.
  - `has_failures()`, to hide the error bar once retries succeed.
  - `draft_note(id)`, an open draft's notebook and trash state, kept current by `Mutated` and `Remote` (rule 3), so Kotlin keeps no copy.
  - `Ready` carries the saved `selected_note` and `cursor`.
- **`create_note` puts the note in the default notebook**, using the `default_notebook_id` from `Ready`. Storage puts new notes there anyway.
- **`link_at` returns only http, https and mailto links**: it calls core's `Document::link_at`, as the desktop does, so Kotlin has nothing to filter.
- **Error operations** are an `Operation` enum: `Open`, `List`, `Load`, `Create`, `Save`, `Change`, `Preferences`, `Sync` and `Flush`. Failures of saves, creates, initialization and preferences are retryable; core's `editor::Failures` keeps them for both apps.
- **Bindings:** the Kotlin file has about 5,100 lines. `CoreError` becomes `CoreException`, and records become `data class`es.
- **Kotlin's listener must catch its own exceptions.** An exception thrown from `on_event` reaches Rust as a callback error, on the event thread.
- **Android build:** the library builds for arm64 with and without `logcat`, and exports `Java_com_pkkulhari_notebook_AndroidContext_install` plus the UniFFI functions.
- **Tests (`crates/ffi/tests/core.rs`):** the six planned ones, plus a failed save that is retried along with later edits (using a SQLite trigger, as the GTK test does), and the Markdown functions in UTF-16.
