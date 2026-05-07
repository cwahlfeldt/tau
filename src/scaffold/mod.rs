//! Generate a minimal Tauri v2 project on disk.
//!
//! We don't bundle the user's frontend into a `dist/` of our own — we just
//! point Tauri's `frontendDist` at the source directory the user gave us.
//! Tauri serves the files (with correct MIME types) and bundles the whole
//! tree into the platform package. No HTML rewriting, no asset discovery,
//! no plugin shims.
//!
//! Layout written into the temp project dir:
//! ```text
//! <tmp>/
//!   src-tauri/
//!     Cargo.toml
//!     tauri.conf.json   # frontendDist points at user's source dir
//!     build.rs
//!     src/{main,lib}.rs
//!     capabilities/default.json
//!     icons/icon.png
//! ```

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::config::Config;

const CARGO_TMPL: &str = include_str!("templates/Cargo.toml.tmpl");
const MAIN_TMPL: &str = include_str!("templates/main.rs.tmpl");
const LIB_TMPL: &str = include_str!("templates/lib.rs.tmpl");
const BUILD_TMPL: &str = include_str!("templates/build.rs.tmpl");
const ICON_PNG: &[u8] = include_bytes!("templates/icon.png");

/// All paths inside the generated scaffold, derived once from the project root.
struct Layout {
    src_tauri: PathBuf,
    src_tauri_src: PathBuf,
    capabilities: PathBuf,
    icons: PathBuf,
}

impl Layout {
    fn new(project_dir: &Path) -> Self {
        let src_tauri = project_dir.join("src-tauri");
        Self {
            src_tauri_src: src_tauri.join("src"),
            capabilities: src_tauri.join("capabilities"),
            icons: src_tauri.join("icons"),
            src_tauri,
        }
    }

    fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            &self.src_tauri,
            &self.src_tauri_src,
            &self.capabilities,
            &self.icons,
        ] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("create dir {}", dir.display()))?;
        }
        Ok(())
    }
}

/// Create a Tauri scaffold whose `frontendDist` points at `source_root`.
/// Tauri reads files directly from there at build time and at dev time.
pub fn create_for_source(project_dir: &Path, cfg: &Config, source_root: &Path) -> Result<()> {
    let layout = Layout::new(project_dir);
    layout.ensure_dirs()?;
    write_src_tauri(&layout, cfg, FrontendSource::Local(source_root))?;
    Ok(())
}

/// Scaffold a Tauri project whose webview points at a remote URL. We still
/// need a `frontendDist` that exists at build time (the bundler insists);
/// we use a one-file stub directory inside the scaffold itself.
pub fn create_for_url(project_dir: &Path, cfg: &Config, url: &str) -> Result<()> {
    let layout = Layout::new(project_dir);
    layout.ensure_dirs()?;

    let stub_dir = project_dir.join("dist");
    std::fs::create_dir_all(&stub_dir).with_context(|| format!("create dir {}", stub_dir.display()))?;
    write_bytes(&stub_dir.join("index.html"), URL_STUB_HTML.as_bytes())?;

    write_src_tauri(&layout, cfg, FrontendSource::Url { url, stub_dir: &stub_dir })?;
    Ok(())
}

const URL_STUB_HTML: &str = "<!doctype html><meta charset=\"utf-8\"><title>tau</title>";

enum FrontendSource<'a> {
    Local(&'a Path),
    Url { url: &'a str, stub_dir: &'a Path },
}

fn write_src_tauri(layout: &Layout, cfg: &Config, frontend: FrontendSource<'_>) -> Result<()> {
    // Fixed crate name lets the shared CARGO_TARGET_DIR be reused across
    // wraps. Per-app branding lives in tauri.conf.json (productName/identifier).
    write_text(
        &layout.src_tauri.join("Cargo.toml"),
        &render(CARGO_TMPL, &[("version", &cfg.version)]),
    )?;
    write_text(&layout.src_tauri.join("build.rs"), BUILD_TMPL)?;
    write_text(&layout.src_tauri_src.join("main.rs"), MAIN_TMPL)?;
    write_text(&layout.src_tauri_src.join("lib.rs"), LIB_TMPL)?;
    write_json(&layout.src_tauri.join("tauri.conf.json"), &tauri_conf(cfg, &frontend))?;
    write_json(&layout.capabilities.join("default.json"), &default_capability())?;
    write_bytes(&layout.icons.join("icon.png"), ICON_PNG)?;
    Ok(())
}

fn write_text(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}

fn write_bytes(path: &Path, contents: &[u8]) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("write {}", path.display()))
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let pretty = serde_json::to_string_pretty(value)
        .with_context(|| format!("encode json for {}", path.display()))?;
    write_text(path, &pretty)
}

/// Minimal `{key}` substitution. We don't need a real template engine and a
/// dependency-free helper keeps the JSON/Toml templates readable.
fn render(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{}}}", k), v);
    }
    out
}

