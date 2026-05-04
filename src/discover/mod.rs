//! Stream `index.html`, collect referenced local assets, and rewrite paths.
//!
//! Everything an `index.html` references is funneled through `lol_html`
//! element handlers. Absolute paths (e.g. `/js/app.js`) get rewritten to
//! relative form (`./js/app.js`) so Tauri's `frontendDist` resolves them
//! inside the bundled `dist/`. The plugin-bridge `<script>` tags are
//! injected into `<head>` (or prepended if `<head>` is missing).

use anyhow::{anyhow, Context, Result};
use lol_html::html_content::{ContentType, Element, TextChunk};
use lol_html::{ElementContentHandlers, HtmlRewriter, Selector, Settings};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use walkdir::WalkDir;

use crate::config::Config;

mod css;
mod glob;

#[derive(Debug)]
pub struct Discovered {
    /// Rewritten index.html bytes (absolute paths converted to relative).
    pub index_html: Vec<u8>,
    /// Set of local assets relative to the source root.
    pub assets: BTreeSet<PathBuf>,
    /// Absolute path to the directory the assets are relative to.
    pub source_root: PathBuf,
}

/// Element/attribute pairs treated as asset references.
const ATTR_TARGETS: &[(&str, &str)] = &[
    ("script[src]", "src"),
    ("link[href]", "href"),
    ("img[src]", "src"),
    ("source[src]", "src"),
    ("audio[src]", "src"),
    ("video[src]", "src"),
    ("video[poster]", "poster"),
    ("iframe[src]", "src"),
    ("a[href]", "href"),
];

/// Elements whose `srcset` attribute is parsed as a list of candidate URLs.
const SRCSET_TARGETS: &[&str] = &["img[srcset]", "source[srcset]"];

/// Script tags injected at the start of `<head>`. Filenames must match the
/// bundles emitted by `scaffold::PLUGIN_BUNDLES` into `dist/__tauri/`.
/// `withGlobalTauri` is delivered by Tauri as a webview init script (runs
/// before document parsing), so `window.__TAURI__` exists by the time these
/// scripts execute. No `defer` — we want them parsed and run before any
/// later user scripts in `<head>`.
const PLUGIN_HEAD_INJECTION: &str = concat!(
    "<script src=\"./__tauri/fs.js\"></script>",
    "<script src=\"./__tauri/dialog.js\"></script>",
    "<script src=\"./__tauri/haptics.js\"></script>",
);

/// Extra script tag injected only in dev mode. Polls a marker file in
/// `dist/__tauri/reload-token` and reloads the webview when its contents
/// change. The token file is bumped by `dev` after each scaffold refresh.
const LIVERELOAD_INJECTION: &str = "<script src=\"./__tauri/livereload.js\"></script>";

fn head_injection(dev_mode: bool) -> String {
    let mut s = String::from(PLUGIN_HEAD_INJECTION);
    if dev_mode {
        s.push_str(LIVERELOAD_INJECTION);
    }
    s
}

/// Shared, mutable accumulator for raw asset references collected during the
/// `lol_html` pass. The `'static` element handlers force interior mutability.
type RefSink = Rc<RefCell<Vec<String>>>;

fn new_sink() -> RefSink {
    Rc::new(RefCell::new(Vec::new()))
}

fn push_ref(sink: &RefSink, value: impl Into<String>) {
    sink.borrow_mut().push(value.into());
}

/// Scan an `index.html` for local asset references. Absolute paths are
/// rewritten to relative form so Tauri resolves them inside the bundled
/// `frontendDist`.
pub fn discover(index_path: &Path, source_root: &Path, cfg: &Config) -> Result<Discovered> {
    discover_with_mode(index_path, source_root, cfg, false)
}

pub fn discover_with_mode(
    index_path: &Path,
    source_root: &Path,
    cfg: &Config,
    dev_mode: bool,
) -> Result<Discovered> {
    let html_bytes = std::fs::read(index_path)
        .with_context(|| format!("read failed: {}", index_path.display()))?;

    let sink = new_sink();
    let rewritten = rewrite_html(&html_bytes, &sink, dev_mode)?;
    let raw_refs = sink.take();

    let mut assets = resolve_references(&raw_refs, source_root);
    apply_include_globs(&cfg.include, source_root, &mut assets);

    Ok(Discovered {
        index_html: rewritten,
        assets,
        source_root: source_root.to_path_buf(),
    })
}

