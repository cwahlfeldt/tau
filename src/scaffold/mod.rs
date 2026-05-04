//! Generate a minimal Tauri v2 project on disk.
//!
//! Layout written into the temp project dir:
//! ```text
//! <tmp>/
//!   dist/                     # bundled web app (Tauri frontendDist)
//!     index.html              # rewritten by `discover`
//!     __tauri/                # injected plugin IIFE bundles
//!     <user assets>
//!   src-tauri/
//!     Cargo.toml
//!     tauri.conf.json
//!     build.rs
//!     src/{main,lib}.rs
//!     capabilities/{default,mobile}.json
//!     icons/icon.png
//! ```

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::discover::Discovered;

const CARGO_TMPL: &str = include_str!("templates/Cargo.toml.tmpl");
const MAIN_TMPL: &str = include_str!("templates/main.rs.tmpl");
const LIB_TMPL: &str = include_str!("templates/lib.rs.tmpl");
const BUILD_TMPL: &str = include_str!("templates/build.rs.tmpl");
const ICON_PNG: &[u8] = include_bytes!("templates/icon.png");

const PLUGIN_FS_JS: &[u8] = include_bytes!("templates/plugins/fs.iife.js");
const PLUGIN_DIALOG_JS: &[u8] = include_bytes!("templates/plugins/dialog.iife.js");
const PLUGIN_HAPTICS_JS: &[u8] = include_bytes!("templates/plugins/haptics.iife.js");

/// Plugin IIFE bundles emitted into `dist/__tauri/` and loaded by injected
/// `<script>` tags in the wrapped app's `<head>`. Filenames here MUST match
/// the script tags injected by `discover::PLUGIN_HEAD_INJECTION`.
const PLUGIN_BUNDLES: &[(&str, &[u8])] = &[
    ("fs.js", PLUGIN_FS_JS),
    ("dialog.js", PLUGIN_DIALOG_JS),
    ("haptics.js", PLUGIN_HAPTICS_JS),
];

const PLUGIN_DIR_NAME: &str = "__tauri";

/// All paths inside the generated scaffold, derived once from the project root.
struct Layout {
    dist: PathBuf,
    plugins: PathBuf,
    src_tauri: PathBuf,
    src_tauri_src: PathBuf,
    capabilities: PathBuf,
    icons: PathBuf,
}

impl Layout {
    fn new(project_dir: &Path) -> Self {
        let dist = project_dir.join("dist");
        let src_tauri = project_dir.join("src-tauri");
        Self {
            plugins: dist.join(PLUGIN_DIR_NAME),
            dist,
            src_tauri_src: src_tauri.join("src"),
            capabilities: src_tauri.join("capabilities"),
            icons: src_tauri.join("icons"),
            src_tauri,
        }
    }

