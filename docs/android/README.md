# Android app: implementation plan

Notebook for Android, with the same philosophy as the Linux app: minimal, light and fast. Rust owns storage, the CRDT, Markdown parsing and sync. Kotlin draws the UI and forwards input.

This folder splits the work into steps. Each step lands on the `android-app` branch as one or more commits.

## Principles

- **One core, two UIs.** Rust holds everything that decides what a note contains: merging, undo, save batching and Markdown structure. Both the GTK and Android apps use it, and Kotlin never reimplements it.
- **The Linux app keeps working after every step.** At every commit, `cargo test` passes and the desktop app behaves as before.
- **Few dependencies.** The Android app uses no Compose, Room, Hilt, ViewModel or coroutines. The Rust library is most of the APK, so Kotlin should add as little as possible.
- **The main thread never waits on SQLite or the network.** Storage and sync stay on the Rust threads they already run on.

## Target architecture

```
 ┌──────────── Android app (Kotlin, framework Views) ────────────┐
 │ MainActivity · Store · NoteListAdapter · EditorController ·   │
 │ SyncScreen                                                    │
 └──────────────┬──────────────────────────────▲─────────────────┘
      calls     │   UniFFI-generated Kotlin    │ listener callbacks,
   (main thread)│                              │ posted to main thread
 ┌──────────────▼──────────────────────────────┴─────────────────┐
 │ notebook-ffi     Core · MarkdownDocument · parse_markdown     │
 └──────────────┬────────────────────────────────────────────────┘
 ┌──────────────▼──────────────── notebook-core ─────────────────┐
 │ editor   Drafts / Draft (editor's LoroDoc, undo, save batching)│
 │ storage  worker thread → SQLite (docs, notes, FTS5)           │
 │ sync     tokio thread → iroh, mDNS, pairing                   │
 │ markdown · crdt · model                                       │
 └──────────────▲────────────────────────────────────────────────┘
 ┌──────────────┴──── notebook (GTK4, Linux) ────┐
 │ main.rs · ui.rs                               │
 └───────────────────────────────────────────────┘
```

## Decisions

| Decision | Why |
| --- | --- |
| Cargo workspace with `crates/core`, `crates/gtk` and `crates/ffi` | `gtk4` is a dependency of the whole package today, so nothing builds for Android until it is split out. |
| The editor's document logic moves into core as `editor::Drafts` | `Draft` in `src/ui.rs` holds merge-critical code: the editor's own `LoroDoc`, undo of this device's edits only, and acked/in-flight save versions. Keeping a second copy in Kotlin would drift. |
| Positions cross the FFI boundary in UTF-16 units | Android's `Editable` counts UTF-16 units, while GTK and Loro count characters. Core converts, so Kotlin never counts characters. |
| UniFFI generates the bindings | It generates Kotlin code with objects, records, enums and callback interfaces. Each call costs microseconds, which doesn't matter at typing speed. |
| Framework Views, not Compose; the editor is an `EditText` with spans | This gives the smallest APK and fastest cold start. `EditText` spans map directly onto GTK text tags. |
| Sync runs only while the app is in the foreground (v1) | Peer-to-peer sync needs both devices online, and Android restricts networking in the background. |
| `sync.json` lives in `noBackupFilesDir` | A restored backup must never give two devices the same sync identity. |

## Steps

| # | Doc | Area | Needs | Result |
| --- | --- | --- | --- | --- |
| 1 | [Spike](01-spike.md) | Rust, Android | — | Evidence that the core and iroh work on a phone; findings recorded |
| 2 | [Workspace split](02-workspace-split.md) | Rust | — | `notebook-core` builds without GTK |
| 3 | [Editor session](03-editor-session.md) | Rust | 2 | `Draft` logic in core, used by GTK |
| 4 | [Text offsets](04-text-offsets.md) | Rust | 3 | UTF-16 positions for editing and Markdown |
| 5 | [Platform hooks](05-platform-hooks.md) | Rust | 2 | Status callbacks, sync suspend, network changes, device name |
| 6 | [FFI crate](06-ffi-crate.md) | Rust | 3, 4, 5 | `notebook-ffi`, the Kotlin-facing API |
| 7 | [Android build](07-android-build.md) | Gradle | 6 | `./gradlew assembleDebug` builds Rust and the bindings |
| 8 | [App shell](08-app-shell.md) | Kotlin | 7 | Notebooks, note list, search, trash, lifecycle |
| 9 | [Editor](09-editor.md) | Kotlin | 8 | Markdown editing on Android |
| 10 | [Sync](10-sync.md) | Kotlin, Rust | 8 | Pairing and syncing with the Linux app |
| 11 | [Release](11-release.md) | Both | 9, 10 | A signed, measured, size-checked APK |

- Step 1 runs on its own throwaway branch; only its findings are kept.
- Steps 3 and 5 can run in parallel once step 2 lands.
- Steps 9 and 10 are independent of each other.

## Working on the branch

- All work goes on `android-app`, branched from `master`. Rebase on `master` if desktop fixes land there in the meantime.
- Every commit leaves `cargo test` passing and the desktop app working.
- Each doc starts with a **Status** line. Update it as work lands, and write down anything surprising in the doc's **Notes** section, so later steps can use it.
- Code paths in these docs refer to the layout before step 2 (`src/ui.rs`) unless a doc says otherwise.