fn rewrite_html(html_bytes: &[u8], sink: &RefSink, dev_mode: bool) -> Result<Vec<u8>> {
    let injected = Rc::new(RefCell::new(false));
    let selectors = build_selectors()?;
    let handlers = build_handlers(sink, &injected, dev_mode);

    let element_content_handlers: Vec<(Cow<'_, Selector>, ElementContentHandlers<'_>)> = selectors
        .iter()
        .zip(handlers)
        .map(|(s, h)| (Cow::Borrowed(s), h))
        .collect();

    let mut out = Vec::new();
    let mut rewriter = HtmlRewriter::new(
        Settings {
            element_content_handlers,
            ..Settings::default()
        },
        |c: &[u8]| out.extend_from_slice(c),
    );
    rewriter.write(html_bytes)?;
    rewriter.end()?;

    // Fallback for HTML without a literal <head> (lol_html doesn't synthesize
    // one). Browsers will fold a leading <script> into a synthetic head.
    if !*injected.borrow() {
        let inj = head_injection(dev_mode);
        let mut prefixed = inj.into_bytes();
        prefixed.extend_from_slice(&out);
        out = prefixed;
    }
    Ok(out)
}

fn build_selectors() -> Result<Vec<Selector>> {
    let mut selectors = Vec::with_capacity(ATTR_TARGETS.len() + SRCSET_TARGETS.len() + 2);
    for (sel, _) in ATTR_TARGETS {
        selectors.push(parse_selector(sel)?);
    }
    for sel in SRCSET_TARGETS {
        selectors.push(parse_selector(sel)?);
    }
    selectors.push(parse_selector("style")?);
    selectors.push(parse_selector("head")?);
    Ok(selectors)
}

fn build_handlers(
    sink: &RefSink,
    injected: &Rc<RefCell<bool>>,
    dev_mode: bool,
) -> Vec<ElementContentHandlers<'static>> {
    let mut handlers = Vec::with_capacity(ATTR_TARGETS.len() + SRCSET_TARGETS.len() + 2);
    for (_, attr) in ATTR_TARGETS {
        handlers.push(attr_handler(attr, sink.clone()));
    }
    for _ in SRCSET_TARGETS {
        handlers.push(srcset_handler(sink.clone()));
    }
    handlers.push(style_handler(sink.clone()));
    handlers.push(head_injection_handler(injected.clone(), dev_mode));
    handlers
}

fn parse_selector(sel: &str) -> Result<Selector> {
    sel.parse()
        .map_err(|e| anyhow!("bad selector {}: {:?}", sel, e))
}

type HandlerError = Box<dyn std::error::Error + Send + Sync>;

fn attr_handler(attr: &'static str, sink: RefSink) -> ElementContentHandlers<'static> {
    ElementContentHandlers::default().element(move |el: &mut Element| -> Result<(), HandlerError> {
        if let Some(val) = el.get_attribute(attr) {
            push_ref(&sink, val.clone());
            if let Some(rewritten) = rewrite_attr_value(&val) {
                el.set_attribute(attr, &rewritten)?;
            }
        }
        Ok(())
    })
}

fn srcset_handler(sink: RefSink) -> ElementContentHandlers<'static> {
    ElementContentHandlers::default().element(move |el: &mut Element| -> Result<(), HandlerError> {
        if let Some(val) = el.get_attribute("srcset") {
            for cand in val.split(',') {
                let url = cand.trim().split_ascii_whitespace().next().unwrap_or("");
                if !url.is_empty() {
                    push_ref(&sink, url);
                }
            }
            if let Some(rewritten) = rewrite_srcset_value(&val) {
                el.set_attribute("srcset", &rewritten)?;
            }
        }
        Ok(())
    })
}

fn head_injection_handler(
    injected: Rc<RefCell<bool>>,
    dev_mode: bool,
) -> ElementContentHandlers<'static> {
    let inj = head_injection(dev_mode);
    ElementContentHandlers::default().element(move |el: &mut Element| -> Result<(), HandlerError> {
        el.prepend(&inj, ContentType::Html);
        *injected.borrow_mut() = true;
        Ok(())
    })
}

