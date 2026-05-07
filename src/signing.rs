//! Signing helpers for sideload-ready release APKs.
//!
//! Tauri's Android scaffold leaves the `release` buildType unsigned.
//! `adb install` then refuses with INSTALL_PARSE_FAILED_NO_CERTIFICATES.
//! For local testing we sign release builds with the Android debug
//! keystore (the same one Gradle uses for debug builds by default).
//! That's enough to install and run on real devices; it is **not**
//! enough for Play Store distribution — distribution-grade signing
//! requires a real keystore and is a separate feature.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::log::Logger;

/// Patch the just-generated `app/build.gradle.kts` to sign the release
/// buildType with the debug keystore. Idempotent on re-run because we
/// detect prior patches and skip them; safe across `cargo tauri android
/// init` regenerating the file (it's regenerated each build, so we
/// re-patch each build).
pub fn patch_android_release_signing(project_dir: &Path, log: &Logger) -> Result<()> {
    let keystore = ensure_debug_keystore(log)?;
    let gradle = project_dir
        .join("src-tauri")
        .join("gen")
        .join("android")
        .join("app")
        .join("build.gradle.kts");
    if !gradle.exists() {
        bail!(
            "expected Tauri-generated Gradle script at {} (did `cargo tauri android init` succeed?)",
            gradle.display()
        );
    }

    let original = std::fs::read_to_string(&gradle)
        .with_context(|| format!("read {}", gradle.display()))?;
    if original.contains("// tau:debug-signed-release") {
        return Ok(()); // already patched
    }

    // Inject a `signingConfigs` block at the top of the `android { ... }`
    // body, then attach it to the release buildType. Both edits are
    // string-anchored on the literal Tauri scaffold output, which is
    // stable across Tauri 2 minor versions; if Tauri changes the
    // template materially the assertions below will catch it.
    let signing_block = format!(
        r#"
    // tau:debug-signed-release
    signingConfigs {{
        create("debugKey") {{
            storeFile = file({key:?})
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }}
    }}
"#,
        key = keystore.display().to_string()
    );

    // Anchor 1: insert the signingConfigs block right after `android {`.
    let android_open = "android {";
    let Some(idx) = original.find(android_open) else {
        bail!(
            "could not find `android {{` in {} — Tauri scaffold layout changed?",
            gradle.display()
        );
    };
    let insert_at = idx + android_open.len();
    let mut patched = String::with_capacity(original.len() + signing_block.len() + 64);
    patched.push_str(&original[..insert_at]);
    patched.push_str(&signing_block);
    patched.push_str(&original[insert_at..]);

    // Anchor 2: add `signingConfig = ...` inside the release block.
    let release_open = "getByName(\"release\") {";
    let Some(rel_idx) = patched.find(release_open) else {
        bail!(
            "could not find release buildType in {} — Tauri scaffold layout changed?",
            gradle.display()
        );
    };
    let rel_insert_at = rel_idx + release_open.len();
    let release_inject =
        "\n            signingConfig = signingConfigs.getByName(\"debugKey\")\n";
    let mut final_text = String::with_capacity(patched.len() + release_inject.len());
    final_text.push_str(&patched[..rel_insert_at]);
    final_text.push_str(release_inject);
    final_text.push_str(&patched[rel_insert_at..]);

    std::fs::write(&gradle, &final_text)
        .with_context(|| format!("write patched {}", gradle.display()))?;
    log.detail("signing", "release APK signed with Android debug keystore");
    Ok(())
}

/// Return the path to the Android debug keystore, generating it if
/// missing. The keystore lives at `~/.android/debug.keystore` — the
/// same well-known location Gradle's Android plugin uses, with the
/// well-known password `android` / alias `androiddebugkey`.
fn ensure_debug_keystore(log: &Logger) -> Result<std::path::PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    let android_dir = Path::new(&home).join(".android");
    let keystore = android_dir.join("debug.keystore");

    if keystore.exists() {
        return Ok(keystore);
    }

    log.step(&format!("creating Android debug keystore at {}", keystore.display()));
    std::fs::create_dir_all(&android_dir)
        .with_context(|| format!("create {}", android_dir.display()))?;

    let status = Command::new("keytool")
        .args([
            "-genkeypair",
            "-v",
            "-keystore",
        ])
        .arg(&keystore)
        .args([
            "-storepass", "android",
            "-alias", "androiddebugkey",
            "-keypass", "android",
            "-keyalg", "RSA",
            "-keysize", "2048",
            "-validity", "10000",
            "-dname", "CN=Android Debug,O=Android,C=US",
        ])
        .status()
        .context("failed to spawn keytool — is the JDK installed?")?;
    if !status.success() {
        bail!("keytool exited with status {}", status);
    }
    Ok(keystore)
}
