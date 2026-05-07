//! Reference-graph tracer over the user's source tree.
//!
//! Starts from `index.html`, parses each scannable file (HTML/JS/CSS) for
//! every static reference pattern we recognize, resolves each reference
//! against the source root + importmap, and recurses. The output is a set
//! of relative paths the bundler is *known* to need plus a list of
//! "ambiguous" sites (dynamic `import(variable)` etc.) that the user
//! should review.
//!
//! This is intentionally regex-based and best-effort. A full parser is
//! overkill for the same reason this design is safe: when discovery is
//! wrong, the cost is "user adds a glob to `include`" — not silent runtime
//! 404s — because the pipeline always force-keeps `include` matches and a
//! prominent build-time warning lists what got dropped.
//!
//! Patterns we DO recognize:
//!   HTML: <script src>, <link href>, <img src>, <source src/srcset>,
//!         <video src>, <audio src>, <iframe src>, <script type="importmap">
//!   JS:   import "..." [from "..."], import("..."), export ... from "...",
//!         new Worker("..."), new SharedWorker("..."),
//!         fetch("..."), new URL("...", import.meta.url)
//!   CSS:  url(...), @import "..."
//!
//! Patterns we DO NOT recognize (ambiguous → user uses `include`):
//!   - import(expression) where the arg is non-literal
//!   - new URL(`${var}/foo.js`, ...) template-literal forms
//!   - fetch(buildPath(...))
//!   - Workers loaded by computed name
//!   - Strings constructed at runtime that happen to be paths

use anyhow::Result;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

/// Result of tracing references starting from an entry HTML file.
#[derive(Debug, Default)]
pub struct TraceResult {
    /// Relative paths (from source root) that were reached.
    pub reached: BTreeSet<PathBuf>,
    /// Relative paths of the source files we couldn't fully analyze
    /// (e.g. files containing dynamic `import(variable)` expressions).
    /// Surfaced so users know where to look when something breaks.
    pub ambiguous: Vec<AmbiguityNote>,
    /// Non-fatal warnings: missing files, bare specifiers without
    /// importmap entries, paths that escape the source root.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct AmbiguityNote {
    pub file: PathBuf,
    pub kind: &'static str,
}

/// Entry point: trace references reachable from `entry_rel` (a path
/// relative to `source_root`, normally `index.html`).
pub fn trace(source_root: &Path, entry_rel: &Path) -> Result<TraceResult> {
    let mut state = State {
        source_root: source_root.to_path_buf(),
        importmap: Importmap::default(),
        result: TraceResult::default(),
    };

    let mut queue: VecDeque<PathBuf> = VecDeque::new();
    queue.push_back(entry_rel.to_path_buf());
    state.result.reached.insert(entry_rel.to_path_buf());

    while let Some(rel) = queue.pop_front() {
        let abs = state.source_root.join(&rel);
        let bytes = match std::fs::read(&abs) {
            Ok(b) => b,
            Err(e) => {
                state
                    .result
                    .warnings
                    .push(format!("could not read {}: {}", rel.display(), e));
                continue;
            }
        };
        // Treat as text only if it looks like text. Binary leaf files
        // (images, fonts, glb, wasm) reach the queue but aren't scanned.
        let Ok(text) = std::str::from_utf8(&bytes) else {
            continue;
        };

        let kind = scannable_kind(&rel);
        let refs = match kind {
            Some(ScanKind::Html) => extract_html_refs(text, &mut state, &rel),
            Some(ScanKind::Js) => extract_js_refs(text, &mut state, &rel),
            Some(ScanKind::Css) => extract_css_refs(text, &mut state, &rel),
            None => Vec::new(),
        };

        for spec in refs {
            if let Some(resolved) = state.resolve(&spec, &rel) {
                if state.result.reached.insert(resolved.clone()) {
                    queue.push_back(resolved);
                }
            }
        }
    }

    Ok(state.result)
}

/// State carried across the BFS — read-only source root plus the
/// importmap (which `extract_html_refs` may populate when it sees a
/// `<script type="importmap">` tag) plus the accumulated result.
struct State {
    source_root: PathBuf,
    importmap: Importmap,
    result: TraceResult,
}