fn style_handler(sink: RefSink) -> ElementContentHandlers<'static> {
    // <style> can arrive as multiple TextChunks; buffer and process at end.
    let buf: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
    ElementContentHandlers::default().text(move |t: &mut TextChunk| -> Result<(), HandlerError> {
        buf.borrow_mut().push_str(t.as_str());
        if t.last_in_text_node() {
            let css = std::mem::take(&mut *buf.borrow_mut());
            let (rewritten, urls) = css::process(&css, rewrite_attr_value);
            for u in urls {
                push_ref(&sink, u);
            }
            t.replace(&rewritten, ContentType::Text);
        } else {
            t.remove();
        }
        Ok(())
    })
}

fn resolve_references(raw_refs: &[String], source_root: &Path) -> BTreeSet<PathBuf> {
    let mut assets = BTreeSet::new();
    for raw in raw_refs {
        if is_external(raw) {
            continue;
        }
        let trimmed = strip_query_fragment(raw);
        if trimmed.is_empty() {
            continue;
        }
        let Some(rel) = resolve_local(trimmed) else {
            eprintln!("warning: skipping reference outside source root: {}", raw);
            continue;
        };
        let abs = source_root.join(&rel);
        if !abs.exists() {
            eprintln!("warning: referenced asset not found: {}", raw);
            continue;
        }
        if abs.is_dir() {
            continue;
        }
        assets.insert(rel);
    }
    assets
}

fn apply_include_globs(patterns: &[String], source_root: &Path, assets: &mut BTreeSet<PathBuf>) {
    if patterns.is_empty() {
        return;
    }
    for entry in WalkDir::new(source_root).into_iter().flatten() {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(source_root) else {
            continue;
        };
        let rel_str = rel.to_string_lossy();
        if patterns.iter().any(|p| glob::glob_match(p, &rel_str)) {
            assets.insert(rel.to_path_buf());
        }
    }
}

fn is_external(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("http://")
        || s.starts_with("https://")
        || s.starts_with("//")
        || s.starts_with("data:")
        || s.starts_with("mailto:")
        || s.starts_with("tel:")
        || s.starts_with("javascript:")
        || s.starts_with('#')
}

fn strip_query_fragment(s: &str) -> &str {
    let s = s.trim();
    let s = s.split('#').next().unwrap_or(s);
    s.split('?').next().unwrap_or(s)
}

/// Resolve a local reference (with or without a leading `/`) into a path
/// *relative to the source root*. Returns `None` if `..` segments would
/// escape the root — catches traversal that lexical join + `strip_prefix`
/// alone would miss.
fn resolve_local(href: &str) -> Option<PathBuf> {
    let raw = href.strip_prefix('/').unwrap_or(href);
    lexical_normalize(Path::new(raw))
}

/// Resolve `.` and `..` lexically. Returns `None` if `..` escapes the root.
/// We don't use `Path::canonicalize` because the path may not exist yet (we
/// also want to detect missing-asset traversal attempts).
fn lexical_normalize(rel: &Path) -> Option<PathBuf> {
    let mut out: Vec<&std::ffi::OsStr> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => return None,
            Component::CurDir => continue,
            Component::ParentDir => {
                out.pop()?;
            }
            Component::Normal(s) => out.push(s),
        }
    }
    let mut p = PathBuf::new();
    for s in out {
        p.push(s);
    }
    Some(p)
}

fn rewrite_attr_value(value: &str) -> Option<String> {
    if is_external(value) {
        return None;
    }
    value.strip_prefix('/').map(|stripped| format!("./{}", stripped))
}

