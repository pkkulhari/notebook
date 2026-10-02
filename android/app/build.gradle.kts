import java.util.Properties
import javax.inject.Inject

plugins {
    alias(libs.plugins.android.application)
}

/** The Cargo workspace, one level above this Gradle project. */
val workspaceDir: File = rootDir.parentFile

/** The Linux and Android apps share the workspace's version number. */
val workspaceVersion: String = Regex("\\[workspace\\.package][^\\[]*?\\bversion = \"([^\"]+)\"")
    .find(File(workspaceDir, "Cargo.toml").readText())
    ?.groupValues?.get(1)
    ?: error("No [workspace.package] version in Cargo.toml")

/** 1.2.3 is 10203, so every release counts up. */
val versionNumber: Int = workspaceVersion.split(".").map(String::toInt).let { (major, minor, patch) ->
    major * 10_000 + minor * 100 + patch
}

/** Release builds ship arm64 only; `-PreleaseAbis=x86_64` makes one an emulator can run. */
val releaseAbis: List<String> = providers.gradleProperty("releaseAbis").orNull?.split(",") ?: listOf("arm64-v8a")

/**
 * The release key, from a `keystore.properties` beside this project that git
 * ignores: storeFile, storePassword, keyAlias and keyPassword. Without it,
 * release builds are unsigned.
 */
val keystore: Properties? = rootProject.file("keystore.properties").takeIf { it.exists() }?.let { file ->
    Properties().apply { file.inputStream().use(::load) }
}

android {
    namespace = "com.pkkulhari.notebook"
    compileSdk {
        version = release(37)
    }
    // NDK r29 ships API levels up to 35; the Rust library is built with -P 35,
    // which is fine because it only has to be at or below minSdk.
    ndkVersion = "29.0.14206865"

    defaultConfig {
        applicationId = "com.pkkulhari.notebook"
        minSdk = 36
        targetSdk = 37
        versionCode = versionNumber
        versionName = workspaceVersion

        // Runs tests against a throwaway database, never the real notes.
        testInstrumentationRunner = "com.pkkulhari.notebook.TestRunner"
    }

    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = rootProject.file(keystore.getProperty("storeFile"))
                storePassword = keystore.getProperty("storePassword")
                keyAlias = keystore.getProperty("keyAlias")
                keyPassword = keystore.getProperty("keyPassword")
            }
        }
    }

    buildTypes {
        debug {
            ndk {
                abiFilters += listOf("arm64-v8a", "x86_64")
            }
        }
        release {
            // R8 shrinks the Kotlin and JNA classes; keepRules/rules.keep
            // keeps what JNA and the bindings find by reflection.
            optimization {
                enable = true
            }
            ndk {
                abiFilters += releaseAbis
            }
            signingConfig = signingConfigs.findByName("release")
        }
    }

    androidResources {
        // The app is in English; this drops the platform libraries' other languages.
        localeFilters += "en"
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

/** Inputs every Rust task shares: a change anywhere in the workspace's crates. */
fun rustSources(): FileTree = fileTree(workspaceDir) {
    include("crates/**/*.rs", "crates/**/Cargo.toml", "crates/**/uniffi.toml", "Cargo.toml", "Cargo.lock")
}

/** `cargo` from PATH, or from rustup's default location, which apps started from a desktop launcher often lack on PATH. */
val cargoPath: String = listOfNotNull(System.getenv("CARGO"), "${System.getProperty("user.home")}/.cargo/bin/cargo")
    .firstOrNull { File(it).canExecute() } ?: "cargo"

/** Cross-compiles notebook-ffi with cargo-ndk, and keeps only its library. */
abstract class CargoNdkBuild : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val sources: ConfigurableFileCollection

    @get:Input abstract val cargo: Property<String>
    @get:Input abstract val workspace: Property<String>
    @get:Input abstract val ndkDirectory: Property<String>

    /** "dev" or a profile from the workspace's Cargo.toml. */
    @get:Input abstract val profile: Property<String>
    @get:Input abstract val abis: ListProperty<String>
    @get:Input abstract val features: ListProperty<String>

    @get:OutputDirectory abstract val outputDirectory: DirectoryProperty

    @get:Inject abstract val exec: ExecOperations
    @get:Inject abstract val files: FileSystemOperations

    @TaskAction
    fun build() {
        val abis = abis.get()
        exec.exec {
            workingDir = File(workspace.get())
            environment("ANDROID_NDK_HOME", ndkDirectory.get())
            commandLine(
                listOf(cargo.get(), "ndk") + abis.flatMap { listOf("-t", it) } +
                    // cargo-ndk defaults to API 21.
                    listOf("-P", "35", "build", "-p", "notebook-ffi", "--lib", "--profile", profile.get()) +
                    features.get().flatMap { listOf("--features", it) },
            )
        }
        // cargo-ndk's own -o would also copy iroh's cdylibs.
        val directory = if (profile.get() == "dev") "debug" else profile.get()
        files.sync {
            for (abi in abis) {
                from("${workspace.get()}/target/${TRIPLES.getValue(abi)}/$directory") {
                    include("libnotebook_ffi.so")
                    into(abi)
                }
            }
            into(outputDirectory)
        }
    }

    companion object {
        val TRIPLES = mapOf("arm64-v8a" to "aarch64-linux-android", "x86_64" to "x86_64-linux-android")
    }
}

