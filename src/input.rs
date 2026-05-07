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
}
