# 1. Spike: core and sync on a real phone

**Status:** Done, except the manual pairing with the real GTK app and the checks listed under "Not covered"
**Needs:** nothing
**Branch:** `android-spike` (commits `85cfedd`, `aab2330`). Throwaway: only this doc is kept. The branch holds the code, in case a later step wants to copy from it.

## Goal

Before any refactoring, find out whether the parts we can't easily change work on Android:

- rusqlite with bundled SQLite, including the FTS5 search table
- iroh 1.2: binding an endpoint, mDNS discovery (`iroh-mdns-address-lookup` → `swarm-discovery`), and `netwatch`'s interface monitoring
- pairing and syncing between a phone and the Linux app
- the relay path

**Answer: all of it works.** None of the results changes the plan's direction. They adjust details in steps 5, 6, 7, 10 and 11, listed under Findings.

## Toolchain setup

This is what worked; later steps need it too.

1. **Rust:** `rustup target add aarch64-linux-android x86_64-linux-android` and `cargo install cargo-ndk --locked` (4.1.2).
2. **SDK command-line tools:** unzip `commandlinetools-linux-16111833_latest.zip` to `~/Android/Sdk/cmdline-tools/latest`. In version 23, `sdkmanager` is deprecated in favour of the Android CLI, which uses slash-separated package names.
3. **NDK:** `~/Android/Sdk/cmdline-tools/latest/bin/android sdk --sdk=$HOME/Android/Sdk install ndk/29.0.14206865`
4. **Environment:** `ANDROID_NDK_HOME=~/Android/Sdk/ndk/29.0.14206865`. cargo-ndk also finds the highest version under `~/Android/Sdk/ndk` by itself.

## What was built

On `android-spike`:

- **Root crate:** `gtk` became an optional default feature, and the `notebook` binary requires it. The root package became a workspace with the spike crate as a member. There's a `spike-release` profile (fat LTO, one codegen unit).
- **Core patch:** `Control::NetworkChanged` in `src/sync.rs`, calling `Endpoint::network_change()` and then redialing.
- **`spike/ffi` (`notebook-spike`):**
  - A UniFFI 0.32 object `Spike` wrapping the storage worker and the sync thread.
  - One hand-written JNI export, `AndroidContext.install`.
  - Logging to logcat.
  - `examples/spike_peer.rs`, a laptop-side peer driven from the command line and stdin.
- **Spike app:** one framework `Activity` driven by buttons or by `adb shell am start … --es cmd "…"`, with results in logcat under `NotebookSpike`.
- **Scripts:** `spike/build-android.sh` (cargo-ndk plus bindings) and `spike/phone.sh` (adb helpers).

## Test setup

- **Phone:** Motorola Edge 50 Fusion, Android 16 (SDK 36), arm64, 4 KB pages, on the same Wi-Fi as the laptop.
- **Laptop peer:** the spike's laptop peer, which runs the same `sync.rs` as the GTK app.
- **Builds:** the phone ran the `spike-release` library unless stated otherwise.
- **Clocks:** latencies are corrected for the phone's clock running 1.425 s behind the laptop's. Phone-side times include up to 500 ms of event polling.

## Results

| # | Check | Result |
| --- | --- | --- |
| 1 | The library builds for `arm64-v8a` and `x86_64` | ✅ Both build. Making `gtk` optional was the **only** change needed: no other fixes, no warnings. |
| 2 | The database opens and search finds notes | ✅ Prefix search works (FTS5 is in bundled SQLite). Notes survive a force-stop. |
| 3 | The sync endpoint binds | ✅ It binds on the Wi-Fi address and `192.0.0.2` (464XLAT). mDNS sends and receives. |
| 4 | The phone discovers the laptop over mDNS without its own `MulticastLock` | ✅ Paired and exchanged notes in about 1.6 s. Google Play services and the system's `NsdService` already held multicast locks, so this isn't a true "no lock" test (Finding 9). |
| 5 | Same, while holding a `MulticastLock` | ✅ |
| 6 | Pairing works in both directions | ✅ The laptop also found the phone's code over mDNS. |
| 7 | Edits sync both ways | ✅ New notes arrived in about 0.1 s each way. Edits took 0.11 s from phone to laptop and at most 0.37 s from laptop to phone. |
| 8 | Sessions recover after the network changes | ✅ A 20 s Wi-Fi drop with a new IP kept the session alive. A 45 s drop timed out the session, which came back 1.5 s after Wi-Fi returned, or 0.6 s with a `NetworkChanged` nudge. Switching between networks wasn't tested. |
| 9 | The relay path works | ✅ Worked through n0's public `aps1` relay with the laptop in relay-only mode: about 0.13 s from phone to laptop, at most 0.56 s the other way. TLS needed no setup (Finding 6). |
| 10 | Release `.so` and APK size | ✅ `.so`: 16.2 MB stripped, or 14.1 MB with `opt-level = "s"`. Release APK: 18.8 MB on disk, **8.4 MB download**. 16 KB aligned, and `zipalign -P 16` passes. |
| 11 | Time from process start to the first search result | ✅ Release APK with 10,000 notes: **about 170 ms** from tap to first search result and about 190 ms to the first frame. The core opens in 16–21 ms. The first search (2,350 matches) takes about 40 ms. |

## Findings

Each finding names the step that acts on it.

