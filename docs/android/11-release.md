# 11. Release

**Status:** Done, except the measurements and tests that need a phone, and choosing a distribution channel
**Needs:** steps 9 and 10

## Goal

A release APK that is measurably small and fast, signed, and tested on real devices. The desktop README also documents how to build it.

## Budgets

The spike measured a Motorola Edge 50 Fusion (Android 16) with the `spike-release` library (opt-level 3, fat LTO), R8 off, and 10,000 notes. The budgets leave headroom for the real UI. Measure every release against them, and update the "Spike" column if the reference device changes.

| Metric | How to measure | Spike | 0.2.0 | Budget |
| --- | --- | --- | --- | --- |
| APK download size (arm64) | `apkanalyzer apk download-size` | 8.4 MB | 6.5 MB | ≤ 10 MB |
| APK file size (arm64) | `apkanalyzer apk file-size` | 18.8 MB | 15.0 MB | ≤ 22 MB |
| Rust `.so` size | `ls -l` on the stripped library | 16.2 MB | 14.3 MB | ≤ 18 MB |
| Cold start to note list | `adb shell am start -W`, `TotalTime` | about 190 ms | 205–209 ms with 10,000 notes and sync on | ≤ 300 ms |
| Keystroke to screen | `dumpsys gfxinfo` while typing near the top of a long note | — | median 6–7 ms, 99th percentile 11–20 ms, at most 1 janky frame | no dropped frames |
| Open a note | main-thread time sampled with simpleperf | — | 7–15 ms | < 100 ms |
| Memory, idle in editor | `adb shell dumpsys meminfo`, total PSS | not measured | 107 MB (110 MB with a 100 KB note open) | ≤ 150 MB |

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

- **R8 is on for release.** JNA refers to AWT classes Android lacks, so `rules.keep` adds `-dontwarn java.awt.**` next to the JNA, bindings and `AndroidContext` keep rules. The dex is 398 KB.
  - An R8 build was checked on the emulator: notes, Markdown styling, the sync screen and a pairing code.
  - `-PreleaseAbis=x86_64` builds a release an emulator can run. Release builds are otherwise arm64 only.
- **`opt-level = "s"`** is now part of the `android` profile. Host benchmark (`crates/core/examples/benchmark.rs`, now with a typing measurement), size against speed:

  | | opt-level 3 | "s" |
  | --- | --- | --- |
  | Markdown parse, 108 KB note (median) | 1.21 ms | 1.32 ms |
  | Keystroke into that note's draft, UTF-16 (median) | 2.9 µs | 3.3 µs |
  | List and search 10,000 notes | within noise | within noise |
  | `.so` / APK download | 16.6 MB / 8.0 MB | 14.3 MB / 6.5 MB |

  About 10% slower on work that takes microseconds to low milliseconds, off the frame budget, for 2.3 MB less library. Revisit if the phone shows dropped frames while typing.
- **Version:** `versionName` comes from `[workspace.package] version` in `Cargo.toml` (0.2.0), and `versionCode` from it (200).
- **Signing** reads `android/keystore.properties` (storeFile, storePassword, keyAlias, keyPassword), which git ignores, as it does `*.jks` and `*.keystore`. Without the file, release builds are unsigned.
- **Resources:** `androidResources.localeFilters` keeps English only, and the template resources were removed in step 7.
- **Not done:**
  - `cargo bloat`: the size budget is met, so the tool wasn't installed.
  - A baseline profile: decide once cold start is measured on the phone.
  - A distribution channel.
- **Test matrix so far:** all 14 instrumented tests pass on an x86_64 API 36 emulator with 16 KB pages, and on a Motorola Edge 50 Fusion (arm64, Android 16, 4 KB pages). Run `connectedDebugAndroidTest` on a phone only if its notes don't matter.
- **Phone measurements** used the release build signed with the debug key, which is `profileable` so `simpleperf` and `gfxinfo` can measure it:
  - Keystrokes were measured on a 50,000-character note with every line formatted (0 janky frames), and on this folder's docs as one 100 KB note (1 janky frame out of 25–53).
  - Opening a note costs the main thread 7–15 ms. The first frame of the dense 50,000-character note takes about 190 ms to complete; the 100 KB docs note shows no slow frame.
  - Memory is mostly native heap (37 MB: Rust, SQLite and iroh) and graphics buffers (42 MB); the Java heap is 7 MB.
