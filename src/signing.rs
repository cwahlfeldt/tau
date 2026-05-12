//! Android keystore management for release APK signing.
//!
//! Android refuses to install any APK that isn't signed (v1/v2/v3), even for
//! sideloaded testing. By default `cargo tauri android build` (release)
//! emits an unsigned APK and Gradle never gets told what key to use.
//!
//! We solve this in two layers:
//!
//! 1. If the user gave us a keystore via `tau.conf.json`'s `signing` block,
//!    use it as-is.
//! 2. Otherwise auto-generate (once) a debug keystore in the user's cache
//!    dir and reuse it for every release build. This is the same trick
//!    Android Studio uses for local debug installs — not Play-Store-valid,
//!    but sufficient for `adb install`.
//!
//! Either way, we inject the keystore into the Gradle project by writing a
//! `keystore.properties` file at the gen root and patching `app/build.gradle.kts`
//! to read it in a `signingConfigs.release` block and wire it to
//! `buildTypes.release.signingConfig`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cache;
use crate::config::SigningConfig;
use crate::log::Logger;

/// Resolved signing material for a single build. All fields are required
/// (we always sign on Android release — there is no unsigned path).
pub struct KeystoreInfo {
    pub path: PathBuf,
    pub alias: String,
    pub store_password: String,
    pub key_password: String,
}

/// Standard Android debug keystore values. Matches what Android Studio
/// generates at `~/.android/debug.keystore` so devices that have trusted
/// a debug build before will accept ours too.
const DEBUG_ALIAS: &str = "androiddebugkey";
const DEBUG_PASSWORD: &str = "android";

/// Pick the keystore to sign Android release builds with.
///
/// Priority: explicit user config > auto-generated debug keystore.
/// The debug keystore is created on first use and reused thereafter.
pub fn resolve_android_keystore(
    signing: Option<&SigningConfig>,
    log: &Logger,
) -> Result<KeystoreInfo> {
    if let Some(cfg) = signing {
        if let Some(path) = &cfg.android_keystore {
            return user_keystore(cfg, path);
        }
    }
    debug_keystore(log)
}

fn user_keystore(cfg: &SigningConfig, path: &Path) -> Result<KeystoreInfo> {
    if !path.exists() {
        bail!(
            "signing.android_keystore points at {} but the file does not exist",
            path.display()
        );
    }
    let alias = cfg
        .android_key_alias
        .clone()
        .context("signing.android_key_alias is required when android_keystore is set")?;
    let store_password = cfg
        .android_keystore_password
        .clone()
        .context("signing.android_keystore_password is required when android_keystore is set")?;
    // It's common to use the same password for both store and key; fall back if absent.
    let key_password = cfg
        .android_key_password
        .clone()
        .unwrap_or_else(|| store_password.clone());
    Ok(KeystoreInfo {
        path: path.to_path_buf(),
        alias,
        store_password,
        key_password,
    })
}

fn debug_keystore(log: &Logger) -> Result<KeystoreInfo> {
    let path = debug_keystore_path()?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create dir {}", parent.display()))?;
        }
        generate_debug_keystore(&path, log)?;
    }
    Ok(KeystoreInfo {
        path,
        alias: DEBUG_ALIAS.to_string(),
        store_password: DEBUG_PASSWORD.to_string(),
        key_password: DEBUG_PASSWORD.to_string(),
    })
}

fn debug_keystore_path() -> Result<PathBuf> {
    Ok(cache::base()?.join("tau").join("keystores").join("debug.keystore"))
}

fn generate_debug_keystore(path: &Path, log: &Logger) -> Result<()> {
    log.step(&format!("generating debug keystore at {}", path.display()));
    let status = Command::new("keytool")
        .args([
            "-genkey",
            "-v",
            "-keystore",
        ])
        .arg(path)
        .args([
            "-alias",
            DEBUG_ALIAS,
            "-keyalg",
            "RSA",
            "-keysize",
            "2048",
            "-validity",
            "10000",
            "-storepass",
            DEBUG_PASSWORD,
            "-keypass",
            DEBUG_PASSWORD,
            "-dname",
            "CN=tau debug, OU=tau, O=tau, L=Unknown, S=Unknown, C=US",
        ])
        .status()
        .context(
            "failed to spawn `keytool` — install a JDK (it ships with the Android SDK's JDK too)",
        )?;
    if !status.success() {
        bail!("keytool exited with status {}", status);
    }
    Ok(())
}

