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

netwatch's Android route monitor does nothing, so iroh never notices a network change by itself. Recovery on a LAN still works without help, through mDNS rediscovery and the 10 s redial. In the spike, sync reconnected 1.5 s after Wi-Fi returned from a 45 s outage. Add:

```rust
Control::NetworkChanged
```

This calls `node.endpoint.network_change().await`, then `self.tick()` to redial devices that aren't connected. Android sends it from a `ConnectivityManager.NetworkCallback`, which cut the same reconnection to 0.6 s. Desktop never needs to send it. The spike carried exactly this patch.

## 6. Replace a stale session when a device reconnects

When one side's process restarts, the other side keeps its dead session until QUIC's idle timeout of about 30 s. Meanwhile `Sync::adopt` (`src/sync.rs:730`) rejects the restarted device's new connection as a duplicate. In the spike, reopening the app left it disconnected for 19–30 s (spike Finding 10), and Android restarts app processes all the time.

Change `adopt` so a new connection can replace a session that is no longer fresh:

- **The existing session is young** (say, under 5 s old; record when each `Session` starts). This is two devices dialing each other at once, so keep the current rule, which keeps the connection dialed by the smaller ID.
- **The existing session is older.** A device only dials a paired device it has no session with. A new connection long after the session started means the other side has lost it, usually because its process restarted. Replace the old session with the new connection.

Dropping the old `Session` aborts its task. Nothing is lost, because the new session starts with a digest exchange.

## 7. Nothing else needed

- `Config::save` uses `std::os::unix::fs::OpenOptionsExt` with mode `0o600`. Android is Unix, so this works unchanged (confirmed in the spike).
- `Command::Flush` / `Event::Flushed` already give Android a way to wait until earlier saves are on disk (step 8).
- `Repository::open` creates the parent directories itself.

## Tests

1. `tests/network.rs`-style test with two nodes in one process: suspend A, edit on B, resume A. Both converge, and A's status shows `suspended` during the pause.
2. `SetRelay` while suspended saves the config but doesn't bind. After resume, the node uses the new relay.
3. `NetworkChanged` while connected leaves the session up. While disconnected, it triggers a dial.
4. The status sink receives a status on startup and after each change, the same statuses the channel used to carry.
5. With no `device_name` in the config, the default argument is used and saved. With one present, the argument is ignored.
6. Two nodes connected; drop node B's `SyncHandle` and start a new one on the same config and database. A accepts B's new connection at once, without waiting for the old one to time out. Test this with both A and B as the original dialer.
7. Two nodes dialing each other at the same moment still end up with exactly one session, as the existing simultaneous-dial test checks.

## Notes

_Anything surprising goes here._