fn tauri_conf(cfg: &Config, frontend: &FrontendSource<'_>) -> Value {
    // `withGlobalTauri` exposes core APIs at `window.__TAURI__.*` for plain
    // <script>-loaded code (no bundler required). Plugins are not registered
    // by default — users who want them can scaffold their own Tauri project.
    let mut window = json!({
        "label": "main",
        "title": cfg.name,
        "width": 1024,
        "height": 768,
        "resizable": true,
        "fullscreen": false,
        "titleBarStyle": "Transparent",
        "hiddenTitle": true
    });
    if let FrontendSource::Url { url, .. } = frontend {
        window["url"] = Value::String((*url).to_string());
    }

    let frontend_dist = frontend_dist_value(frontend);

    json!({
        "$schema": "https://schema.tauri.app/config/2",
        "productName": cfg.name,
        "version": cfg.version,
        "identifier": cfg.identifier,
        "build": { "frontendDist": frontend_dist },
        "app": {
            "windows": [window],
            "security": { "csp": null },
            "withGlobalTauri": true
        },
        "bundle": {
            "active": true,
            // Skip installer formats (dmg/msi/etc.) — the dmg bundler in
            // particular mounts and opens a disk image during the build.
            "targets": ["app", "appimage", "nsis"],
            "icon": ["icons/icon.png"]
        }
    })
}

/// `frontendDist` is interpreted relative to `src-tauri/`. Absolute paths
/// work too, which is what we want for both the user's source dir (anywhere
/// on disk) and the URL-mode stub (sibling of `src-tauri/`).
fn frontend_dist_value(frontend: &FrontendSource<'_>) -> String {
    match frontend {
        FrontendSource::Local(p) => p.display().to_string(),
        FrontendSource::Url { stub_dir, .. } => stub_dir.display().to_string(),
    }
}

fn default_capability() -> Value {
    json!({
        "$schema": "../gen/schemas/desktop-schema.json",
        "identifier": "default",
        "description": "Default capability for the wrapped app",
        "windows": ["main"],
        "permissions": ["core:default"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BuildProfile, Config};

    fn sample_cfg() -> Config {
        Config {
            name: "X".into(),
            version: "0.1.0".into(),
            identifier: "com.x".into(),
            output: ".".into(),
            platforms: vec![],
            profile: BuildProfile::Debug,
        }
    }

    #[test]
    fn render_substitutes_vars() {
        assert_eq!(render("hi {name}", &[("name", "bob")]), "hi bob");
        assert_eq!(render("{a}/{b}", &[("a", "x"), ("b", "y")]), "x/y");
    }

    #[test]
    fn default_capability_only_grants_core() {
        let v = default_capability();
        let perms: Vec<&str> = v["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert_eq!(perms, vec!["core:default"]);
    }

    #[test]
    fn tauri_conf_enables_global_tauri() {
        let v = tauri_conf(&sample_cfg(), &FrontendSource::Local(Path::new("/tmp/src")));
        assert_eq!(v["app"]["withGlobalTauri"], serde_json::json!(true));
        assert!(v["app"]["windows"][0].get("url").is_none());
    }

    #[test]
    fn tauri_conf_points_frontend_dist_at_source_root() {
        let v = tauri_conf(&sample_cfg(), &FrontendSource::Local(Path::new("/abs/src")));
        assert_eq!(v["build"]["frontendDist"], serde_json::json!("/abs/src"));
    }

    #[test]
    fn tauri_conf_sets_window_url_for_remote_wrap() {
        let v = tauri_conf(
            &sample_cfg(),
            &FrontendSource::Url { url: "https://example.com", stub_dir: Path::new("/tmp/stub") },
        );
        assert_eq!(v["app"]["windows"][0]["url"], serde_json::json!("https://example.com"));
        assert_eq!(v["build"]["frontendDist"], serde_json::json!("/tmp/stub"));
    }

    #[test]
    fn cargo_template_uses_fixed_crate_name() {
        // The fixed crate name is what enables cache reuse across different
        // wrapped apps. Renaming it would silently re-fragment the cache.
        assert!(CARGO_TMPL.contains("name = \"tau_app\""));
        assert!(MAIN_TMPL.contains("tau_app::run()"));
    }

    #[test]
    fn cargo_template_disables_incremental() {
        assert!(CARGO_TMPL.contains("[profile.dev]"));
        assert!(CARGO_TMPL.contains("incremental = false"));
    }

    #[test]
    fn cargo_template_release_profile_is_size_tuned() {
        // Without these, a release Android APK ships ~80-100 MB of unstripped
        // Rust cdylib per ABI. Removing any of them silently re-bloats output.
        assert!(CARGO_TMPL.contains("[profile.release]"));
        assert!(CARGO_TMPL.contains("strip = true"));
        assert!(CARGO_TMPL.contains("lto = true"));
        assert!(CARGO_TMPL.contains("opt-level = \"s\""));
        assert!(CARGO_TMPL.contains("panic = \"abort\""));
        assert!(CARGO_TMPL.contains("codegen-units = 1"));
    }
}
