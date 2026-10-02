# 10. Sync on Android

**Status:** Not started
**Needs:** step 8 (and step 1's findings, step 5's `Suspend` and `NetworkChanged`)

## Goal

A phone pairs with the Linux app, or with another phone, using the same six-digit code. It then syncs with it whenever both apps are open on the same network, or anywhere through a relay. The protocol, pairing, and merge rules are all in the core already. This step adds the screen, the Android permissions, and the lifecycle glue.

## Screen

Build a sync screen (or bottom sheet) that mirrors the desktop dialog (`show_sync_dialog`, `crates/gtk/src/ui.rs:2032`) and uses the same wording:

- **Sync with your other devices**: an on/off switch, with the hint "Notes travel directly between your paired devices, encrypted. Nothing leaves this network unless you add a relay."
- **This device's name**: sends `SetDeviceName` when editing finishes.
- **Paired devices**: each row shows the name and "Connected", "Not connected. Last synced: …", or "Not connected", with a **Remove** button (`Forget`). When the list is empty it says "No paired devices yet."
- **Pairing:**
  - **Show a pairing code**: displays "123 456" with **Cancel**.
  - **Enter a code**: a six-digit numeric field that shows a searching state.
  - Outcomes: "Paired with …" or the failure message from `Pairing::Failed`.
  - Pairing controls are disabled unless `status.running`.
- **Sync over the internet**: the relay address field, sending `SetRelay`.
- **Problem**: `status.problem`, shown only when present.

The top bar of the list screen shows a small sync indicator. It is active when any device is connected, like the desktop's `sync-active` button, and tapping it opens this screen.

Status arrives through `CoreListener.onSyncStatus`, posted to the main thread. While status updates are applied to the widgets, their listeners must not send controls back. The desktop has a `quiet` flag for this.

## Manifest

```xml
<uses-permission android:name="android.permission.INTERNET" />
<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE" />
<uses-permission android:name="android.permission.ACCESS_WIFI_STATE" />
<uses-permission android:name="android.permission.CHANGE_WIFI_MULTICAST_STATE" />
```

**Local network access:** with `targetSdk = 37`, Android 17's local network protection may require a runtime permission before the app can reach devices on the LAN. Step 1 records what it needs.

- Request the permission when the user turns sync on or starts pairing, never at launch.
- Explain it in one line: "Notebook finds your other devices on this network."
- If it's denied, sync still works through a relay. Say so in the problem line.

## Lifecycle

Sync runs while the app is visible. Peer-to-peer sync needs both apps open anyway, and background networking on Android is heavily restricted.

| Moment | Action |
| --- | --- |
| `Activity.onStart` | Acquire the `MulticastLock` (if sync is on), register the network callback, then `sync(Suspend(false))`. |
| `Activity.onStop` | `flush()`, then `sync(Suspend(true))`, then unregister the callback and release the lock. |
| Sync turned on or off | Acquire or release the lock to match. |

- **Multicast lock:** `WifiManager.createMulticastLock("notebook-sync")` with `setReferenceCounted(false)`. Without it, many phones drop incoming mDNS packets. On the spike's phone, Google Play services and the system's `NsdService` already held locks, which is why mDNS worked without ours. Don't rely on that.
- **Network callback:** `ConnectivityManager.registerDefaultNetworkCallback`. Send `NetworkChanged` from `onAvailable`, `onLost` and `onLinkPropertiesChanged`, debounced by 500 ms. In the spike this cut reconnection after a Wi-Fi outage from 1.5 s to 0.6 s.
- **mDNS cost:** swarm-discovery sends a query about every 0.7 s for as long as the endpoint runs, with no back-off (75–83 a minute in the spike). Suspending in `onStop` already limits that to while the app is visible.
  - Worth considering: pause discovery while every paired device is connected.
  - Also propose a cadence option upstream, since `iroh-mdns-address-lookup` doesn't expose swarm-discovery's `with_cadence`.
- **Reopening the app:** until step 5's stale-session fix lands, a device the phone synced with before the app restarted can take up to 30 s to reconnect.
- **Pairing across backgrounding:** suspending cancels a pairing in progress, just as stopping sync does on the desktop. When the app comes back, the pairing section shows its idle state again.

## Files and backup

- `sync.json` is in `noBackupFilesDir`, so Android never backs it up (step 5). A phone restored from backup gets a new identity and must be paired again. Its notes then merge with the other devices' copies without duplicates, as the README describes for copied databases.
- Update `res/xml/data_extraction_rules.xml` and `backup_rules.xml`:
  - Include `notebook.db` together with its `-wal` file, so a backup is never missing recent writes.
  - Exclude nothing else. Cache and the no-backup directory are already excluded.

## Android context and TLS

- `AndroidContext.install(context)` (step 6) must run in `Application.onCreate` before `Core.start`. Without it, iroh can't read the network's DNS servers and falls back to 1.1.1.1 and 8.8.8.8. That bypasses the user's network or private DNS, and fails on networks that block outside DNS.
- Relay TLS needs no setup. iroh uses built-in webpki roots, and the spike synced through n0's public relay with no verifier initialised.

## Interop tests (manual, on real devices)

Record the results in Notes, with the phone model and Android version.

1. Pair with the code shown on the Linux app and entered on the phone, then the reverse.
2. Type in the same note on both devices at once: both end with the same text, and neither loses characters.
3. Edit on the phone while the laptop is off. Open both: the changes merge.
4. Create a notebook on each device while apart, with the same name: both keep it, and one reads "Name (2)".
5. Turn Wi-Fi off and on, and switch networks, while syncing: sessions come back without toggling sync.
6. With a relay on both devices, sync over mobile data.
7. Remove the phone on the laptop: the phone can no longer connect, and says so.
8. Background the app for ten minutes, then return: sync reconnects within a few seconds.

## Out of scope for v1

- Syncing in the background. A later version could offer an opt-in "Sync in the background". Options would be a periodic `WorkManager` job that syncs briefly on Wi-Fi, or a foreground service. Each has battery and platform costs to weigh, and both only help when the other device is online.
- QR codes for pairing.

## Notes

_Anything surprising goes here._