/** Generates the Kotlin bindings from the host build of notebook-ffi; the API is the same on every target. */
abstract class UniffiBindings : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val sources: ConfigurableFileCollection

    @get:Input abstract val cargo: Property<String>
    @get:Input abstract val workspace: Property<String>

    @get:OutputDirectory abstract val outputDirectory: DirectoryProperty

    @get:Inject abstract val exec: ExecOperations
    @get:Inject abstract val files: FileSystemOperations

    @TaskAction
    fun generate() {
        val workspace = File(workspace.get())
        exec.exec {
            workingDir = workspace
            commandLine(cargo.get(), "build", "-p", "notebook-ffi")
        }
        files.delete { delete(outputDirectory) }
        exec.exec {
            workingDir = workspace
            commandLine(
                "target/debug/uniffi-bindgen", "generate", "target/debug/libnotebook_ffi.so",
                "--language", "kotlin", "--out-dir", outputDirectory.get().asFile.path, "--no-format",
            )
        }
    }
}

val bindings = tasks.register<UniffiBindings>("uniffiBindings") {
    sources.from(rustSources())
    cargo.set(cargoPath)
    workspace.set(workspaceDir.path)
    outputDirectory.set(layout.buildDirectory.dir("generated/uniffi"))
}

androidComponents {
    onVariants { variant ->
        val release = variant.buildType == "release"
        val rust = tasks.register<CargoNdkBuild>("cargoBuild${variant.name.replaceFirstChar(Char::uppercase)}") {
            sources.from(rustSources())
            cargo.set(cargoPath)
            workspace.set(workspaceDir.path)
            ndkDirectory.set(androidComponents.sdkComponents.ndkDirectory.map { it.asFile.path })
            profile.set(if (release) "android" else "dev")
            abis.set(if (release) releaseAbis else listOf("arm64-v8a", "x86_64"))
            // Rust logs and panics in logcat, for debug builds only.
            features.set(if (release) emptyList() else listOf("logcat"))
            outputDirectory.set(layout.buildDirectory.dir("rustJniLibs/${variant.name}"))
        }
        variant.sources.jniLibs?.addGeneratedSourceDirectory(rust, CargoNdkBuild::outputDirectory)
        variant.sources.kotlin?.addGeneratedSourceDirectory(bindings, UniffiBindings::outputDirectory)
    }
}

dependencies {
    // UniFFI's generated Kotlin calls into Rust through JNA.
    implementation(libs.jna) {
        artifact { type = "aar" }
    }
    androidTestImplementation(libs.androidx.junit)
    androidTestImplementation(libs.androidx.test.runner)
}
