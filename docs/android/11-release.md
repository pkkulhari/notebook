# 11. Release

**Status:** Not started
**Needs:** steps 9 and 10

## Goal

A release APK that is measurably small and fast, signed, and tested on real devices. The desktop README also documents how to build it.

## Budgets

The spike measured a Motorola Edge 50 Fusion (Android 16) with the `spike-release` library (opt-level 3, fat LTO), R8 off, and 10,000 notes. The budgets leave headroom for the real UI. Measure every release against them, and update the "Spike" column if the reference device changes.

| Metric | How to measure | Spike | Budget |
| --- | --- | --- | --- |
| APK download size (arm64) | `apkanalyzer apk download-size` | 8.4 MB | ≤ 10 MB |
| APK file size (arm64) | `apkanalyzer apk file-size` | 18.8 MB | ≤ 22 MB |
| Rust `.so` size | `ls -l` on the stripped library | 16.2 MB | ≤ 18 MB |
| Cold start to note list | `adb shell am start -W`, `TotalTime` | about 190 ms | ≤ 300 ms |
| Keystroke to screen | GPU profiler while typing in a 50,000-character note | — | no dropped frames |
| Open a note | Log timestamps from tap to text shown | — | < 100 ms |
| Memory, idle in editor | `adb shell dumpsys meminfo` | not measured | set in step 8 |

Measure only non-debuggable release builds. In the spike, a debuggable build took about twice as long to start.

## Size

**R8:** set `optimization { enable = true }` for release. The keep rules come from step 7. Check that the bindings, JNA and `AndroidContext.install` still work in a release build, because R8 problems only appear there.

**Rust library:**
- Use the `android` profile from step 7: `lto = "fat"` and `codegen-units = 1`, with symbols stripped.
- Compare `opt-level = 3` with `"s"`. In the spike, `"s"` cut the `.so` from 16.2 MB to 14.1 MB, about 1.5 MB less to download. Keep `"s"` only if typing and parsing stay within budget.
- Run `cargo bloat --crates` on the Android target. Look for iroh, reqwest or tokio features the app doesn't use. Remove them only where the change is contained, and never by forking.

**ABIs:** release builds ship `arm64-v8a` only (minSdk is 36; see step 7).

**Resources:** remove unused template resources, such as the purple and teal colours and the template launcher art, and add `resourceConfigurations` for the languages the app ships.

## Speed

- **Before measuring**, check that `Application.onCreate` does only three things: `AndroidContext.install`, `Core.start` and `initialize`. The storage thread opens the database off the main thread.
- **Baseline profiles:** add a `baseline-prof.txt` generated with Macrobenchmark only if cold start misses its budget. It costs a small `profileinstaller` dependency and a benchmark module, and a Views app gains less from it than a Compose app.
- **16 KB pages:** check the final `.so` and APK alignment as in step 7.

## Signing and versions

- Keep the release keystore outside the repository. Read its path and passwords from a `keystore.properties` file that git ignores, or from environment variables in CI.
- Take `versionName` from the workspace version in `Cargo.toml`, so the Linux and Android apps share version numbers. Derive `versionCode` from it, for example `major * 10000 + minor * 100 + patch`.

## Distribution

Decide this when there's a build worth sharing:

| Channel | Notes |
| --- | --- |
| GitHub releases | Simplest: attach the signed APK next to the `.deb`. |
| F-Droid | Fits the app's philosophy. It builds Rust from source, so the build needs to be reproducible and work from the command line (step 7 does this). |
| Google Play | Needs a developer account, the 16 KB page check, and a Data safety form. The app collects nothing, and sync is peer-to-peer. |

## Test matrix

Run before each release:

- at least one physical arm64 phone on the oldest supported Android version, and one on the newest
- the x86_64 emulator
- a database seeded with `crates/core/examples/seed_demo.rs`, plus a 50,000-character note
- steps 8 to 10's instrumented tests, and step 10's manual interop list with the current Linux release

## Docs

- Add an **Android** section to the root README: building, installing, where data lives on the phone, and what's different (sync runs while the app is open).
- Mark every doc in this folder **Done**, and move anything still open into issues.

## Notes

_Anything surprising goes here._
