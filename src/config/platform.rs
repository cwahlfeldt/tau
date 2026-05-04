//! Per-platform metadata, lookup, and parsing.
//!
//! `PLATFORM_SPECS` is the single source of truth: every fact about a target
//! (rustup triples, artifact extensions, artifact-extraction policy) lives
//! here. Adding a new platform is a one-row data change.

use anyhow::{anyhow, Result};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Windows,
    Linux,
    Android,
    Ios,
}

/// How a platform's bundle output is identified inside the shared cache.
///
/// Desktop bundles share the global `CARGO_TARGET_DIR`, so they get filtered
/// by product name. Mobile bundles live in the ephemeral scaffold tree and
/// use generic Gradle/Xcode names — they get renamed to `<slug>.<ext>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactPolicy {
    FilterByProductName,
    RenameBySlug,
}

pub struct PlatformSpec {
    pub platform: Platform,
    pub canonical: &'static str,
    pub aliases: &'static [&'static str],
    pub rustup_targets: &'static [&'static str],
    pub artifact_exts: &'static [&'static str],
    pub artifact_policy: ArtifactPolicy,
}

pub const PLATFORM_SPECS: &[PlatformSpec] = &[
    PlatformSpec {
        platform: Platform::Macos,
        canonical: "macos",
        aliases: &["mac", "darwin", "osx"],
        rustup_targets: &[],
        artifact_exts: &["app", "dmg"],
        artifact_policy: ArtifactPolicy::FilterByProductName,
    },
    PlatformSpec {
        platform: Platform::Windows,
        canonical: "windows",
        aliases: &["win"],
        rustup_targets: &[],
        artifact_exts: &["exe", "msi"],
        artifact_policy: ArtifactPolicy::FilterByProductName,
    },
    PlatformSpec {
        platform: Platform::Linux,
        canonical: "linux",
        aliases: &[],
        rustup_targets: &[],
        artifact_exts: &["AppImage", "deb", "rpm"],
        artifact_policy: ArtifactPolicy::FilterByProductName,
    },
    PlatformSpec {
        platform: Platform::Android,
        canonical: "android",
        aliases: &[],
        rustup_targets: &[
            "aarch64-linux-android",
            "armv7-linux-androideabi",
            "i686-linux-android",
            "x86_64-linux-android",
        ],
        artifact_exts: &["apk", "aab"],
        artifact_policy: ArtifactPolicy::RenameBySlug,
    },
    PlatformSpec {
        platform: Platform::Ios,
        canonical: "ios",
        aliases: &[],
        rustup_targets: &["aarch64-apple-ios", "aarch64-apple-ios-sim", "x86_64-apple-ios"],
        artifact_exts: &["ipa", "app"],
        artifact_policy: ArtifactPolicy::RenameBySlug,
    },
];

impl Platform {
    pub fn spec(&self) -> &'static PlatformSpec {
        PLATFORM_SPECS
            .iter()
            .find(|s| s.platform == *self)
            .expect("every Platform variant has a PlatformSpec entry")
    }

    pub fn as_str(&self) -> &'static str {
        self.spec().canonical
    }

    pub fn host() -> Self {
        if cfg!(target_os = "macos") {
            Platform::Macos
        } else if cfg!(target_os = "windows") {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

impl FromStr for Platform {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let needle = s.trim().to_ascii_lowercase();
        PLATFORM_SPECS
            .iter()
            .find(|spec| spec.canonical == needle || spec.aliases.iter().any(|a| *a == needle))
            .map(|spec| spec.platform)
            .ok_or_else(|| anyhow!("unknown platform '{}'", s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_parse_canonical_and_aliases() {
        assert_eq!("macos".parse::<Platform>().unwrap(), Platform::Macos);
        assert_eq!("MAC".parse::<Platform>().unwrap(), Platform::Macos);
        assert_eq!(" darwin ".parse::<Platform>().unwrap(), Platform::Macos);
        assert_eq!("windows".parse::<Platform>().unwrap(), Platform::Windows);
        assert_eq!("win".parse::<Platform>().unwrap(), Platform::Windows);
        assert_eq!("linux".parse::<Platform>().unwrap(), Platform::Linux);
        assert_eq!("android".parse::<Platform>().unwrap(), Platform::Android);
        assert_eq!("ios".parse::<Platform>().unwrap(), Platform::Ios);
    }

    #[test]
    fn platform_parse_rejects_unknown() {
        assert!("ps5".parse::<Platform>().is_err());
        assert!("".parse::<Platform>().is_err());
    }

    #[test]
    fn every_platform_has_a_spec() {
        for p in [Platform::Macos, Platform::Windows, Platform::Linux, Platform::Android, Platform::Ios] {
            // would panic if missing
            let _ = p.spec();
        }
    }
}
