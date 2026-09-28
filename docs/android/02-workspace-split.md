# 2. Split the crate into a workspace

**Status:** Not started
**Needs:** nothing
**Behaviour change:** none

## Why

`Cargo.toml` makes `gtk4` a dependency of the whole package, so even the library can't be built for Android. The library is `src/lib.rs` with `crdt`, `markdown`, `model`, `storage` and `sync`. The UI is already separate: it is a module of the binary (`src/main.rs` declares `mod ui`). So this step is mostly moving files.

## Target layout

```
Cargo.toml                    workspace root: members, shared dependencies, profiles
Cargo.lock
crates/
  core/                       package notebook-core, library notebook_core
    Cargo.toml
    src/lib.rs crdt.rs markdown.rs model.rs storage.rs sync.rs
    tests/storage.rs sync.rs network.rs
    examples/benchmark.rs seed_demo.rs
  gtk/                        package notebook, binary notebook (unchanged name)
    Cargo.toml                keeps [package.metadata.deb]
    src/main.rs ui.rs style.css
    tests/session-bus.conf
  ffi/                        added in step 6
data/  docs/  README.md       unchanged
android/                      Gradle project
```

## Steps

1. Move files with `git mv` so their history follows them.
2. Write the root `Cargo.toml`. Profiles must stay at the root, because Cargo ignores them in member manifests.
   ```toml
   [workspace]
   resolver = "3"
   members = ["crates/core", "crates/gtk"]
   default-members = ["crates/core", "crates/gtk"]

   [workspace.package]
   version = "0.2.0"
   edition = "2024"
   rust-version = "1.92"
   license = "MIT"

   [workspace.dependencies]
   blake3 = "1.8.7"
   loro = "1.16"
   # … every dependency shared by more than one member

   [profile.dev]
   debug = 0

   [profile.release]
   strip = true
   ```
3. Write `crates/core/Cargo.toml` with every current dependency except `gtk`, plus `tempfile` as a dev-dependency.
4. Write `crates/gtk/Cargo.toml`:
   - Package `notebook`, with dependencies on `notebook-core` (by path), `gtk` and `loro`. `ui.rs` uses `LoroDoc` and `UndoManager` directly until step 3 moves them out.
   - Move `description` and `[package.metadata.deb]` here.
   - cargo-deb resolves asset paths relative to the package directory. It maps `target/release/…` to the real target directory, so that entry can stay as it is. The others become `../../README.md` and `../../data/io.github.pkkulhari.Notebook.desktop`.
5. Replace `notebook::` with `notebook_core::` in `ui.rs`, the tests and the examples.
6. Leave `storage::data_path()`, `sync::config_path()` and `xdg_path` in core for now. They are Linux conventions, and step 5 moves them into the gtk crate.
7. In the README, change the package command to `cargo deb -p notebook --locked`. `cargo build --release` stays the same because of `default-members`.

## Done when

- `cargo test` at the root runs the same tests as before, and they pass.
- `cargo tree -p notebook-core -e normal | grep -i gtk` prints nothing.
- `cargo ndk -t arm64-v8a build -p notebook-core` succeeds, using the toolchain from step 1.
- `cargo deb -p notebook --locked` builds a package with the same files as before. Compare with `dpkg -c`.
- `./target/release/notebook` opens an existing database without changes.

## Notes

_Anything surprising goes here._