    fn ensure_dirs(&self) -> Result<()> {
        for dir in [
            &self.dist,
            &self.plugins,
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

pub fn create(project_dir: &Path, cfg: &Config, discovered: &Discovered) -> Result<()> {
    let layout = Layout::new(project_dir);
    layout.ensure_dirs()?;

    write_frontend(&layout, discovered)?;
    write_src_tauri(&layout, cfg)?;
    Ok(())
}

fn write_frontend(layout: &Layout, discovered: &Discovered) -> Result<()> {
    write_bytes(&layout.dist.join("index.html"), &discovered.index_html)?;
    copy_assets(&layout.dist, discovered)?;
    write_plugin_bundles(&layout.plugins)?;
    Ok(())
}

fn write_src_tauri(layout: &Layout, cfg: &Config) -> Result<()> {
    // The wrapped crate uses a fixed name (`tau_app`) so artifacts in
    // the shared CARGO_TARGET_DIR can be reused across different wrapped apps.
    // Per-app branding lives in `tauri.conf.json` (`productName`/`identifier`),
    // not in the Rust crate identity.
    write_text(
        &layout.src_tauri.join("Cargo.toml"),
        &render(CARGO_TMPL, &[("version", &cfg.version)]),
    )?;
    write_text(&layout.src_tauri.join("build.rs"), BUILD_TMPL)?;
    write_text(&layout.src_tauri_src.join("main.rs"), MAIN_TMPL)?;
    write_text(&layout.src_tauri_src.join("lib.rs"), LIB_TMPL)?;
    write_json(&layout.src_tauri.join("tauri.conf.json"), &tauri_conf(cfg))?;
    write_json(&layout.capabilities.join("default.json"), &default_capability())?;
    write_json(&layout.capabilities.join("mobile.json"), &mobile_capability())?;
    write_bytes(&layout.icons.join("icon.png"), ICON_PNG)?;
    Ok(())
}

fn write_plugin_bundles(plugin_dir: &Path) -> Result<()> {
    for (filename, bytes) in PLUGIN_BUNDLES {
        write_bytes(&plugin_dir.join(filename), bytes)?;
    }
    Ok(())
}

fn copy_assets(dist_dir: &Path, discovered: &Discovered) -> Result<()> {
    for rel in &discovered.assets {
        if is_root_index(rel) {
            continue;
        }
        let from = discovered.source_root.join(rel);
        let to = dist_dir.join(rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if from.is_file() {
            std::fs::copy(&from, &to)
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

/// `index.html` was already written from rewritten bytes in `write_frontend`.
fn is_root_index(rel: &Path) -> bool {
    let at_root = rel.parent().is_none_or(|p| p.as_os_str().is_empty());
    at_root && rel.file_name().and_then(|s| s.to_str()) == Some("index.html")
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

fn tauri_conf(cfg: &Config) -> Value {
    // Skip installer formats (dmg/msi/etc.) — we only want the runnable app.
    // The dmg bundler in particular spawns bundle_dmg.sh which mounts and
    // opens a disk image, which is intrusive.
    json!({
        "$schema": "https://schema.tauri.app/config/2",
        "productName": cfg.name,
        "version": cfg.version,
        "identifier": cfg.identifier,
        "build": { "frontendDist": "../dist" },
        "app": {
            "windows": [{
                "label": "main",
                "title": cfg.name,
                "width": 1024,
                "height": 768,
                "resizable": true,
                "fullscreen": false
            }],
            "security": { "csp": null },
            "withGlobalTauri": true
        },
        "bundle": {
            "active": true,
            "targets": ["app", "appimage", "nsis"],
            "icon": ["icons/icon.png"]
        }
    })
}

fn default_capability() -> Value {
    json!({
        "$schema": "../gen/schemas/desktop-schema.json",
        "identifier": "default",
        "description": "Default capability for the wrapped app",
        "windows": ["main"],
        "permissions": ["core:default", "fs:default", "dialog:default"]
    })
}

/// Mobile-only permissions live in a platform-scoped capability. Putting
/// haptics permissions in the desktop capability would fail `tauri-build` —
/// the haptics crate isn't compiled in for desktop targets, so the
/// permission IDs aren't registered.
///
/// Note: `tauri-plugin-haptics` does not ship a `default.toml` permission
/// set (unlike fs/dialog), so we list each command-level allow explicitly.
fn mobile_capability() -> Value {
    json!({
        "$schema": "../gen/schemas/mobile-schema.json",
        "identifier": "mobile",
        "description": "Mobile-only capabilities for the wrapped app",
        "windows": ["main"],
        "platforms": ["iOS", "android"],
        "permissions": [
            "haptics:allow-vibrate",
            "haptics:allow-impact-feedback",
            "haptics:allow-notification-feedback",
            "haptics:allow-selection-feedback"
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_vars() {
        assert_eq!(render("hi {name}", &[("name", "bob")]), "hi bob");
        assert_eq!(render("{a}/{b}", &[("a", "x"), ("b", "y")]), "x/y");
    }

    #[test]
    fn default_capability_includes_fs_and_dialog_not_haptics() {
        let v = default_capability();
        let perms: Vec<&str> = v["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        assert!(perms.contains(&"core:default"));
        assert!(perms.contains(&"fs:default"));
        assert!(perms.contains(&"dialog:default"));
        assert!(!perms.contains(&"haptics:default"));
    }

    #[test]
    fn mobile_capability_is_platform_scoped() {
        let v = mobile_capability();
        assert_eq!(v["platforms"], serde_json::json!(["iOS", "android"]));
        let perms: Vec<&str> = v["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap())
            .collect();
        // tauri-plugin-haptics has no `default` permission set, so we list
        // each command-level allow explicitly.
        assert!(perms.contains(&"haptics:allow-vibrate"));
        assert!(perms.contains(&"haptics:allow-impact-feedback"));
        assert!(perms.contains(&"haptics:allow-notification-feedback"));
        assert!(perms.contains(&"haptics:allow-selection-feedback"));
        assert!(!perms.contains(&"haptics:default"));
    }

    #[test]
    fn tauri_conf_enables_global_tauri() {
        use crate::config::{BuildProfile, Config};
        let cfg = Config {
            name: "X".into(),
            version: "0.1.0".into(),
            identifier: "com.x".into(),
            include: vec![],
            output: ".".into(),
            platforms: vec![],
            profile: BuildProfile::Debug,
        };
        let v = tauri_conf(&cfg);
        assert_eq!(v["app"]["withGlobalTauri"], serde_json::json!(true));
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
    fn plugin_bundles_are_nonempty_and_self_contained() {
        for (name, bytes) in PLUGIN_BUNDLES {
            assert!(bytes.len() > 100, "{} bundle is suspiciously small", name);
            let s = std::str::from_utf8(bytes).expect("plugin bundle is utf-8");
            assert!(
                !s.contains("require("),
                "{} bundle contains unresolved require()",
                name
            );
        }
    }

    #[test]
    fn is_root_index_only_for_top_level() {
        assert!(is_root_index(Path::new("index.html")));
        assert!(!is_root_index(Path::new("sub/index.html")));
        assert!(!is_root_index(Path::new("other.html")));
    }
}
