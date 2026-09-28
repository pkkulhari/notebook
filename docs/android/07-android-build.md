# 7. Android build

**Status:** Not started
**Needs:** step 6 (and step 1's toolchain setup)

## Goal

`./gradlew assembleDebug` in `android/` builds the Rust library for each ABI, generates the Kotlin bindings, and packages both. No manual `cargo` commands are needed. Android Studio's Run button just works.

## Starting point

The existing skeleton in `android/`:

- AGP 9.4.1 and Gradle 9.6, with a version catalog in `gradle/libs.versions.toml`
- `compileSdk` and `targetSdk` 37, `minSdk` 36
- no Kotlin sources yet, and dependencies on AppCompat and Material Components
- R8 is off (`optimization { enable = false }`), and the keep rules are in `app/src/main/keepRules/rules.keep`

## Steps

### 1. Kotlin

- AGP 9's built-in Kotlin compiles `.kt` sources with no Kotlin plugin applied (confirmed in the spike). Don't add `org.jetbrains.kotlin.android`: it's incompatible with AGP 9's new DSL.
- Set the Java/Kotlin target to 17.
- Pin the NDK in `android { ndkVersion = "29.0.14206865" }` so AGP strips with the same NDK that cargo-ndk builds with.

### 2. Dependencies

Keep the list short. Step 8 decides the UI dependencies.

| Dependency | Why |
| --- | --- |
| `net.java.dev.jna:jna:5.19.1`, as an AAR | Needed at runtime by UniFFI's Kotlin bindings |

Add JNA through the version catalog and request the AAR:

```kotlin
implementation(libs.jna) { artifact { type = "aar" } }
```

No TLS verifier artifact is needed. iroh verifies relay certificates with built-in roots (spike Finding 6).

Remove `androidx.appcompat`, `androidx.core` and `com.google.android.material` if step 8 goes framework-only; the spike ran fine without them on a `android:Theme.DeviceDefault.DayNight` theme.

### 3. Rust build tasks

Put these in `app/build.gradle.kts`, or in a small `buildSrc` convention plugin if it grows. `spike/build-android.sh` on `android-spike` is a working shell version of the same steps.

| Task | Command | Output |
| --- | --- | --- |
| `cargoBuildDebug` | `cargo ndk -t arm64-v8a -t x86_64 -P 35 -o build/rustJniLibs/debug build -p notebook-ffi --lib` | `.so` per ABI |
| `cargoBuildRelease` | `cargo ndk -t arm64-v8a -P 35 -o build/rustJniLibs/release build -p notebook-ffi --lib --profile android` | `.so` for arm64 only |
| `uniffiBindings` | `cargo build -p notebook-ffi`, then `target/debug/uniffi-bindgen generate target/debug/libnotebook_ffi.so --language kotlin --out-dir <build>/generated/uniffi --no-format` | Kotlin sources |

Notes on these commands:

- **`-P` is required.** cargo-ndk defaults to API 21. NDK r29's sysroot stops at API 35, and building for 35 is fine because the native API level only has to be at or below minSdk 36.
- **cargo-ndk's `-o` copies every cdylib in the build**, including iroh's and iroh-relay's own. Have the task copy only `libnotebook_ffi.so` into the `jniLibs` directory.
- **Bindgen reads the host library.** The API is the same on every target. Don't pass `--library`: it's deprecated in UniFFI 0.32 and ignored.

Wire them up as follows:

- Run all three tasks from the workspace root, one level above `android/`.
- Declare `crates/**` `.rs` files, the `Cargo.toml` files and `Cargo.lock` as task inputs, and the output directories as outputs. Gradle then skips the tasks when nothing changed, and Cargo's own incremental build handles the rest.
- Register `build/rustJniLibs/<variant>` as that variant's `jniLibs` source directory, and `build/generated/uniffi` as a Kotlin source directory.
- Make `preBuild` depend on `uniffiBindings`, and each variant's native library merge task depend on its `cargoBuild…` task.

### 4. Cargo profile for Android

Add a separate profile so desktop release builds stay quick:

```toml
[profile.android]
inherits = "release"
lto = "fat"
codegen-units = 1
```

- Keep `panic = "unwind"`. UniFFI turns Rust panics into Kotlin exceptions, and iroh's DNS resolver needs unwinding to recover (spike Finding 7).
- In the spike, this profile gave a 16.2 MB stripped `.so`, which built in about 80 s. `opt-level = "s"` gave 14.1 MB; step 11 decides whether that's worth its speed cost.
- cargo-ndk 4 doesn't strip. `strip = true`, inherited from `release`, does it.

### 5. 16 KB pages

Devices on Android 15 and later may use 16 KB memory pages, and Google Play requires 16 KB-aligned native libraries.

- NDK r28+ links with 16 KB alignment by default. The spike's r29 build had `Align 0x4000` on every `LOAD` segment.
- Check the release `.so` with `llvm-readelf -l` and the APK with `zipalign -c -P 16 -v 4`. Both passed in the spike, including JNA's `libjnidispatch.so`.

### 6. R8 keep rules

Add these to `app/src/main/keepRules/rules.keep` now, even though R8 stays off until step 11:

```
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keep class com.pkkulhari.notebook.core.** { *; }
-keep class com.pkkulhari.notebook.AndroidContext { *; }
```

### 7. Smoke test

Add an instrumented test (`androidTest`) that:

- starts `Core` in a temporary directory,
- receives `Ready`,
- inserts text into the draft,
- calls `flush`, receives `Flushed`, and
- restarts `Core` and loads the note with the same text.

Delete the template `ExampleUnitTest.kt` and `ExampleInstrumentedTest.kt`.

## Decision: `minSdk` 36

The app supports Android 16 (API 36) and newer. That means:

- Platform APIs up to API 36 can be used without version checks, including dynamic colours, predictive back (`OnBackInvokedCallback`) and `Theme.DeviceDefault.DayNight`.
- Release builds ship `arm64-v8a` only, which covers the phones this app targets; `x86_64` is for the emulator. Add `armeabi-v7a` only if a real 32-bit Android 16 device turns up.
- The Rust libraries are built with `-P 36` (cargo-ndk defaults to API 21).

## Done when

- A clean checkout runs `./gradlew assembleDebug` with only the SDK, NDK, Rust targets and cargo-ndk installed.
- A second run with no changes skips the Rust tasks.
- Changing a Rust file rebuilds only the `.so` and the bindings.
- The smoke test passes on an arm64 phone and an x86_64 emulator.

## Notes

_Anything surprising goes here._
