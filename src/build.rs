//! Drive `cargo tauri ...` for each platform and collect the resulting
//! artifacts into the user's output directory.
//!
//! All builds share a single `CARGO_TARGET_DIR` (see `cache::dir`) — without
//! it every wrap would recompile Tauri from scratch, since the scaffold lives
//! in a fresh tempdir each time.

use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use walkdir::WalkDir;

use crate::cache;
use crate::config::{ArtifactPolicy, Config, Platform};
use crate::log::Logger;

/// Ensure rustup targets needed for `platform` are installed.
pub fn ensure_targets(platform: Platform, log: &Logger) -> Result<()> {
    let targets = platform.spec().rustup_targets;
    if targets.is_empty() {
        return Ok(());
    }
    let installed = installed_targets()?;
    for t in targets {
        if installed.iter().any(|i| i == t) {
            continue;
        }
        log.step(&format!("rustup target add {}", t));
        let status = Command::new("rustup")
            .args(["target", "add", t])
            .status()
            .context("failed to spawn rustup")?;
        if !status.success() {
            bail!("rustup target add {} failed", t);
        }
    }
    Ok(())
}

fn installed_targets() -> Result<Vec<String>> {
    let out = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .context("rustup not found in PATH")?;
    if !out.status.success() {
        bail!("rustup target list failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// Run the Tauri build for `platform` inside `project_dir`. Returns the root
/// directory where the platform's bundles can be found.
pub fn build_platform(
    project_dir: &Path,
    platform: Platform,
    cfg: &Config,
    log: &Logger,
) -> Result<PathBuf> {
    let target_dir = cache::dir()?;
    log.detail("cache", &target_dir.display().to_string());

    let tauri = TauriCmd::new(project_dir, &target_dir, log);
    match MobileFlavor::from_platform(platform) {
        None => {
            tauri.build_desktop(cfg)?;
            Ok(target_dir)
        }
        Some(flavor) => {
            tauri.build_mobile(cfg, flavor)?;
            Ok(flavor.gen_dir(project_dir))
        }
    }
}

/// Builder for `cargo tauri ...` invocations rooted in the scaffold tempdir.
pub(crate) struct TauriCmd<'a> {
    project_dir: &'a Path,
    target_dir: &'a Path,
    log: &'a Logger,
}

impl<'a> TauriCmd<'a> {
    pub(crate) fn new(project_dir: &'a Path, target_dir: &'a Path, log: &'a Logger) -> Self {
        Self { project_dir, target_dir, log }
    }

    fn cargo(&self) -> Command {
        let mut cmd = Command::new("cargo");
        cmd.current_dir(self.project_dir)
            .env("CARGO_TARGET_DIR", self.target_dir);
        cmd
    }

    fn run(&self, mut cmd: Command, label: &str) -> Result<()> {
        self.log.command(label);
        let status = cmd.status().with_context(|| format!("failed to spawn: {}", label))?;
        if !status.success() {
            return Err(anyhow!("{} exited with status {}", label, status));
        }
        Ok(())
    }

    fn build_desktop(&self, cfg: &Config) -> Result<()> {
        let mut cmd = self.cargo();
        cmd.args(["tauri", "build"]);
        if !cfg.profile.is_release() {
            cmd.arg("--debug");
        }
        self.run(cmd, "cargo tauri build")
    }

    fn build_mobile(&self, cfg: &Config, flavor: MobileFlavor) -> Result<()> {
        let sub = flavor.subcommand();

        let mut init = self.cargo();
        init.args(["tauri", sub, "init"]);
        self.run(init, &format!("cargo tauri {} init", sub))?;

        // Tauri's generated Android Gradle script leaves the `release`
        // buildType unsigned. Without a signature `adb install` rejects
        // the APK ("Failed to collect certificates"). For local sideload
        // testing we sign the release build with the standard Android
        // debug keystore — same way debug builds are auto-signed. Real
        // distribution still needs a proper keystore (a future feature).
        if matches!(flavor, MobileFlavor::Android) && cfg.profile.is_release() {
            crate::signing::patch_android_release_signing(self.project_dir, self.log)?;
        }

        let mut build = self.cargo();
        build.args(["tauri", sub, "build"]);
        if !cfg.profile.is_release() {
            build.arg("--debug");
        }
        self.run(build, &format!("cargo tauri {} build", sub))
    }

    /// Spawn `cargo tauri dev` and return the child handle so the caller
    /// can run a filesystem watcher (or any other concurrent work)
    /// alongside the long-lived dev process. Stdio is inherited.
    pub(crate) fn spawn_dev_desktop(&self) -> Result<Child> {
        let mut cmd = self.cargo();
        cmd.args(["tauri", "dev"]);
        self.log.command("cargo tauri dev");
        cmd.spawn().with_context(|| "failed to spawn: cargo tauri dev".to_string())
    }

    /// Same as `spawn_dev_desktop` but for mobile. The synchronous `init`
    /// step still runs to completion before returning the spawned dev child.
    pub(crate) fn spawn_dev_mobile(&self, flavor: MobileFlavor) -> Result<Child> {
        let sub = flavor.subcommand();

        let mut init = self.cargo();
        init.args(["tauri", sub, "init"]);
        self.run(init, &format!("cargo tauri {} init", sub))?;

        let mut dev = self.cargo();
        dev.args(["tauri", sub, "dev"]);
        let label = format!("cargo tauri {} dev", sub);
        self.log.command(&label);
        dev.spawn().with_context(|| format!("failed to spawn: {}", label))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum MobileFlavor {
    Android,
    Ios,
}

impl MobileFlavor {
    pub(crate) fn from_platform(p: Platform) -> Option<Self> {
        match p {
            Platform::Android => Some(MobileFlavor::Android),
            Platform::Ios => Some(MobileFlavor::Ios),
            _ => None,
        }
    }

    pub(crate) fn subcommand(self) -> &'static str {
        match self {
            MobileFlavor::Android => "android",
            MobileFlavor::Ios => "ios",
        }
    }

    /// Where Tauri writes the generated platform project, relative to scaffold.
    fn gen_dir(self, project_dir: &Path) -> PathBuf {
        let leaf = match self {
            MobileFlavor::Android => "android",
            MobileFlavor::Ios => "apple",
        };
        project_dir.join("src-tauri").join("gen").join(leaf)
    }
}

/// Walk the platform's build output and copy final artifacts into `output_dir`.
pub fn extract_artifacts(
    artifacts_root: &Path,
    output_dir: &Path,
    cfg: &Config,
    platform: Platform,
) -> Result<Vec<PathBuf>> {
    let spec = platform.spec();
    match spec.artifact_policy {
        ArtifactPolicy::FilterByProductName => {
            let variants = product_name_variants(&cfg.name);
            let output_dir = output_dir.to_path_buf();
            copy_matching(artifacts_root, spec.artifact_exts, cfg.profile.dir_name(), |path, file_name| {
                let lower = file_name.to_ascii_lowercase();
                if !variants.iter().any(|v| lower.starts_with(v.as_str())) {
                    return None;
                }
                Some(output_dir.join(path.file_name().unwrap()))
            })
        }
        ArtifactPolicy::RenameBySlug => {
            let slug = product_slug(&cfg.name);
            let output_dir = output_dir.to_path_buf();
            copy_matching(artifacts_root, spec.artifact_exts, cfg.profile.dir_name(), |path, _file_name| {
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                Some(output_dir.join(format!("{}.{}", slug, ext)))
            })
        }
    }
}

/// Walk `root`, find files (or `.app` bundles which are dirs) whose extension
/// matches `exts` and whose path passes through a directory component named
/// `profile_dir`, hand them to `dest_for` which returns the destination path
/// or None to skip, and copy. Deduplicates by destination.
fn copy_matching(
    root: &Path,
    exts: &[&str],
    profile_dir: &str,
    mut dest_for: impl FnMut(&Path, &str) -> Option<PathBuf>,
) -> Result<Vec<PathBuf>> {
    let mut copied = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !has_matching_extension(path, exts) {
            continue;
        }
        if !path_passes_through(path, profile_dir) {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let Some(dest) = dest_for(path, file_name) else {
            continue;
        };
        if !seen.insert(dest.clone()) {
            continue;
        }
        if path.is_dir() {
            copy_dir_recursive(path, &dest)
                .with_context(|| format!("copy dir {} -> {}", path.display(), dest.display()))?;
        } else {
            std::fs::copy(path, &dest)
                .with_context(|| format!("copy {} -> {}", path.display(), dest.display()))?;
        }
        copied.push(dest);
    }

    Ok(copied)
}

fn has_matching_extension(path: &Path, exts: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| exts.iter().any(|e| e.eq_ignore_ascii_case(ext)))
}

/// True if `path` has a directory component literally equal to `name`.
/// Used to filter `target/<triple?>/{debug,release}/...` artifacts by profile.
fn path_passes_through(path: &Path, name: &str) -> bool {
    path.components()
        .any(|c| c.as_os_str().to_string_lossy() == name)
}

fn product_name_variants(name: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    vec![lower.clone(), lower.replace(' ', "-"), lower.replace(' ', "_")]
}

fn product_slug(name: &str) -> String {
    let raw: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    raw.trim_matches('-').to_string()
}

fn copy_dir_recursive(from: &Path, to: &Path) -> Result<()> {
    if to.exists() {
        std::fs::remove_dir_all(to)?;
    }
    std::fs::create_dir_all(to)?;
    for entry in WalkDir::new(from).into_iter().filter_map(|e| e.ok()) {
        let rel = entry.path().strip_prefix(from)?;
        let dest = to.join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&dest)?;
        } else if entry.file_type().is_file() {
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passes_through_profile_component() {
        assert!(path_passes_through(Path::new("/x/target/debug/foo"), "debug"));
        assert!(path_passes_through(
            Path::new("/x/target/aarch64-apple-darwin/release/foo"),
            "release"
        ));
        assert!(!path_passes_through(Path::new("/x/target/debug/foo"), "release"));
    }

    #[test]
    fn product_name_variants_covers_separators() {
        let v = product_name_variants("My App");
        assert!(v.contains(&"my app".to_string()));
        assert!(v.contains(&"my-app".to_string()));
        assert!(v.contains(&"my_app".to_string()));
    }

    #[test]
    fn product_slug_strips_specials() {
        assert_eq!(product_slug("My App!"), "my-app");
        assert_eq!(product_slug("hello"), "hello");
    }

    #[test]
    fn mobile_flavor_only_for_mobile_platforms() {
        assert!(MobileFlavor::from_platform(Platform::Android).is_some());
        assert!(MobileFlavor::from_platform(Platform::Ios).is_some());
        assert!(MobileFlavor::from_platform(Platform::Macos).is_none());
        assert!(MobileFlavor::from_platform(Platform::Windows).is_none());
        assert!(MobileFlavor::from_platform(Platform::Linux).is_none());
    }
}
