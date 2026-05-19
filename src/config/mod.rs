//! Resolved configuration for a single tau run.
//!
//! Three-tier resolution: CLI flags > `tau.conf.json` > defaults.
//! `Config` is the immutable, validated handle that every downstream stage
//! (`scaffold`, `build`) consumes by reference.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

mod platform;

pub use platform::{ArtifactPolicy, Platform};

/// Caller-supplied overrides that win over `tau.conf.json` and defaults.
/// This is the small struct `resolve()` actually needs — much smaller than
/// the full `Cli` it used to take, and free of any CLI coupling.
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub name: Option<String>,
    pub identifier: Option<String>,
    pub output: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub platforms: Vec<String>,
    pub release: bool,
}

pub const DEFAULT_NAME: &str = "WrappedApp";
pub const DEFAULT_VERSION: &str = "0.1.0";
pub const CONFIG_FILE: &str = "tau.conf.json";
const DEFAULT_OUTPUT: &str = "./build";

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub version: String,
    pub identifier: String,
    pub output: String,
    pub platforms: Vec<Platform>,
    pub profile: BuildProfile,
    /// Glob patterns (relative to the source root) of files to exclude when
    /// materializing the frontend tree the bundler embeds. Tauri's
    /// `frontendDist` walks the whole directory, so without this `.git`,
    /// `node_modules`, `build/` outputs, README files etc. all ship inside
    /// the app. `DEFAULT_EXCLUDES` covers the most common footguns;
    /// users add to it via `tau.conf.json`.
    pub exclude: Vec<String>,
    /// Extra Tauri capability permission identifiers appended to the default
    /// cross-platform capability (e.g. `"fs:allow-audio-write-recursive"`,
    /// `"fs:scope-document"`). The defaults — core, fs+appdata, dialog,
    /// notification — are always included; this field adds to them.
    pub permissions: Vec<String>,
    /// Optional signing material from `tau.conf.json`. When `None`, Android
    /// release builds fall back to an auto-generated debug keystore so the
    /// APK is at least installable on physical devices.
    pub signing: Option<SigningConfig>,
}

/// Patterns always excluded from the materialized frontend tree, regardless
/// of user config. The user's `output` dir is added on top of these by
/// `resolve()` (it's only known after config layering).
const DEFAULT_EXCLUDES: &[&str] = &[
    ".git",
    ".git/**",
    ".gitignore",
    ".DS_Store",
    "**/.DS_Store",
    "node_modules",
    "node_modules/**",
    ".claude",
    ".claude/**",
    "tau.conf.json",
];

/// Rust compile profile. Independent of bundle signing — an unsigned
/// release build is valid (it's what you sideload locally), and signing
/// is a separate distribution concern that may also apply to debug builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildProfile {
    Debug,
    Release,
}

impl BuildProfile {
    pub fn is_release(&self) -> bool {
        matches!(self, BuildProfile::Release)
    }