impl State {
    /// Resolve a reference specifier (the literal string from inside
    /// `src=`, `import "..."`, `url(...)`, etc.) against `referrer`'s
    /// directory and the importmap. Returns `None` if the spec is
    /// external, unresolvable, or the resolved file does not exist.
    fn resolve(&mut self, spec: &str, referrer: &Path) -> Option<PathBuf> {
        let trimmed = spec.trim();
        if trimmed.is_empty() {
            return None;
        }
        // Skip non-file references — these are runtime/external.
        if trimmed.starts_with("http://")
            || trimmed.starts_with("https://")
            || trimmed.starts_with("//")
            || trimmed.starts_with("data:")
            || trimmed.starts_with("blob:")
            || trimmed.starts_with('#')
            || trimmed.starts_with("mailto:")
            || trimmed.starts_with("javascript:")
        {
            return None;
        }

        // Apply importmap first (bare specifiers, scoped specifiers,
        // and directory-prefix mappings). When we hit one, the result
        // is interpreted as relative to the importmap's base URL — i.e.
        // the source root — *not* the importing file's directory. Per
        // the import-map spec, the rewrite is a URL substitution.
        let mapped = self.importmap.resolve(trimmed);
        let path_str = mapped.as_deref().unwrap_or(trimmed);

        // Strip query string and fragment — `?v=123` and `#id` aren't
        // part of the file path on disk.
        let path_str = path_str
            .split_once('?')
            .map(|(p, _)| p)
            .unwrap_or(path_str);
        let path_str = path_str
            .split_once('#')
            .map(|(p, _)| p)
            .unwrap_or(path_str);

        if path_str.is_empty() {
            return None;
        }

        // Bare specifier with no importmap match — almost always an
        // npm-style package name resolved at runtime by an injected
        // shim or Tauri itself. Note it once and skip.
        if !path_str.starts_with('.')
            && !path_str.starts_with('/')
            && mapped.is_none()
            && !looks_like_path(path_str)
        {
            self.result
                .warnings
                .push(format!("bare specifier (no importmap match): {}", path_str));
            return None;
        }

        let candidate = if path_str.starts_with('/') {
            // Absolute paths in HTML/CSS resolve against the source root
            // when the user wraps a tree as `frontendDist`.
            self.source_root.join(path_str.trim_start_matches('/'))
        } else if mapped.is_some() {
            // Importmap-substituted: base is the source root (where the
            // importmap lives in HTML), not the referrer.
            self.source_root.join(path_str.trim_start_matches("./"))
        } else if let Some(parent) = referrer.parent() {
            self.source_root.join(parent).join(path_str)
        } else {
            self.source_root.join(path_str)
        };

        // Normalize `..` and `.` without touching the filesystem.
        let normalized = normalize(&candidate)?;

        // Reject paths that escape the source root.
        let rel = match normalized.strip_prefix(&self.source_root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => {
                self.result
                    .warnings
                    .push(format!("reference escapes source root: {}", path_str));
                return None;
            }
        };

        if !normalized.exists() {
            self.result
                .warnings
                .push(format!("referenced file not found: {}", rel.display()));
            return None;
        }

        Some(rel)
    }
}