fn rewrite_srcset_value(value: &str) -> Option<String> {
    let mut changed = false;
    let parts: Vec<String> = value
        .split(',')
        .map(|cand| {
            let cand = cand.trim();
            let mut bits = cand.splitn(2, |c: char| c.is_ascii_whitespace());
            let url = bits.next().unwrap_or("");
            let rest = bits.next().unwrap_or("").trim();
            let new_url = match rewrite_attr_value(url) {
                Some(v) => {
                    changed = true;
                    v
                }
                None => url.to_string(),
            };
            if rest.is_empty() {
                new_url
            } else {
                format!("{} {}", new_url, rest)
            }
        })
        .collect();
    changed.then(|| parts.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_detection() {
        assert!(is_external("https://cdn.example.com/x.js"));
        assert!(is_external("http://cdn.example.com/x.js"));
        assert!(is_external("//cdn.example.com/x.js"));
        assert!(is_external("data:image/png;base64,xxx"));
        assert!(is_external("mailto:a@b.c"));
        assert!(is_external("#anchor"));
        assert!(!is_external("/local/path.js"));
        assert!(!is_external("relative/path.js"));
    }

    #[test]
    fn strip_query_and_fragment() {
        assert_eq!(strip_query_fragment("a.js"), "a.js");
        assert_eq!(strip_query_fragment("a.js?v=1"), "a.js");
        assert_eq!(strip_query_fragment("a.js#frag"), "a.js");
        assert_eq!(strip_query_fragment("a.js?v=1#frag"), "a.js");
    }

    #[test]
    fn rewrite_attr_keeps_externals_alone() {
        assert_eq!(rewrite_attr_value("https://x.y/z.js"), None);
        assert_eq!(rewrite_attr_value("relative/x.js"), None);
        assert_eq!(rewrite_attr_value("/x.js"), Some("./x.js".into()));
    }

    #[test]
    fn rewrite_srcset_only_when_changed() {
        assert_eq!(rewrite_srcset_value("a.png 1x, b.png 2x"), None);
        assert_eq!(
            rewrite_srcset_value("/a.png 1x, b.png 2x"),
            Some("./a.png 1x, b.png 2x".into())
        );
    }

    #[test]
    fn lexical_normalize_blocks_traversal() {
        assert!(lexical_normalize(Path::new("../etc/passwd")).is_none());
        assert!(lexical_normalize(Path::new("a/../../etc")).is_none());
        assert_eq!(
            lexical_normalize(Path::new("a/./b")).as_deref(),
            Some(Path::new("a/b"))
        );
        assert_eq!(
            lexical_normalize(Path::new("a/b/../c")).as_deref(),
            Some(Path::new("a/c"))
        );
    }

    #[test]
    fn resolve_local_blocks_traversal() {
        // Absolute-style local refs (`/x`) get the leading slash stripped.
        assert_eq!(resolve_local("/x.js").as_deref(), Some(Path::new("x.js")));
        // `..` that escapes the root is rejected.
        assert!(resolve_local("../escape.js").is_none());
        assert!(resolve_local("/a/../../etc").is_none());
        // `..` that stays within the root resolves cleanly.
        assert_eq!(resolve_local("a/b/../c").as_deref(), Some(Path::new("a/c")));
    }

    #[test]
    fn injects_plugin_scripts_into_head() {
        let html = b"<!doctype html><html><head><title>x</title></head><body></body></html>";
        let sink = new_sink();
        let out = rewrite_html(html, &sink, false).unwrap();
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains(r#"<script src="./__tauri/fs.js"></script>"#));
        assert!(s.contains(r#"<script src="./__tauri/dialog.js"></script>"#));
        assert!(s.contains(r#"<script src="./__tauri/haptics.js"></script>"#));
        assert!(!s.contains("livereload.js"));
        let plug_idx = s.find("__tauri/fs.js").unwrap();
        let title_idx = s.find("<title>").unwrap();
        assert!(plug_idx < title_idx, "plugin scripts must precede <title>");
    }

    #[test]
    fn injects_when_head_missing() {
        let html = b"<!doctype html><html><body></body></html>";
        let sink = new_sink();
        let out = rewrite_html(html, &sink, false).unwrap();
        let s = std::str::from_utf8(&out).unwrap();
        assert!(s.contains("__tauri/fs.js"));
        assert!(s.contains("__tauri/dialog.js"));
        assert!(s.contains("__tauri/haptics.js"));
    }

    #[test]
    fn injects_livereload_only_in_dev_mode() {
        let html = b"<!doctype html><html><head></head><body></body></html>";
        let sink = new_sink();
        let dev = rewrite_html(html, &sink, true).unwrap();
        assert!(std::str::from_utf8(&dev).unwrap().contains("__tauri/livereload.js"));

        let sink = new_sink();
        let prod = rewrite_html(html, &sink, false).unwrap();
        assert!(!std::str::from_utf8(&prod).unwrap().contains("livereload.js"));
    }
}