1. **Step 2:** Making GTK optional is enough for the core to compile for Android. rusqlite (bundled, with FTS5), ring, blake3, iroh, netwatch, netdev and swarm-discovery all build unchanged.
2. **Step 7:** NDK r29's sysroot only goes up to API 35. Build with `-P 35`, which is fine because the native API level only has to be at or below minSdk 36. cargo-ndk defaults to API 21, so always pass `-P`. AGP should pin `ndkVersion = "29.0.14206865"`.
3. **Step 7:** `cargo ndk -o` copies every cdylib in the build, including iroh's and iroh-relay's own, so copy only `libnotebook_ffi.so` into `jniLibs`.
4. **Step 7:** AGP 9.4's built-in Kotlin compiles `.kt` sources with no Kotlin plugin applied. A first build downloads Gradle 9.6 and works.
5. **Steps 6 and 7 (UniFFI 0.32):**
   - Proc macros and `uniffi.toml` (`package_name`, `cdylib_name`) work as expected.
   - Generate bindings with `uniffi-bindgen generate <lib.so> --language kotlin --out-dir … --no-format`; `--library` is deprecated and does nothing.
   - Enable the `cli` feature only on the host (`cfg(not(target_os = "android"))`), so clap and the other bindgen crates aren't built for Android.
   - Add JNA as `net.java.dev.jna:jna:5.19.1` with `artifact { type = "aar" }`.
   - `u32` becomes Kotlin `UInt`; prefer signed types in the real API.
6. **Steps 6, 7 and 10:** iroh 1.2 never calls `rustls-platform-verifier`, because relay TLS uses built-in webpki roots. Relays work with no verifier setup, so **drop the verifier init and its AAR** from the plan. It only matters if we ever opt into `CaTlsConfig::system()`.
7. **Steps 6 and 10:** The one JNI call we need is `iroh::dns::install_android_jni_context(vm, app_context)`, run once before starting the core and guarded with `Once`.
   - Without it, iroh's DNS resolver panics internally ("android context was not initialized"). It survives only because Rust panics unwind, then falls back to 1.1.1.1 and 8.8.8.8 instead of the network's own DNS.
   - **Keep `panic = "unwind"`** in every Android profile.
8. **Step 5:** `Settings.Global.DEVICE_NAME` gives a good default name ("motorola edge 50 fusion"). The spike set the name on every start, which would overwrite a name the user chose. Pass it only as the default for when the config has none, as step 5 describes.
9. **Step 10:** On this phone, Google Play services and `NsdService` always hold multicast locks, so mDNS works even without ours. Phones without Google Play services won't have them. The app should still hold its own lock while syncing in the foreground.
10. **Steps 5 and 10: restarted peers wait for a stale session.**
    - If one side's process restarts, the other side keeps the dead session until QUIC's idle timeout (about 30 s). Meanwhile `adopt()` rejects the new connection as a duplicate.
    - Seen twice: a restarted laptop peer took 19 s to reconnect, and after the phone app restarted, the laptop showed it disconnected for about 25–30 s.
    - Android kills app processes routinely, so reopening the app often hits this. The fix in step 5: when a paired device connects while a session with it exists, replace the old session instead of keeping it.
11. **Steps 5 and 10:** netwatch's Android route monitor does nothing, but recovery doesn't depend on it:
    - QUIC migrated a live session across a Wi-Fi drop and IP change.
    - After a longer outage, mDNS rediscovery plus the 10 s redial reconnected in 1.5 s.
    - A `NetworkChanged` nudge from `ConnectivityManager` cut that to 0.6 s. It's worth keeping, as a cheap improvement rather than a requirement.
12. **Step 10: mDNS queries every 0.7 s for as long as sync runs** (75–83 queries a minute, with no back-off). That's swarm-discovery's default cadence, and `iroh-mdns-address-lookup` doesn't expose a setting for it. To save battery:
    - run mDNS only while the app is visible (already planned),
    - consider pausing discovery while every paired device is connected, and
    - propose a cadence option upstream.
13. **Step 6:** Loro logs block diagnostics at INFO on every save. Any logcat subscriber must cap `loro_internal` at WARN.
14. **Steps 7 and 11 (size):**
    - The `.so` needs only `libc`, `libm`, `libdl` and `liblog`.
    - `opt-level = "s"` saves about 2 MB raw and 1.5 MB compressed (20%); step 11 should weigh that against speed.
    - The download size (8.4 MB) is mostly the Rust library. JNA's `libjnidispatch.so` can't be stripped, which only produces a warning.
15. **Step 11 (speed):**
    - Debuggable builds are about twice as slow at startup (370 ms), so measure only release builds.
    - Seeding 10,000 notes took 10 s, about 1 ms per note with `synchronous=FULL`.
    - Listing all 10,013 notes took 99 ms in a debuggable build. Paging isn't needed for v1.
16. **Step 10:** On Android 16, LAN sync needed no runtime permission. Android 17's local network permission is still untested.

## Not covered

- **Pairing with the real GTK app (manual).** The laptop peer runs the same sync code, but check the real app once. Before pairing, clear the spike app's data (`adb shell pm clear com.pkkulhari.notebook`), and expect its blank starting note to appear on the desktop as "Untitled".
- **An Android 17 device**, to test `ACCESS_LOCAL_NETWORK` enforcement.
- **Switching between networks, or mobile data only** (needs another SSID or a SIM).
- **Whether the laptop's active ufw firewall needed a rule** for LAN sync. It worked in both directions, but the rule state wasn't recorded.
