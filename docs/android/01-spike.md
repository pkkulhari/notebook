# 1. Spike: core and sync on a real phone

**Status:** Not started
**Needs:** nothing
**Branch:** `android-spike`, a throwaway branch. Only this doc's Findings section is kept.

## Goal

Before any refactoring, find out whether the parts we can't easily change work on Android:

- rusqlite with bundled SQLite, including the FTS5 search table
- iroh 1.2: binding an endpoint, mDNS discovery (`iroh-mdns-address-lookup` → `swarm-discovery`), and `netwatch`'s interface monitoring
- pairing and syncing between a phone and the Linux app
- the relay path, which brings in `rustls-platform-verifier` through `reqwest`

A spike that fails here changes steps 5, 7 and 10, so it goes first.

## Toolchain setup

Later steps need this setup too.

1. Install the NDK. In Android Studio, open SDK Manager → SDK Tools → NDK (Side by side), or run `sdkmanager "ndk;<version>"`. Use r28 or newer, which aligns libraries to 16 KB pages by default. Nothing is installed under `~/Android/Sdk/ndk` yet.
2. `rustup target add aarch64-linux-android x86_64-linux-android`
3. `cargo install cargo-ndk`
4. Set `ANDROID_NDK_HOME` if cargo-ndk doesn't find the NDK on its own.

## Steps

1. On `android-spike`, make GTK optional in the root `Cargo.toml` so the library builds without it:
   ```toml
   [lib]
   crate-type = ["lib", "cdylib"]

   [features]
   default = ["gtk"]

   [[bin]]
   name = "notebook"
   path = "src/main.rs"
   required-features = ["gtk"]
   ```
   Make `gtk` `optional = true`. Then check that this builds:
   `cargo ndk -t arm64-v8a build --release --lib --no-default-features`
   Fix only what blocks the build, and list every fix in Findings.
2. Add a small throwaway UniFFI surface. Using UniFFI now also tests the pipeline that step 7 needs.
   - `open(dir)`: calls `storage::spawn_worker` and `sync::spawn`, with paths under the app's `filesDir`. Pass `noBackupFilesDir` for `sync.json`.
   - `create_note(body)` and `search(query) -> Vec<String>`
   - `set_sync_enabled(bool)`, `start_pairing()` and `join_pairing(code)`
   - `status() -> String`: the `Debug` output of the latest `sync::Status`
3. Build a single Activity with buttons and a `TextView` that shows the status, refreshed every 500 ms. Generate the bindings by hand with `uniffi-bindgen`.
4. Run it on a physical phone on the same Wi-Fi as the Linux laptop, with `targetSdk = 37` as in the skeleton.

## Checks

Fill in the Result column as you go.

| # | Check | How | Result |
| --- | --- | --- | --- |
| 1 | The library builds for `arm64-v8a` and `x86_64` | `cargo ndk` | |
| 2 | The database opens and search finds notes | Create notes, then search for a word prefix | |
| 3 | The sync endpoint binds | `status.addr` is `Some` | |
| 4 | The phone discovers the laptop over mDNS without a `MulticastLock` | Show a pairing code on the laptop, enter it on the phone | |
| 5 | Same, while holding a `WifiManager.MulticastLock` | As in 4 | |
| 6 | Pairing works in both directions | Code shown on the phone, entered on the laptop, and the reverse | |
| 7 | Edits sync both ways | Type on each device and watch the other | |
| 8 | Sessions recover after the network changes | Turn Wi-Fi off and on, and switch networks, without restarting sync | |
| 9 | The relay path works | Set a relay URL on both devices with Wi-Fi off, and record whether TLS fails until `rustls-platform-verifier` is initialised | |
| 10 | Release `.so` and APK size | Build with `lto = true`, `codegen-units = 1` and stripping | |
| 11 | Time from process start to the first search result | Log timestamps | |

## Likely problems

- `Config::load` (`src/sync.rs:68`) reads `/proc/sys/kernel/hostname`, which says `localhost` on Android. Step 5 fixes this.
- Android 11+ blocks some netlink calls for apps (for example `RTM_GETLINK`). This may affect `netwatch`'s interface monitoring, so check logcat.
- With `targetSdk = 37`, Android 17's local network protection may block LAN traffic until the app holds the local-network permission. Find out what the check above needs.
- `rustls-platform-verifier` panics with "Expect rustls-platform-verifier to be initialized" if a TLS certificate is verified before `init_with_env` is called.

## Findings

_Fill this in after the spike. Name the step (5, 7, 10 or 11) that should act on each finding._