fn looks_like_path(s: &str) -> bool {
    // Heuristic: anything containing a dot followed by a known file ext,
    // or a slash with a known ext, is a path even without `./` prefix.
    // Used only to disambiguate "looks like a bare spec" cases — when in
    // doubt we treat it as a bare spec and warn (safe over-include via
    // user's `include` list, never silent under-include).
    let lower = s.to_ascii_lowercase();
    [".js", ".mjs", ".cjs", ".css", ".html", ".json", ".wasm", ".png",
     ".jpg", ".jpeg", ".webp", ".svg", ".glb", ".gltf"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Resolve a path with `..` / `.` components without touching disk.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

#[derive(Copy, Clone)]
enum ScanKind {
    Html,
    Js,
    Css,
}

fn scannable_kind(rel: &Path) -> Option<ScanKind> {
    let ext = rel.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => Some(ScanKind::Html),
        "js" | "mjs" | "cjs" => Some(ScanKind::Js),
        "css" => Some(ScanKind::Css),
        _ => None,
    }
}

// ---------- Importmap ----------

#[derive(Default)]
struct Importmap {
    /// Exact-name → resolved spec (relative path or URL). Populated
    /// from the `imports` map's non-trailing-slash entries.
    exact: BTreeMap<String, String>,
    /// Prefix (always ending in `/`) → resolved-prefix (always ending
    /// in `/`). For dir-prefix entries we strip the source-name prefix
    /// from the import and append the suffix to the target.
    prefix: BTreeMap<String, String>,
}

impl Importmap {
    fn resolve(&self, spec: &str) -> Option<String> {
        if let Some(target) = self.exact.get(spec) {
            return Some(target.clone());
        }
        // Longest matching prefix wins, per the import map spec.
        let mut best: Option<(&str, &str)> = None;
        for (src, dst) in &self.prefix {
            if spec.starts_with(src)
                && best.map(|(s, _)| src.len() > s.len()).unwrap_or(true)
            {
                best = Some((src, dst));
            }
        }
        best.map(|(src, dst)| {
            let suffix = &spec[src.len()..];
            format!("{}{}", dst, suffix)
        })
    }

    fn ingest(&mut self, json_text: &str, warnings: &mut Vec<String>) {
        let value: serde_json::Value = match serde_json::from_str(json_text) {
            Ok(v) => v,
            Err(e) => {
                warnings.push(format!("importmap parse error: {}", e));
                return;
            }
        };
        if let Some(imports) = value.get("imports").and_then(|v| v.as_object()) {
            for (k, v) in imports {
                let Some(target) = v.as_str() else { continue };
                if k.ends_with('/') {
                    if !target.ends_with('/') {
                        warnings.push(format!(
                            "importmap prefix '{}' maps to '{}' which lacks trailing slash; ignored",
                            k, target
                        ));
                        continue;
                    }
                    self.prefix.insert(k.clone(), target.to_string());
                } else {
                    self.exact.insert(k.clone(), target.to_string());
                }
            }
        }
        // Scopes intentionally ignored — they're rare in practice and
        // require resolving relative to the importing file's URL, which
        // adds a lot of complexity for marginal coverage.
    }
}

// ---------- HTML extraction ----------

fn extract_html_refs(text: &str, state: &mut State, referrer: &Path) -> Vec<String> {
    let mut refs = Vec::new();

    // Importmap first — it influences how subsequent JS imports resolve.
    for caps in importmap_re().captures_iter(text) {
        let body = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        state.importmap.ingest(body, &mut state.result.warnings);
    }

    // Generic attribute extractor for src/href on the tags we care about.
    // Group 2: double-quoted, group 3: single-quoted, group 4: unquoted.
    for caps in html_src_href_re().captures_iter(text) {
        if let Some(v) = caps.get(2).or_else(|| caps.get(3)).or_else(|| caps.get(4)) {
            refs.push(v.as_str().to_string());
        }
    }

    // srcset has comma-separated `url descriptor` pairs — split out URLs.
    for caps in html_srcset_re().captures_iter(text) {
        let value = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        for part in value.split(',') {
            let url = part.split_whitespace().next().unwrap_or("");
            if !url.is_empty() {
                refs.push(url.to_string());
            }
        }
    }

    // Inline <style> blocks and style="..." attributes contain CSS that
    // can reference assets via `url(...)`. Without scanning these the
    // tracer would miss assets referenced only from inline CSS — common
    // in single-file HTML apps (e.g. body { background: url(...) }).
    for caps in html_style_block_re().captures_iter(text) {
        let body = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        for inner in extract_css_refs(body, state, referrer) {
            refs.push(inner);
        }
    }

    // Inline <script> blocks (esp. <script type="module">) hold the same
    // import / fetch / new URL patterns we'd otherwise only catch in
    // .js files. Without this, single-file HTML apps that put their
    // entire app inside one <script type="module"> trace as zero deps.
    // Exclude importmaps (handled above) and plain-string-only JSON
    // blocks won't match the JS extractor regexes anyway.
    for caps in html_script_block_re().captures_iter(text) {
        let attrs = caps.get(1).map(|m| m.as_str()).unwrap_or_default();
        // Skip importmap blocks — already ingested into `state.importmap`.
        if attrs.to_ascii_lowercase().contains("importmap") {
            continue;
        }
        let body = caps.get(2).map(|m| m.as_str()).unwrap_or_default();
        for inner in extract_js_refs(body, state, referrer) {
            refs.push(inner);
        }
    }

    refs
}

fn html_script_block_re() -> &'static Regex {
    // Captures (attrs, body). Only matches <script>…</script> (with body),
    // never `<script src="..." />` self-closers (those are caught by the
    // generic src/href extractor).
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?is)<script\b([^>]*)>(.*?)</script>"#).unwrap())
}