/// Wire `keystore` into the Tauri-generated Android Gradle project at
/// `gen_android_dir`. Writes `keystore.properties` next to `build.gradle.kts`
/// and edits `app/build.gradle.kts` to add a `signingConfigs.release` block
/// and assign it to the release build type.
///
/// Idempotent: if `build.gradle.kts` already contains our marker we skip the
/// edit. Safe to call after every `tauri android init`.
pub fn inject_signing(gen_android_dir: &Path, keystore: &KeystoreInfo) -> Result<()> {
    write_keystore_properties(gen_android_dir, keystore)?;
    patch_app_build_gradle(gen_android_dir)?;
    Ok(())
}

fn write_keystore_properties(gen_android_dir: &Path, keystore: &KeystoreInfo) -> Result<()> {
    let path = gen_android_dir.join("keystore.properties");
    let content = format!(
        "storePassword={}\nkeyPassword={}\nkeyAlias={}\nstoreFile={}\n",
        keystore.store_password,
        keystore.key_password,
        keystore.alias,
        keystore.path.display(),
    );
    std::fs::write(&path, content)
        .with_context(|| format!("write {}", path.display()))
}

const MARKER: &str = "// tau: signing injected";

fn patch_app_build_gradle(gen_android_dir: &Path) -> Result<()> {
    let path = gen_android_dir.join("app").join("build.gradle.kts");
    let original = std::fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;

    if original.contains(MARKER) {
        return Ok(());
    }

    let patched = patch_gradle_source(&original)?;
    std::fs::write(&path, patched)
        .with_context(|| format!("write {}", path.display()))
}

/// Insert the signingConfigs + release-signingConfig wiring into a Kotlin
/// Gradle script. Pure string transform so it's unit-testable.
///
/// Kotlin script files require all `import` declarations come before any
/// non-import statements. The Tauri template already opens with
/// `import java.util.Properties` so we:
///
///   1. Find the end of the import region (last `import ...` line).
///   2. Insert an extra `import java.io.FileInputStream` (idempotent: skip
///      if Tauri ever adds it) and our `keystoreProperties` loader after it,
///      before the existing `plugins { ... }` block.
///   3. Inside the `android { ... }` block, prepend a `signingConfigs.create("release")`
///      sub-block.
///   4. Inside `buildTypes.getByName("release") { ... }`, inject
///      `signingConfig = signingConfigs.getByName("release")`.
fn patch_gradle_source(original: &str) -> Result<String> {
    let import_end = find_import_region_end(original)
        .context("could not find any `import` line in app/build.gradle.kts — Tauri template changed?")?;

    let mut imports_to_add = String::new();
    if !original.contains("import java.io.FileInputStream") {
        imports_to_add.push_str("import java.io.FileInputStream\n");
    }

    let loader = format!(
        "{marker}\n\
         val tauKeystorePropertiesFile = rootProject.file(\"keystore.properties\")\n\
         val tauKeystoreProperties = Properties().apply {{\n    \
             if (tauKeystorePropertiesFile.exists()) {{\n        \
                 tauKeystorePropertiesFile.inputStream().use {{ load(it) }}\n    \
             }}\n\
         }}\n\n",
        marker = MARKER,
    );

    let signing_block = "\n    signingConfigs {\n        \
        create(\"release\") {\n            \
            keyAlias = tauKeystoreProperties[\"keyAlias\"] as String?\n            \
            keyPassword = tauKeystoreProperties[\"keyPassword\"] as String?\n            \
            storeFile = tauKeystoreProperties[\"storeFile\"]?.let { file(it as String) }\n            \
            storePassword = tauKeystoreProperties[\"storePassword\"] as String?\n        \
        }\n    }\n";

    // Compose: original[..import_end] + new imports + loader + original[import_end..]
    let mut out = String::with_capacity(original.len() + imports_to_add.len() + loader.len() + signing_block.len() + 256);
    out.push_str(&original[..import_end]);
    out.push_str(&imports_to_add);
    // Ensure exactly one blank line between import region and loader.
    if !out.ends_with("\n\n") {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&loader);
    out.push_str(&original[import_end..]);

    // Now insert the signingConfigs block right after `android {`.
    let android_anchor = "android {";
    let idx = out
        .find(android_anchor)
        .context("could not find `android {` block in app/build.gradle.kts — Tauri template changed?")?;
    let insert_after = idx + android_anchor.len();
    out.insert_str(insert_after, signing_block);

    // And the signingConfig assignment inside the release buildType block.
    let release_anchor = "getByName(\"release\")";
    let release_idx = out
        .find(release_anchor)
        .context("could not find release buildType block in app/build.gradle.kts")?;
    let brace_offset = out[release_idx..]
        .find('{')
        .context("malformed release buildType block (no opening brace)")?;
    let insert_at = release_idx + brace_offset + 1;
    out.insert_str(
        insert_at,
        "\n            signingConfig = signingConfigs.getByName(\"release\")",
    );

    Ok(out)
}

