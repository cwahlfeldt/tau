//! Materialize a filtered copy of the user's source tree for the bundler.
//!
//! Tauri's `frontendDist` walks the *whole* directory and embeds every file
//! it finds. That means anything sitting next to `index.html` — `.git/`,
//! `node_modules/`, README files, prior `build/` output, dev-only asset
//! variants — would otherwise ship inside the app. To keep the original
//! "we don't rewrite, just point" design intact while still letting users
//! drop unused content, we copy a filtered tree to a tempdir and point
//! `frontendDist` at the copy. The dev path keeps pointing at the live
//! source (so reloads pick up edits).

use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::Path;
use tempfile::TempDir;
use walkdir::WalkDir;

/// Result of walking the source tree and copying allowed files into a
/// fresh tempdir. The `TempDir` is kept in `dir` so the caller can extend
/// its lifetime past the copy itself (the bundler reads from it later).
pub struct Materialized {
    pub dir: TempDir,
    pub copied: usize,
    pub excluded: usize,
}

impl Materialized {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

pub fn materialize(source: &Path, patterns: &[String]) -> Result<Materialized> {
    let matcher = build_matcher(patterns)?;
    let dest = tempfile::Builder::new()
        .prefix("tau-frontend-")
        .tempdir()
        .context("failed to create temp frontend dir")?;

    let mut copied = 0usize;
    let mut excluded = 0usize;

    let walker = WalkDir::new(source).into_iter().filter_entry(|entry| {
        // Prune directories early so we don't recurse into excluded trees
        // (e.g. `node_modules`) at all — pure perf, not correctness.
        let Ok(rel) = entry.path().strip_prefix(source) else {
            return true;
        };
        if rel.as_os_str().is_empty() {
            return true; // root itself
        }
        !matcher.is_match(rel)
    });

    for entry in walker {
        let entry = entry.context("walk source tree")?;
        let path = entry.path();
        let rel = match path.strip_prefix(source) {
            Ok(r) if !r.as_os_str().is_empty() => r,
            _ => continue,
        };

        if matcher.is_match(rel) {
            excluded += 1;
            continue;
        }

        let target = dest.path().join(rel);
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("create dir {}", target.display()))?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create dir {}", parent.display()))?;
            }
            std::fs::copy(path, &target)
                .with_context(|| format!("copy {} -> {}", path.display(), target.display()))?;
            copied += 1;
        }
        // Skip symlinks/special files — keeps the bundler input deterministic.
    }

    Ok(Materialized { dir: dest, copied, excluded })
}

fn build_matcher(patterns: &[String]) -> Result<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let glob = Glob::new(pat).with_context(|| format!("invalid exclude pattern: {}", pat))?;
        builder.add(glob);
    }
    builder.build().context("build exclude matcher")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn touch(path: &Path) {
        if let Some(p) = path.parent() {
            fs::create_dir_all(p).unwrap();
        }
        fs::write(path, b"").unwrap();
    }

    fn collect_relpaths(root: &Path) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for entry in WalkDir::new(root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            paths.push(entry.path().strip_prefix(root).unwrap().to_path_buf());
        }
        paths.sort();
        paths
    }

    #[test]
    fn copies_everything_when_no_patterns() {
        let src = tempfile::tempdir().unwrap();
        touch(&src.path().join("index.html"));
        touch(&src.path().join("a/b.js"));

        let m = materialize(src.path(), &[]).unwrap();
        assert_eq!(m.copied, 2);
        assert_eq!(m.excluded, 0);
        assert_eq!(
            collect_relpaths(m.path()),
            vec![PathBuf::from("a/b.js"), PathBuf::from("index.html")]
        );
    }

    #[test]
    fn excludes_top_level_dir() {
        let src = tempfile::tempdir().unwrap();
        touch(&src.path().join("index.html"));
        touch(&src.path().join(".git/config"));
        touch(&src.path().join(".git/objects/x"));

        let m = materialize(src.path(), &[".git".into(), ".git/**".into()]).unwrap();
        assert_eq!(collect_relpaths(m.path()), vec![PathBuf::from("index.html")]);
        assert_eq!(m.copied, 1);
    }

    #[test]
    fn excludes_glob_anywhere_in_tree() {
        let src = tempfile::tempdir().unwrap();
        touch(&src.path().join("index.html"));
        touch(&src.path().join(".DS_Store"));
        touch(&src.path().join("assets/.DS_Store"));
        touch(&src.path().join("assets/img.png"));

        let m = materialize(src.path(), &["**/.DS_Store".into()]).unwrap();
        assert_eq!(
            collect_relpaths(m.path()),
            vec![PathBuf::from("assets/img.png"), PathBuf::from("index.html")]
        );
    }

    #[test]
    fn excludes_user_specified_files() {
        let src = tempfile::tempdir().unwrap();
        touch(&src.path().join("index.html"));
        touch(&src.path().join("README.md"));
        touch(&src.path().join("LICENSE.txt"));
        touch(&src.path().join("assets/License.txt"));

        let m = materialize(
            src.path(),
            &["README.md".into(), "**/License.txt".into(), "LICENSE.txt".into()],
        )
        .unwrap();
        assert_eq!(collect_relpaths(m.path()), vec![PathBuf::from("index.html")]);
    }

    #[test]
    fn invalid_glob_errors_at_build_time() {
        let err = build_matcher(&["[unclosed".into()]).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("[unclosed"), "error should reference the bad pattern: {msg}");
    }
}