fn html_style_block_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?is)<style\b[^>]*>(.*?)</style>"#).unwrap())
}

fn importmap_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?is)<script\b[^>]*\btype\s*=\s*["']importmap["'][^>]*>(.*?)</script>"#)
            .unwrap()
    })
}

fn html_src_href_re() -> &'static Regex {
    // Matches src= or href= on any tag, double or single quoted. Lazily
    // tolerates unquoted values too. The leading attribute name is
    // captured as group 1 just so the engine has somewhere to put it;
    // group 2 is the actual URL.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?i)\b(src|href)\s*=\s*(?:"([^"]+)"|'([^']+)'|([^\s>]+))"#).unwrap()
    })
}

fn html_srcset_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)\bsrcset\s*=\s*"([^"]+)""#).unwrap())
}

// ---------- JS extraction ----------

fn extract_js_refs(text: &str, state: &mut State, referrer: &Path) -> Vec<String> {
    let mut refs = Vec::new();

    for caps in js_static_import_re().captures_iter(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            refs.push(m.as_str().to_string());
        }
    }
    for caps in js_dynamic_import_re().captures_iter(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            refs.push(m.as_str().to_string());
        }
    }
    for caps in js_export_from_re().captures_iter(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            refs.push(m.as_str().to_string());
        }
    }
    for caps in js_worker_re().captures_iter(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            refs.push(m.as_str().to_string());
        }
    }
    for caps in js_fetch_re().captures_iter(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            refs.push(m.as_str().to_string());
        }
    }
    for caps in js_new_url_re().captures_iter(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            refs.push(m.as_str().to_string());
        }
    }

    // Flag dynamic-with-non-literal-arg sites for the user.
    if js_dynamic_import_nonliteral_re().is_match(text) {
        state.result.ambiguous.push(AmbiguityNote {
            file: referrer.to_path_buf(),
            kind: "dynamic import() with non-literal argument",
        });
    }

    refs
}

fn js_static_import_re() -> &'static Regex {
    // import "x"; import x from "x"; import { y } from "x";
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#"\bimport\s+(?:(?:[\w*${}\s,]+?)\s+from\s+)?(?:"([^"]+)"|'([^']+)')"#,
        )
        .unwrap()
    })
}

fn js_dynamic_import_re() -> &'static Regex {
    // import("x")
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"\bimport\s*\(\s*(?:"([^"]+)"|'([^']+)')\s*\)"#).unwrap())
}

fn js_dynamic_import_nonliteral_re() -> &'static Regex {
    // import(<not a string literal>) — flags template strings, vars, exprs.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"\bimport\s*\(\s*[^"'\s)]"#).unwrap())
}

fn js_export_from_re() -> &'static Regex {
    // export ... from "x";
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"\bexport\s+(?:[\w*${}\s,]+?)\s+from\s+(?:"([^"]+)"|'([^']+)')"#).unwrap()
    })
}

fn js_worker_re() -> &'static Regex {
    // new Worker("x"); new SharedWorker("x")
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"\bnew\s+(?:Shared)?Worker\s*\(\s*(?:"([^"]+)"|'([^']+)')"#).unwrap()
    })
}

fn js_fetch_re() -> &'static Regex {
    // fetch("x")
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"\bfetch\s*\(\s*(?:"([^"]+)"|'([^']+)')"#).unwrap())
}

fn js_new_url_re() -> &'static Regex {
    // new URL("x", import.meta.url) or new URL("x", base) — capture the
    // first arg only; we don't validate the second.
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"\bnew\s+URL\s*\(\s*(?:"([^"]+)"|'([^']+)')"#).unwrap())
}

// ---------- CSS extraction ----------

fn extract_css_refs(text: &str, _state: &mut State, _referrer: &Path) -> Vec<String> {
    let mut refs = Vec::new();
    for caps in css_url_re().captures_iter(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)).or_else(|| caps.get(3)) {
            refs.push(m.as_str().to_string());
        }
    }
    for caps in css_import_re().captures_iter(text) {
        if let Some(m) = caps.get(1).or_else(|| caps.get(2)) {
            refs.push(m.as_str().to_string());
        }
    }
    refs
}

