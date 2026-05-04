//! Resolved configuration for a single tau run.
//!
//! Three-tier resolution: CLI flags > `tau.conf.json` > defaults.
//! `Config` is the immutable, validated handle that every downstream stage
//! (`discover`, `scaffold`, `build`) consumes by reference.

use anyhow::{anyhow, Context, Result};
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
    pub include: Vec<String>,
    pub output: String,
    pub platforms: Vec<Platform>,
    pub profile: BuildProfile,
}

/// "What kind of build" with signing baked in. Release without signing is
/// unrepresentable so we fail at config time, not deep inside the build.
/// The `SigningConfig` payload is currently a seam — release signing is
/// validated but not yet wired into the build commands.
#[derive(Debug, Clone)]
pub enum BuildProfile {
    Debug,
    #[allow(dead_code)]
    Release(SigningConfig),
}

impl BuildProfile {
    pub fn is_release(&self) -> bool {
        matches!(self, BuildProfile::Release(_))
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
    include: Option<Vec<String>>,
    build: Option<BuildSection>,
    signing: Option<SigningConfig>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct BuildSection {
    output: Option<String>,
    platforms: Option<Vec<String>>,
}

pub fn resolve(cwd: &Path, cli: &Cli) -> Result<Config> {
    let file = load_file_config(cwd, cli.config.as_deref())?;
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
    let profile = resolve_profile(cli.release, file.signing)?;

    Ok(Config {
        name,
        version,
        identifier,
        include: file.include.unwrap_or_default(),
        output,
        platforms,
        profile,
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

fn resolve_profile(release: bool, signing: Option<SigningConfig>) -> Result<BuildProfile> {
    if !release {
        return Ok(BuildProfile::Debug);
    }
    let signing = signing
        .ok_or_else(|| anyhow!("--release requires a 'signing' block in tau.conf.json"))?;
    Ok(BuildProfile::Release(signing))
}

fn load_file_config(cwd: &Path, explicit: Option<&Path>) -> Result<FileConfig> {
    let path = match explicit {
        Some(p) => p.to_path_buf(),
        None => {
            let default = cwd.join(DEFAULT_CONFIG_FILE);
            if !default.exists() {
                return Ok(FileConfig::default());
            }
            default
        }
    };
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read config: {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse config: {}", path.display()))
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
        assert_eq!(BuildProfile::Release(SigningConfig::default()).dir_name(), "release");
    }
}
