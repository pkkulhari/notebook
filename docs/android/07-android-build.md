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

- AGP 9 builds Kotlin itself, so check that a first `.kt` file compiles without adding `org.jetbrains.kotlin.android`. If it doesn't, add the plugin through the version catalog.
- Set the Java/Kotlin target to 17.

### 2. Dependencies

Keep the list short. Step 8 decides the UI dependencies.

| Dependency | Why |
| --- | --- |
| `net.java.dev.jna:jna:<version>@aar` | Needed at runtime by UniFFI's Kotlin bindings |
| `org.rustls:rustls-platform-verifier` | Kotlin half of the TLS verifier (step 6); its version is read from `Cargo.lock` |

Remove `androidx.appcompat` and `com.google.android.material` if step 8 goes framework-only.

The verifier is published in a Maven repository on GitHub. Because `settings.gradle.kts` uses `RepositoriesMode.FAIL_ON_PROJECT_REPOS`, declare the repository there, not in the app module:

```kotlin
dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
        maven("https://github.com/rustls/rustls-platform-verifier/raw/maven-archive/android-release-support/maven/")
    }
}
```

Read the version from the `rustls-platform-verifier-android` entry in `../Cargo.lock`. The crate's README has a configuration-cache-friendly `ValueSource` for this. That way, a `cargo update` can never leave the Kotlin and Rust halves mismatched.

### 3. Rust build tasks

Put these in `app/build.gradle.kts`, or in a small `buildSrc` convention plugin if it grows.

| Task | Command | Output |
| --- | --- | --- |
| `cargoBuildDebug` | `cargo ndk -t arm64-v8a -t x86_64 --platform <minSdk> -o build/rustJniLibs/debug build -p notebook-ffi` | `.so` per ABI |
| `cargoBuildRelease` | `cargo ndk -t arm64-v8a --platform <minSdk> -o build/rustJniLibs/release build -p notebook-ffi --profile android` | `.so` for arm64 only |
| `uniffiBindings` | `cargo build -p notebook-ffi`, then `cargo run -p notebook-ffi --bin uniffi-bindgen -- generate --library ../target/debug/libnotebook_ffi.so --language kotlin --out-dir build/generated/uniffi` | Kotlin sources |

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

- Keep `panic = "unwind"`, because UniFFI turns Rust panics into Kotlin exceptions.
- Step 11 decides whether `opt-level = "s"` is worth its speed cost.

### 5. 16 KB pages

Devices on Android 15 and later may use 16 KB memory pages, and Google Play requires 16 KB-aligned native libraries.

- NDK r28+ links with 16 KB alignment by default. On older NDKs, add `-C link-arg=-Wl,-z,max-page-size=16384`.
- Check the release `.so` with `llvm-readelf -l` (every `LOAD` segment should have `Align 0x4000`) and the APK with `zipalign -c -P 16 -v 4`.

### 6. R8 keep rules

Add these to `app/src/main/keepRules/rules.keep` now, even though R8 stays off until step 11:

```
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keep class com.pkkulhari.notebook.core.** { *; }
-keep, includedescriptorclasses class org.rustls.platformverifier.** { *; }
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