fn css_url_re() -> &'static Regex {
    // url("x") | url('x') | url(x)
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"\burl\(\s*(?:"([^"]+)"|'([^']+)'|([^)\s]+))\s*\)"#).unwrap()
    })
}

fn css_import_re() -> &'static Regex {
    // @import "x"; @import url("x");
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"@import\s+(?:url\()?\s*(?:"([^"]+)"|'([^']+)')"#).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    #[test]
    fn html_static_script_and_link_are_traced() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "index.html",
            r#"<link rel="stylesheet" href="style.css">
               <script src="app.js"></script>
               <img src="logo.png">"#,
        );
        write(root, "style.css", "");
        write(root, "app.js", "");
        write(root, "logo.png", "");
        write(root, "unused.js", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: Vec<_> = r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains(&"style.css".to_string()));
        assert!(reached.contains(&"app.js".to_string()));
        assert!(reached.contains(&"logo.png".to_string()));
        assert!(!reached.contains(&"unused.js".to_string()));
    }

    #[test]
    fn js_static_imports_recursed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "index.html", r#"<script type="module" src="app.js"></script>"#);
        write(root, "app.js", r#"import { x } from "./util.js";"#);
        write(root, "util.js", r#"import "./helper.js";"#);
        write(root, "helper.js", "");
        write(root, "orphan.js", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> = r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("app.js"));
        assert!(reached.contains("util.js"));
        assert!(reached.contains("helper.js"));
        assert!(!reached.contains("orphan.js"));
    }

    #[test]
    fn importmap_exact_alias_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "index.html",
            r#"<script type="importmap">
                 { "imports": { "three": "./lib/three/three.module.min.js" } }
               </script>
               <script type="module" src="app.js"></script>"#,
        );
        write(root, "app.js", r#"import * as THREE from "three";"#);
        write(root, "lib/three/three.module.min.js", "");
        write(root, "lib/three/three.cjs", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("lib/three/three.module.min.js"));
        // The unused variant should NOT be reached.
        assert!(!reached.contains("lib/three/three.cjs"));
    }

    #[test]
    fn importmap_directory_prefix_recurses() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "index.html",
            r#"<script type="importmap">
                 { "imports": {
                   "three": "./lib/three/three.module.min.js",
                   "three/addons/": "./lib/three/addons/"
                 } }
               </script>
               <script type="module" src="app.js"></script>"#,
        );
        write(root, "app.js", r#"import { GLTFLoader } from "three/addons/loaders/GLTFLoader.js";"#);
        write(
            root,
            "lib/three/addons/loaders/GLTFLoader.js",
            r#"import "../utils/BufferGeometryUtils.js";"#,
        );
        write(root, "lib/three/addons/utils/BufferGeometryUtils.js", "");
        write(root, "lib/three/addons/libs/draco/draco_decoder.js", "");
        write(root, "lib/three/three.module.min.js", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("lib/three/addons/loaders/GLTFLoader.js"));
        // Transitive import should also be reached.
        assert!(reached.contains("lib/three/addons/utils/BufferGeometryUtils.js"));
        // Unimported addon should NOT be reached — this is the user's
        // chosen tradeoff for directory prefixes.
        assert!(!reached.contains("lib/three/addons/libs/draco/draco_decoder.js"));
    }

    #[test]
    fn http_urls_and_data_uris_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "index.html",
            r#"<link href="https://example.com/cdn.css" rel="stylesheet">
               <img src="data:image/png;base64,iVBOR">
               <script src="local.js"></script>"#,
        );
        write(root, "local.js", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("local.js"));
        assert_eq!(reached.len(), 2); // index.html + local.js
    }

    #[test]
    fn path_escape_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "index.html", r#"<script src="../escape.js"></script>"#);

        let r = trace(root, Path::new("index.html")).unwrap();
        // index.html itself is reached; ../escape.js never resolves to
        // a path inside source root, so nothing else.
        assert_eq!(r.reached.len(), 1);
        assert!(r.warnings.iter().any(|w| w.contains("escape")));
    }

    #[test]
    fn dynamic_import_with_variable_is_flagged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "index.html", r#"<script type="module" src="app.js"></script>"#);
        write(
            root,
            "app.js",
            r#"const name = "x"; import(name).then(m => {});"#,
        );

        let r = trace(root, Path::new("app.js")).unwrap();
        assert!(!r.ambiguous.is_empty());
        assert_eq!(r.ambiguous[0].kind, "dynamic import() with non-literal argument");
    }

    #[test]
    fn inline_script_module_imports_are_traced() {
        // Single-file HTML apps commonly inline `<script type="module">`
        // with the entire app inside. Without scanning script bodies the
        // tracer would miss every import they declare.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "index.html",
            r#"<script type="importmap">
                 { "imports": { "three": "./lib/three.js" } }
               </script>
               <script type="module">
                 import * as THREE from "three";
                 import "./util.js";
                 fetch("./data.json");
               </script>"#,
        );
        write(root, "lib/three.js", "");
        write(root, "util.js", "");
        write(root, "data.json", "");
        write(root, "orphan.js", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("lib/three.js"));
        assert!(reached.contains("util.js"));
        assert!(reached.contains("data.json"));
        assert!(!reached.contains("orphan.js"));
    }

    #[test]
    fn inline_style_block_url_is_traced() {
        // Single-file HTML apps commonly put url(...) inside a <style>
        // block. A naive tracer that only reads .css files would miss
        // those references.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "index.html",
            r#"<style>
                 body { background: url("/assets/bg.png"); }
                 .hero { background-image: url(assets/hero.png); }
               </style>"#,
        );
        write(root, "assets/bg.png", "");
        write(root, "assets/hero.png", "");
        write(root, "assets/orphan.png", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("assets/bg.png"));
        assert!(reached.contains("assets/hero.png"));
        assert!(!reached.contains("assets/orphan.png"));
    }

    #[test]
    fn css_url_and_import_are_traced() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "index.html",
            r#"<link rel="stylesheet" href="main.css">"#,
        );
        write(
            root,
            "main.css",
            r#"@import "reset.css";
               body { background: url("bg.png"); }"#,
        );
        write(root, "reset.css", "");
        write(root, "bg.png", "");
        write(root, "unused.png", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("main.css"));
        assert!(reached.contains("reset.css"));
        assert!(reached.contains("bg.png"));
        assert!(!reached.contains("unused.png"));
    }

    #[test]
    fn fetch_and_new_url_string_literals_traced() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "index.html", r#"<script type="module" src="app.js"></script>"#);
        write(
            root,
            "app.js",
            r#"
            fetch("./data/levels.json").then(r => r.json());
            const u = new URL("./assets/model.glb", import.meta.url);
            "#,
        );
        write(root, "data/levels.json", "{}");
        write(root, "assets/model.glb", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("data/levels.json"));
        assert!(reached.contains("assets/model.glb"));
    }

    #[test]
    fn worker_constructor_is_traced() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(root, "index.html", r#"<script type="module" src="app.js"></script>"#);
        write(root, "app.js", r#"new Worker("./worker.js");"#);
        write(root, "worker.js", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("worker.js"));
    }

    #[test]
    fn srcset_extracts_all_urls() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "index.html",
            r#"<img srcset="small.png 1x, large.png 2x" src="small.png">"#,
        );
        write(root, "small.png", "");
        write(root, "large.png", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("small.png"));
        assert!(reached.contains("large.png"));
    }

    #[test]
    fn query_and_fragment_stripped_for_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "index.html",
            r#"<link rel="stylesheet" href="style.css?v=123">
               <script src="app.js#bottom"></script>"#,
        );
        write(root, "style.css", "");
        write(root, "app.js", "");

        let r = trace(root, Path::new("index.html")).unwrap();
        let reached: BTreeSet<_> =
            r.reached.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        assert!(reached.contains("style.css"));
        assert!(reached.contains("app.js"));
    }

    #[test]
    fn longest_importmap_prefix_wins() {
        // Importmap spec: longer prefix takes precedence over shorter.
        let mut im = Importmap::default();
        let mut warns = Vec::new();
        im.ingest(
            r#"{ "imports": { "a/": "/short/", "a/b/": "/long/" } }"#,
            &mut warns,
        );
        assert_eq!(im.resolve("a/b/c.js").unwrap(), "/long/c.js");
        assert_eq!(im.resolve("a/x.js").unwrap(), "/short/x.js");
    }
}
