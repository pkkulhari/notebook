# 5. Platform hooks in core

**Status:** Not started
**Needs:** step 2 (and step 1's findings)
**Behaviour change:** none on desktop

## Why

`storage` and `sync` already take their file paths as arguments and talk through channels. A few Linux assumptions and polling-only interfaces remain, and a phone can't live with them. Each item below is small and can be its own commit.

## 1. Linux paths move to the gtk crate

`storage::data_path()`, `sync::config_path()` and `storage::xdg_path` (`src/storage.rs:969`, `src/sync.rs:112`) encode XDG conventions. Move them to `crates/gtk`, and update `examples/seed_demo.rs` to take the path from its argument or build it the same way. Core then never guesses where files live.

Android passes:

| File | Location | Backed up |
| --- | --- | --- |
| `notebook.db` | `filesDir/notebook.db` | yes |
| `sync.json` | `noBackupFilesDir/sync.json` | never: it holds this device's private key |

## 2. Device name comes from the caller

`Config::load` (`src/sync.rs:68`) falls back to `/proc/sys/kernel/hostname`, which says `localhost` on Android. Add a `default_device_name: String` argument to `sync::spawn`, used only when the config has no name yet:

- GTK passes the hostname, keeping today's logic in the gtk crate.
- Android passes `Settings.Global.DEVICE_NAME`, falling back to `Build.MODEL`.

## 3. Status is pushed, not polled

`SyncHandle::status()` drains a `std::sync::mpsc::Receiver<Status>` held inside the handle, and GTK polls it every 16 ms. The receiver isn't `Sync`, so an FFI layer can't block on it while other threads send controls through the same handle.

Change `sync::spawn` to take a status sink:

```rust
pub fn spawn(
    config_path: PathBuf,
    db_path: PathBuf,
    default_device_name: String,
    storage: std::sync::mpsc::Sender<Command>,
    on_status: impl Fn(Status) + Send + 'static,
    options: Options,
) -> SyncHandle
```

`Sync::publish` calls `on_status` wherever it sends on `status_tx` today. GTK passes `move |status| { let _ = tx.send(status); }` and keeps polling its own receiver, so its behaviour doesn't change.

Storage events need no change. `spawn_worker` returns a plain `Receiver<Event>`, and the FFI layer can block on `recv()` in a thread of its own.

## 4. Suspending sync without turning it off

`Control::SetEnabled` saves `enabled` to `sync.json`, so it can't be used to go quiet in the background. Add:

```rust
Control::Suspend(bool)
```

- **`Suspend(true)`:** calls `stop()`, which closes the endpoint and sessions, detaches the outbox and cancels pairing, and records `suspended = true`. It doesn't touch the config.
- **`Suspend(false)`:** clears the flag, then calls `start()` if `config.enabled`. `start()` → `tick()` dials paired devices at once, and each new session starts with a digest exchange. That catches up on changes made while suspended. Nothing is lost, because changes made while detached are found by the same reconciliation.
- `SetEnabled`, `SetRelay` and `restart()` must respect `suspended`. While suspended they save the config, but don't bind.
- Add `suspended: bool` to `Status` so the UI can tell a paused sync from one that is off.

## 5. Network changes

On Android, interface monitoring inside iroh may be limited (check step 1's findings). Add:

```rust
Control::NetworkChanged
```

This calls `node.endpoint.network_change().await`, then `self.tick()` to redial devices that aren't connected. Android sends it from a `ConnectivityManager.NetworkCallback`. Desktop never needs to send it.

## 6. Nothing else needed

- `Config::save` uses `std::os::unix::fs::OpenOptionsExt` with mode `0o600`. Android is Unix, so this works unchanged.
- `Command::Flush` / `Event::Flushed` already give Android a way to wait until earlier saves are on disk (step 8).
- `Repository::open` creates the parent directories itself.

## Tests

1. `tests/network.rs`-style test with two nodes in one process: suspend A, edit on B, resume A. Both converge, and A's status shows `suspended` during the pause.
2. `SetRelay` while suspended saves the config but doesn't bind. After resume, the node uses the new relay.
3. `NetworkChanged` while connected leaves the session up. While disconnected, it triggers a dial.
4. The status sink receives a status on startup and after each change, the same statuses the channel used to carry.
5. With no `device_name` in the config, the default argument is used and saved. With one present, the argument is ignored.

## Notes

_Anything surprising goes here._