/// Return the byte offset *after* the newline of the last `import` line in
/// `src`, or `None` if no imports exist. Kotlin allows imports interspersed
/// with blank lines but not with declarations — we accept either layout by
/// scanning all leading lines and recording the position past the final
/// import.
fn find_import_region_end(src: &str) -> Option<usize> {
    let mut last_end: Option<usize> = None;
    let mut pos = 0;
    for line in src.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("import ") {
            last_end = Some(pos + line.len());
        }
        pos += line.len();
    }
    last_end
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the actual Tauri 2.x `app/build.gradle.kts` shape closely
    /// enough to catch real-world breakage. If Tauri changes its template,
    /// this fixture should be updated to match.
    const TAURI_FIXTURE: &str = r#"import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 36
    namespace = "com.example"
    buildTypes {
        getByName("debug") {
            isMinifyEnabled = false
        }
        getByName("release") {
            isMinifyEnabled = true
        }
    }
}
"#;

    #[test]
    fn patch_inserts_signing_blocks() {
        let out = patch_gradle_source(TAURI_FIXTURE).unwrap();
        assert!(out.contains(MARKER));
        assert!(out.contains("import java.io.FileInputStream"));
        assert!(out.contains("tauKeystorePropertiesFile"));
        assert!(out.contains("signingConfigs {"));
        assert!(out.contains("create(\"release\")"));
        assert!(out.contains("signingConfig = signingConfigs.getByName(\"release\")"));
    }

    #[test]
    fn patch_keeps_imports_above_declarations() {
        // The Kotlin DSL requires every `import` line to come before any
        // non-import statement. The original Tauri template starts with
        // `import java.util.Properties` then has `val tauriProperties = ...`,
        // so our injected loader must land AFTER all imports, not before
        // the existing import. Otherwise gradle fails with
        // "Unresolved reference: import".
        let out = patch_gradle_source(TAURI_FIXTURE).unwrap();
        let last_import_line = out
            .lines()
            .enumerate()
            .filter(|(_, l)| l.trim_start().starts_with("import "))
            .map(|(i, _)| i)
            .max()
            .expect("at least one import");
        let first_non_import = out
            .lines()
            .enumerate()
            .find(|(_, l)| {
                let t = l.trim_start();
                !t.is_empty() && !t.starts_with("import ") && !t.starts_with("//")
            })
            .map(|(i, _)| i)
            .expect("at least one declaration");
        assert!(
            last_import_line < first_non_import,
            "imports must precede declarations: last_import={}, first_decl={}",
            last_import_line,
            first_non_import,
        );
    }

    #[test]
    fn patch_does_not_duplicate_existing_import() {
        let out = patch_gradle_source(TAURI_FIXTURE).unwrap();
        let count = out.matches("import java.util.Properties").count();
        assert_eq!(count, 1, "Properties import duplicated: {}", out);
    }

    #[test]
    fn patch_fails_loudly_if_android_block_missing() {
        let input = "import java.util.Properties\nplugins {}\n";
        assert!(patch_gradle_source(input).is_err());
    }

    #[test]
    fn patch_fails_loudly_if_release_block_missing() {
        let input = "import java.util.Properties\n\nandroid {\n    namespace = \"x\"\n}\n";
        assert!(patch_gradle_source(input).is_err());
    }

    #[test]
    fn patch_fails_loudly_if_no_imports() {
        let input = "android {\n    getByName(\"release\") {}\n}\n";
        assert!(patch_gradle_source(input).is_err());
    }

    #[test]
    fn user_keystore_requires_alias_and_password() {
        let path = std::env::temp_dir().join("tau-test-keystore-stub");
        std::fs::write(&path, b"stub").unwrap();
        let cfg = SigningConfig {
            android_keystore: Some(path.clone()),
            android_keystore_password: None,
            android_key_alias: None,
            android_key_password: None,
            apple_signing_identity: None,
            apple_team_id: None,
        };
        assert!(user_keystore(&cfg, &path).is_err());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn user_keystore_defaults_key_password_to_store_password() {
        let path = std::env::temp_dir().join("tau-test-keystore-stub2");
        std::fs::write(&path, b"stub").unwrap();
        let cfg = SigningConfig {
            android_keystore: Some(path.clone()),
            android_keystore_password: Some("hunter2".into()),
            android_key_alias: Some("a".into()),
            android_key_password: None,
            apple_signing_identity: None,
            apple_team_id: None,
        };
        let info = user_keystore(&cfg, &path).unwrap();
        assert_eq!(info.key_password, "hunter2");
        assert_eq!(info.store_password, "hunter2");
        let _ = std::fs::remove_file(&path);
    }
}