    pub fn dir_name(&self) -> &'static str {
        if self.is_release() { "release" } else { "debug" }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct SigningConfig {
    pub android_keystore: Option<PathBuf>,
    pub android_keystore_password: Option<String>,
    pub android_key_alias: Option<String>,
    pub android_key_password: Option<String>,
    // Apple signing is parsed for forward-compat but not yet wired into the
    // iOS build. `cargo tauri ios build` currently uses Xcode's default
    // signing resolution (free provisioning profile, etc.).
    #[allow(dead_code)]
    pub apple_signing_identity: Option<String>,
    #[allow(dead_code)]
    pub apple_team_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    name: Option<String>,
    version: Option<String>,
    identifier: Option<String>,
    build: Option<BuildSection>,
    signing: Option<SigningConfig>,
    /// User-supplied glob patterns (relative to source root) that are
    /// appended to `DEFAULT_EXCLUDES`.
    exclude: Option<Vec<String>>,
    /// Extra Tauri capability permission identifiers appended to the
    /// default capability. Each entry is a permission string like
    /// `"fs:allow-audio-write-recursive"` or a scope identifier.
    permissions: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BuildSection {
    output: Option<String>,
    platforms: Option<Vec<String>>,
}

/// Resolve the layered config: CLI flags > `tau.conf.json` > defaults.
///
/// `index_dir` is the directory of the input `index.html` for local-file
/// inputs, or `None` for URL inputs. When `--config` isn't given, the
/// sibling `tau.conf.json` next to the index file beats the cwd's — the
/// config travels with the app it configures, so running
/// `tau path/to/some-app/index.html` from another directory still picks
/// up the project's own conf.
pub fn resolve(cwd: &Path, index_dir: Option<&Path>, overrides: &Overrides) -> Result<Config> {
    let file = load_file_config(cwd, index_dir, overrides.config.as_deref())?;
    let build = file.build.unwrap_or_default();

    let name = overrides
        .name
        .clone()
        .or(file.name)
        .unwrap_or_else(|| DEFAULT_NAME.to_string());

    let version = file.version.unwrap_or_else(|| DEFAULT_VERSION.to_string());

    let identifier = overrides
        .identifier
        .clone()
        .or(file.identifier)
        .unwrap_or_else(|| default_identifier(&name));

    let output = overrides
        .output
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .or(build.output)
        .unwrap_or_else(|| DEFAULT_OUTPUT.to_string());

    let platforms = resolve_platforms(&overrides.platforms, build.platforms.as_deref())?;
    let profile = if overrides.release { BuildProfile::Release } else { BuildProfile::Debug };
    let signing = file.signing;

    let mut exclude: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| (*s).to_string()).collect();
    // The user's output dir is one of the most common things to accidentally
    // re-embed (a previous build sitting next to index.html). Auto-exclude
    // it as a relative path; if `output` is absolute or escapes the source
    // tree the matcher just won't match anything, which is the right
    // outcome — no false positives.
    let trimmed = output.trim_start_matches("./");
    if !trimmed.is_empty() && !trimmed.starts_with('/') {
        exclude.push(trimmed.to_string());
        exclude.push(format!("{}/**", trimmed.trim_end_matches('/')));
    }
    if let Some(user) = file.exclude {
        exclude.extend(user);
    }

    let permissions = file.permissions.unwrap_or_default();

    Ok(Config {
        name,
        version,
        identifier,
        output,
        platforms,
        profile,
        exclude,
        permissions,
        signing,
    })
}

fn resolve_platforms(cli_platforms: &[String], file_platforms: Option<&[String]>) -> Result<Vec<Platform>> {
    let source = if !cli_platforms.is_empty() {
        Some(cli_platforms)
    } else {
        file_platforms
    };
    match source {
        Some(list) => list.iter().map(|s| s.parse::<Platform>()).collect(),
        None => Ok(vec![Platform::host()]),
    }
}

fn load_file_config(
    cwd: &Path,
    index_dir: Option<&Path>,
    explicit: Option<&Path>,
) -> Result<FileConfig> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => match discover_config(cwd, index_dir) {
            Some(p) => p,
            None => return Ok(FileConfig::default()),
        },
    };
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config: {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse config: {}", path.display()))
}

/// Search order for `tau.conf.json` when `--config` isn't given:
/// 1. Next to the input `index.html` (most specific to this app).
/// 2. The current working directory (handles the case where the user is
///    already standing in the app's directory).
fn discover_config(cwd: &Path, index_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = index_dir {
        let candidate = dir.join(CONFIG_FILE);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let candidate = cwd.join(CONFIG_FILE);
    if candidate.exists() {
        return Some(candidate);
    }
    None
}

pub fn default_identifier(name: &str) -> String {
    let slug: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let slug = if slug.is_empty() { "app" } else { slug.as_str() };
    format!("com.tau.{}", slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_identifier_slugifies() {
        assert_eq!(default_identifier("My App"), "com.tau.myapp");
        assert_eq!(default_identifier("123!"), "com.tau.123");
        assert_eq!(default_identifier("!!!"), "com.tau.app");
    }

    #[test]
    fn build_profile_dir_name() {
        assert_eq!(BuildProfile::Debug.dir_name(), "debug");
        assert_eq!(BuildProfile::Release.dir_name(), "release");
    }

    #[test]
    fn discover_prefers_index_dir_over_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("cwd");
        let app = tmp.path().join("app");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&app).unwrap();
        std::fs::write(cwd.join(CONFIG_FILE), "{}").unwrap();
        std::fs::write(app.join(CONFIG_FILE), "{}").unwrap();

        assert_eq!(
            discover_config(&cwd, Some(&app)).unwrap(),
            app.join(CONFIG_FILE)
        );
    }

    #[test]
    fn discover_falls_back_to_cwd_when_index_dir_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("cwd");
        let app = tmp.path().join("app");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&app).unwrap();
        std::fs::write(cwd.join(CONFIG_FILE), "{}").unwrap();

        assert_eq!(
            discover_config(&cwd, Some(&app)).unwrap(),
            cwd.join(CONFIG_FILE)
        );
    }

    #[test]
    fn discover_returns_none_when_neither_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("cwd");
        let app = tmp.path().join("app");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&app).unwrap();

        assert!(discover_config(&cwd, Some(&app)).is_none());
        assert!(discover_config(&cwd, None).is_none());
    }
}
