//! Resolved configuration for a single tau run.
//!
//! Three-tier resolution: CLI flags > `tau.conf.json` > defaults.
//! `Config` is the immutable, validated handle that every downstream stage
//! (`scaffold`, `build`) consumes by reference.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use crate::cli::Cli;

mod platform;

pub use platform::{ArtifactPolicy, Platform};

const DEFAULT_NAME: &str = "WrappedApp";
const DEFAULT_VERSION: &str = "0.1.0";
const DEFAULT_OUTPUT: &str = "./build";
const DEFAULT_CONFIG_FILE: &str = "tau.conf.json";

#[derive(Debug, Clone)]
pub struct Config {
    pub name: String,
    pub version: String,
    pub identifier: String,
    pub output: String,
    pub platforms: Vec<Platform>,
    pub profile: BuildProfile,
    /// Glob patterns (relative to the source root) of files to exclude when
    /// materializing the frontend tree the bundler embeds. Applied as a
    /// post-trace overlay when `tree_shake` is on (drops files even if
    /// reachable), and as the sole filter when `tree_shake` is off.
    pub exclude: Vec<String>,
    /// Glob patterns (relative to the source root) of files that must
    /// always be included regardless of reachability. The escape hatch
    /// for dynamically-loaded assets the tracer can't see (computed
    /// `fetch`/`new URL` paths, workers loaded by computed name, etc.).
    /// Empty when `tree_shake` is off.
    pub include: Vec<String>,
    /// When true (default), only files reachable from `index.html` are
    /// embedded in the bundle. When false, the whole source tree minus
    /// `exclude` ships — the original "we don't rewrite, just point" mode.
    pub tree_shake: bool,
}

/// Patterns always excluded from the materialized frontend tree, regardless
/// of user config. The user's `output` dir is added on top of these by
/// `resolve()` (it's only known after config layering).
pub fn default_excludes() -> Vec<String> {
    vec![
        ".git".into(),
        ".git/**".into(),
        ".gitignore".into(),
        ".DS_Store".into(),
        "**/.DS_Store".into(),
        "node_modules".into(),
        "node_modules/**".into(),
        ".claude".into(),
        ".claude/**".into(),
        "tau.conf.json".into(),
        // Source maps sit next to source files but are never reachable at
        // runtime in a packaged app — they only matter when devtools is
        // pointed at a hosted source tree.
        "**/*.map".into(),
    ]
}

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
#[allow(dead_code)]
pub struct SigningConfig {
    pub android_keystore: Option<PathBuf>,
    pub android_keystore_password: Option<String>,
    pub android_key_alias: Option<String>,
    pub android_key_password: Option<String>,
    pub apple_signing_identity: Option<String>,
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
    /// User-supplied glob patterns (relative to source root) appended to
    /// `default_excludes()`. With `treeShake` on these act as a *post-trace*
    /// overlay (drop a file even if reachable); with `treeShake` off
    /// they're the sole filter against the full tree.
    exclude: Option<Vec<String>>,
    /// Globs (relative to source root) that must always be embedded
    /// regardless of reachability. Escape hatch for dynamic loads.
    /// Ignored when `treeShake` is off.
    include: Option<Vec<String>>,
    /// Discover reachable files from `index.html` and only embed those.
    /// Defaults to true. Set to false to ship the whole source tree minus
    /// `exclude` (the original behavior).
    #[serde(rename = "treeShake")]
    tree_shake: Option<bool>,
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
pub fn resolve(cwd: &Path, index_dir: Option<&Path>, cli: &Cli) -> Result<Config> {
    let file = load_file_config(cwd, index_dir, cli.config.as_deref())?;
    let build = file.build.unwrap_or_default();

    let name = cli
        .name
        .clone()
        .or(file.name)
        .unwrap_or_else(|| DEFAULT_NAME.to_string());

    let version = file.version.unwrap_or_else(|| DEFAULT_VERSION.to_string());

    let identifier = cli
        .identifier
        .clone()
        .or(file.identifier)
        .unwrap_or_else(|| default_identifier(&name));

    let output = cli
        .output
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .or(build.output)
        .unwrap_or_else(|| DEFAULT_OUTPUT.to_string());

    let platforms = resolve_platforms(&cli.platform, build.platforms.as_deref())?;
    let profile = if cli.release { BuildProfile::Release } else { BuildProfile::Debug };
    let _ = file.signing; // parsed for forward-compat; not yet wired into builds

    let mut exclude = default_excludes();
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

    let include = file.include.unwrap_or_default();
    // CLI `--no-tree-shake` wins over the config file. Default is on.
    let tree_shake = if cli.no_tree_shake {
        false
    } else {
        file.tree_shake.unwrap_or(true)
    };

    Ok(Config {
        name,
        version,
        identifier,
        output,
        platforms,
        profile,
        exclude,
        include,
        tree_shake,
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
/// 2. The current working directory (kept for back-compat with the
///    "I'm already standing in the project" workflow).
fn discover_config(cwd: &Path, index_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(dir) = index_dir {
        let candidate = dir.join(DEFAULT_CONFIG_FILE);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    let candidate = cwd.join(DEFAULT_CONFIG_FILE);
    if candidate.exists() {
        return Some(candidate);
    }
    None
}

fn default_identifier(name: &str) -> String {
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
    fn file_config_tree_shake_defaults_on_when_omitted() {
        let parsed: FileConfig = serde_json::from_str("{}").unwrap();
        assert!(parsed.tree_shake.is_none());
        // resolve() materializes the default to true.
    }

    #[test]
    fn file_config_parses_include_and_tree_shake() {
        let json = r#"{
            "include": ["data/**/*.json", "models/*.glb"],
            "treeShake": false
        }"#;
        let parsed: FileConfig = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.include.unwrap().len(), 2);
        assert_eq!(parsed.tree_shake, Some(false));
    }

    #[test]
    fn default_excludes_includes_source_maps() {
        // Source maps sit next to source files but are never reachable.
        // If this changes, tree-shaking discovery may suddenly include them.
        assert!(default_excludes().iter().any(|e| e == "**/*.map"));
    }

    #[test]
    fn discover_prefers_index_dir_over_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("cwd");
        let app = tmp.path().join("app");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&app).unwrap();
        std::fs::write(cwd.join(DEFAULT_CONFIG_FILE), "{}").unwrap();
        std::fs::write(app.join(DEFAULT_CONFIG_FILE), "{}").unwrap();

        assert_eq!(
            discover_config(&cwd, Some(&app)).unwrap(),
            app.join(DEFAULT_CONFIG_FILE)
        );
    }

    #[test]
    fn discover_falls_back_to_cwd_when_index_dir_has_none() {
        let tmp = tempfile::tempdir().unwrap();
        let cwd = tmp.path().join("cwd");
        let app = tmp.path().join("app");
        std::fs::create_dir(&cwd).unwrap();
        std::fs::create_dir(&app).unwrap();
        std::fs::write(cwd.join(DEFAULT_CONFIG_FILE), "{}").unwrap();

        assert_eq!(
            discover_config(&cwd, Some(&app)).unwrap(),
            cwd.join(DEFAULT_CONFIG_FILE)
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
