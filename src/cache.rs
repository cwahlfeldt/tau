//! Shared CARGO_TARGET_DIR management.
//!
//! Every wrap runs in a fresh tempdir, so without a stable cache Cargo would
//! recompile Tauri (and ~700 transitive crates) every time. We point
//! CARGO_TARGET_DIR at a per-user directory under the OS cache root and reuse
//! it across runs. The downside is that the directory accumulates artifacts
//! over time; this module centralises the path computation and the prune /
//! clear / size operations the CLI exposes via `tau cache`.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use walkdir::WalkDir;

use crate::cli::CacheAction;

/// Absolute path to the shared `CARGO_TARGET_DIR`. Created if missing.
///
/// macOS:   `~/Library/Caches/tau/target`
/// Linux:   `$XDG_CACHE_HOME/tau/target` or `~/.cache/tau/target`
/// Windows: `%LOCALAPPDATA%/tau/target`
pub fn dir() -> Result<PathBuf> {
    let dir = base_cache_dir()?.join("tau").join("target");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create cache dir {}", dir.display()))?;
    Ok(dir)
}

/// Public accessor for the OS-level cache root (parent of `tau/`).
/// Other tau-owned cache subdirectories (e.g. keystores) live alongside `target/`.
pub fn base() -> Result<PathBuf> {
    base_cache_dir()
}

/// Resolve the OS-appropriate cache root (the parent of our `tau/target`).
fn base_cache_dir() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CACHE_HOME") {
        return Ok(PathBuf::from(xdg));
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Ok(if cfg!(target_os = "macos") {
            PathBuf::from(home).join("Library").join("Caches")
        } else {
            PathBuf::from(home).join(".cache")
        });
    }
    if let Some(localapp) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(localapp));
    }
    bail!("could not determine cache directory (no HOME / XDG_CACHE_HOME / LOCALAPPDATA)")
}

/// Recursive `du` in bytes. Symlinks are not followed.
pub fn size_bytes(path: &Path) -> Result<u64> {
    if !path.exists() {
        return Ok(0);
    }
    let mut total: u64 = 0;
    for entry in WalkDir::new(path).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            if let Ok(meta) = entry.metadata() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

/// Pretty-print a byte count with binary units (KiB/MiB/GiB).
pub fn format_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut idx = 0;
    while value >= 1024.0 && idx < UNITS.len() - 1 {
        value /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", bytes, UNITS[0])
    } else {
        format!("{:.2} {}", value, UNITS[idx])
    }
}

/// Outcome of a prune pass.
pub struct PruneReport {
    pub files_removed: u64,
    pub bytes_freed: u64,
    pub dry_run: bool,
}

/// Delete files in the cache whose mtime is older than `max_age`. Empty
/// directories left behind are removed in a second pass. With `dry_run`,
/// nothing is deleted but the report still tallies what would have been.
pub fn prune(root: &Path, max_age: std::time::Duration, dry_run: bool) -> Result<PruneReport> {
    let mut report = PruneReport { files_removed: 0, bytes_freed: 0, dry_run };
    if !root.exists() {
        return Ok(report);
    }
    let cutoff = SystemTime::now()
        .checked_sub(max_age)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    for entry in WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        if mtime >= cutoff {
            continue;
        }
        report.files_removed += 1;
        report.bytes_freed = report.bytes_freed.saturating_add(meta.len());
        if !dry_run {
            let _ = std::fs::remove_file(entry.path());
        }
    }

    if !dry_run {
        remove_empty_dirs(root);
    }
    Ok(report)
}

/// Walk bottom-up and `rmdir` anything that's now empty. Best-effort: the
/// root is never removed, and we ignore errors from individual rmdirs.
fn remove_empty_dirs(root: &Path) {
    let mut dirs: Vec<PathBuf> = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_dir())
        .map(|e| e.path().to_path_buf())
        .collect();
    // Process deepest paths first so parents become eligible after their children go.
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in dirs {
        if d == root {
            continue;
        }
        let _ = std::fs::remove_dir(&d);
    }
}

/// Delete the cache directory entirely.
pub fn clear(root: &Path) -> Result<()> {
    if !root.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(root)
        .with_context(|| format!("failed to clear cache dir {}", root.display()))
}

/// Handle `tau cache <action>`. Dispatched from `main.rs`.
pub fn run_command(action: &CacheAction) -> Result<()> {
    let dir = dir().context("could not resolve cache directory")?;
    match action {
        CacheAction::Size => {
            let bytes = size_bytes(&dir)?;
            println!("{}", dir.display());
            println!("{}", format_size(bytes));
        }
        CacheAction::Clear => {
            let bytes = size_bytes(&dir).unwrap_or(0);
            clear(&dir)?;
            println!("cleared {} ({})", dir.display(), format_size(bytes));
        }
        CacheAction::Prune { days, dry_run } => {
            let max_age = std::time::Duration::from_secs(days.saturating_mul(60 * 60 * 24));
            let report = prune(&dir, max_age, *dry_run)?;
            let prefix = if report.dry_run { "would remove" } else { "removed" };
            println!(
                "{} {} files ({}) older than {} days from {}",
                prefix,
                report.files_removed,
                format_size(report.bytes_freed),
                days,
                dir.display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_size_picks_unit() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1024), "1.00 KiB");
        assert_eq!(format_size(1024 * 1024), "1.00 MiB");
        assert_eq!(format_size(5 * 1024 * 1024 * 1024), "5.00 GiB");
    }

    #[test]
    fn size_bytes_counts_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/x.txt"), b"hello").unwrap();
        std::fs::write(root.join("a/b/y.txt"), b"world!").unwrap();
        assert_eq!(size_bytes(root).unwrap(), 5 + 6);
    }

    #[test]
    fn size_bytes_zero_for_missing() {
        let p = std::path::PathBuf::from("/this/path/should/not/exist/wrapitup");
        assert_eq!(size_bytes(&p).unwrap(), 0);
    }

    fn backdate(path: &Path, secs: u64) {
        let f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        let when = SystemTime::now() - std::time::Duration::from_secs(secs);
        f.set_modified(when).unwrap();
    }

    #[test]
    fn prune_dry_run_keeps_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("old.txt"), b"old").unwrap();
        backdate(&root.join("old.txt"), 60 * 60 * 24 * 60);

        let report = prune(root, std::time::Duration::from_secs(60 * 60 * 24 * 30), true).unwrap();
        assert!(report.dry_run);
        assert_eq!(report.files_removed, 1);
        assert!(root.join("old.txt").exists(), "dry-run must not delete");
    }

    #[test]
    fn prune_deletes_old_files_only() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("old.txt"), b"old").unwrap();
        std::fs::write(root.join("new.txt"), b"new").unwrap();
        backdate(&root.join("old.txt"), 60 * 60 * 24 * 60);

        let report = prune(root, std::time::Duration::from_secs(60 * 60 * 24 * 30), false).unwrap();
        assert_eq!(report.files_removed, 1);
        assert!(!root.join("old.txt").exists());
        assert!(root.join("new.txt").exists());
    }
}
