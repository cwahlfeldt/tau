//! Classify the positional `index` argument as either a local HTML file
//! or a remote URL. URLs cause the wrapped app's window to point at the
//! remote; local files cause Tauri's `frontendDist` to point at their
//! parent directory.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub enum Input {
    File { source_root: PathBuf },
    Url(String),
}

impl Input {
    /// Parse the raw positional argument. http(s) prefixes are treated as
    /// URLs; everything else is canonicalized as a filesystem path.
    pub fn parse(raw: &str) -> Result<Self> {
        if is_url(raw) {
            return Ok(Input::Url(raw.to_string()));
        }
        Self::parse_path(Path::new(raw))
    }

    fn parse_path(path: &Path) -> Result<Self> {
        let index_path = std::fs::canonicalize(path)
            .with_context(|| format!("index.html not found: {}", path.display()))?;
        let source_root = index_path
            .parent()
            .ok_or_else(|| anyhow!("could not determine source root from {}", index_path.display()))?
            .to_path_buf();
        Ok(Input::File { source_root })
    }

    /// Short, user-facing label for the header line.
    pub fn label(&self) -> String {
        match self {
            Input::File { source_root } => source_root.display().to_string(),
            Input::Url(u) => u.clone(),
        }
    }
}

fn is_url(s: &str) -> bool {
    let lower = s.trim().to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// A tau game project on disk. The marker is the `.tau/` directory at the
/// project root — `tau.conf.json` is optional and unreliable as a marker.
#[derive(Debug, Clone)]
#[allow(dead_code)] // some fields/methods are part of the project surface but
                    // not used by every command.
pub struct ProjectRoot {
    pub root: PathBuf,
    pub src_dir: PathBuf,
    pub tau_dir: PathBuf,
    pub dist_dir: PathBuf,
}

impl ProjectRoot {
    fn at(root: PathBuf) -> Self {
        let tau_dir = root.join(".tau");
        let dist_dir = tau_dir.join("dist");
        let src_dir = root.join("src");
        Self { root, src_dir, tau_dir, dist_dir }
    }

    /// Path to `src/index.html`. Not validated — caller decides whether to
    /// require it (dev/build do; create doesn't).
    #[allow(dead_code)]
    pub fn index_html(&self) -> PathBuf {
        self.src_dir.join("index.html")
    }
}

/// Walk up from `cwd` looking for a `.tau/` directory. Returns the project
/// root if found. Stops at the filesystem root.
pub fn discover_project(cwd: &Path) -> Option<ProjectRoot> {
    let mut current = cwd.to_path_buf();
    loop {
        if current.join(".tau").is_dir() {
            return Some(ProjectRoot::at(current));
        }
        if !current.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_is_url() {
        assert!(matches!(Input::parse("https://example.com").unwrap(), Input::Url(_)));
        assert!(matches!(Input::parse("HTTPS://Example.com").unwrap(), Input::Url(_)));
    }

    #[test]
    fn http_is_url() {
        assert!(matches!(Input::parse("http://localhost:3000").unwrap(), Input::Url(_)));
    }

    #[test]
    fn plain_string_is_not_url() {
        assert!(!is_url("index.html"));
        assert!(!is_url("./examples/sample-app/index.html"));
        assert!(!is_url("file:///etc/passwd"));
    }

    #[test]
    fn missing_file_errors() {
        let err = Input::parse("definitely-not-a-real-file-xyz.html").unwrap_err();
        assert!(err.to_string().contains("not found"), "got: {}", err);
    }

    #[test]
    fn discover_finds_project_in_cwd() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".tau")).unwrap();
        let p = discover_project(tmp.path()).unwrap();
        assert_eq!(p.root, tmp.path());
        assert_eq!(p.tau_dir, tmp.path().join(".tau"));
        assert_eq!(p.src_dir, tmp.path().join("src"));
        assert_eq!(p.dist_dir, tmp.path().join(".tau").join("dist"));
    }

    #[test]
    fn discover_walks_up_to_parent() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("a").join("b");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir(tmp.path().join(".tau")).unwrap();
        let p = discover_project(&nested).unwrap();
        assert_eq!(p.root, tmp.path());
    }

    #[test]
    fn discover_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover_project(tmp.path()).is_none());
    }

    #[test]
    fn project_index_html_path() {
        let p = ProjectRoot::at(PathBuf::from("/tmp/g"));
        assert_eq!(p.index_html(), PathBuf::from("/tmp/g/src/index.html"));
    }
}
