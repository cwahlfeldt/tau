//! `tau create <name>` — scaffold a new game project on disk.
//!
//! The on-disk shape is deliberately split into "user-facing" and "tooling":
//!
//! ```text
//! <name>/
//!   src/                <- user edits these
//!     index.html
//!     game.js
//!     assets/
//!   tau.conf.json       <- minimal: name + identifier
//!   .gitignore
//!   .tau/               <- hidden; tau owns this
//!     package.json
//!     vite.config.js
//!     pnpm-workspace.yaml
//!     node_modules/     <- populated by `<pm> install` below
//! ```

use anyhow::{bail, Context, Result};
use serde_json::json;
use std::path::Path;

use crate::config;
use crate::log::Logger;
use crate::scaffold;
use crate::tooling;

pub fn run(name: String, log: &Logger) -> Result<()> {
    validate_name(&name)?;
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let target = cwd.join(&name);
    if target.exists() && !is_empty_dir(&target)? {
        bail!(
            "`{}` already exists and is not empty — pick a different name or remove it first",
            target.display()
        );
    }

    log.heading(&format!("Creating tau project: {}", name));
    log.detail("path", &target.display().to_string());

    // Preflight: bail before we write anything if the toolchain is missing.
    tooling::ensure_node_present()?;
    let pm = tooling::detect_package_manager()?;
    log.detail("package manager", pm.label());

    write_template_tree(&target, &name)?;
    log.detail("scaffolded", "src/, .tau/, .gitignore");

    let tau_dir = target.join(".tau");
    tooling::install(pm, &tau_dir, log).context("install JS dependencies in .tau/")?;

    symlink_node_modules(&target, &tau_dir)
        .context("create node_modules symlink at project root")?;

    log.done(&format!(
        "Created {}\n\nNext:\n    cd {}\n    tau dev",
        target.display(),
        name
    ));
    Ok(())
}

/// Symlink `<project>/node_modules` → `.tau/node_modules` so TypeScript and
/// editor tooling find packages (and their `@types/*` sidecars) via normal
/// node_modules walk-up from `src/`. Vite doesn't need this — it resolves
/// modules through explicit aliases in `.tau/vite.config.js` — but tsserver,
/// eslint, and any IDE-level resolver does. Without it, `import { useRef }
/// from 'react'` shows as "Cannot find module" even though the package is
/// installed in `.tau/node_modules`.
///
/// `.gitignore` already excludes `.tau/node_modules/`; the symlink itself is
/// covered because git treats it as a regular ignored file at the same path.
fn symlink_node_modules(project_root: &Path, tau_dir: &Path) -> Result<()> {
    let link = project_root.join("node_modules");
    if link.exists() || link.is_symlink() {
        return Ok(());
    }
    let target = tau_dir.join("node_modules");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, &link)
            .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))?;
    }
    #[cfg(windows)]
    {
        // Directory symlink on Windows. Falls back to a junction-like behavior
        // for users without symlink privileges (Developer Mode disabled).
        std::os::windows::fs::symlink_dir(&target, &link)
            .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))?;
    }
    Ok(())
}

fn write_template_tree(target: &Path, name: &str) -> Result<()> {
    let src_dir = target.join("src");
    let assets_dir = src_dir.join("assets");
    let tau_dir = target.join(".tau");
    for d in [&src_dir, &assets_dir, &tau_dir] {
        std::fs::create_dir_all(d).with_context(|| format!("create dir {}", d.display()))?;
    }

    // index.html: user-visible <title>.
    scaffold::write_text(
        &src_dir.join("index.html"),
        &scaffold::render(scaffold::GAME_INDEX_HTML_TMPL, &[("name", name)]),
    )?;

    // game.tsx: static, identical for every project. Vite handles TS/TSX
    // natively via esbuild + @vitejs/plugin-react for the JSX transform.
    scaffold::write_text(&src_dir.join("game.tsx"), scaffold::GAME_GAME_TSX)?;

    // package.json: npm-safe slug of the user's name. The `name` field has
    // strict rules (lowercase, no spaces, no `@` etc. unless scoped); using
    // the user's raw input would frequently fail at install time.
    let pkg_slug = npm_slug(name);
    scaffold::write_text(
        &tau_dir.join("package.json"),
        &scaffold::render(scaffold::GAME_PACKAGE_JSON_TMPL, &[("slug", &pkg_slug)]),
    )?;

    scaffold::write_text(&tau_dir.join("vite.config.js"), scaffold::GAME_VITE_CONFIG)?;

    // TypeScript support: `tsconfig.json` lives at the project root so
    // tsserver auto-discovers it when the user opens any file in `src/`.
    // `paths` redirects bare imports to `.tau/node_modules/*` (TS can't
    // walk into `.tau/` on its own since it's not a parent of `src/`).
    // `tau.d.ts` is the ambient declaration for the `tau` virtual module
    // emitted by the Vite plugin in vite.config.js — kept inside `.tau/`
    // so users don't see it.
    scaffold::write_text(&target.join("tsconfig.json"), scaffold::GAME_TSCONFIG)?;
    scaffold::write_text(&tau_dir.join("tau.d.ts"), scaffold::GAME_TAU_DTS)?;

    // pnpm-workspace.yaml is what unblocks the build-script gate pnpm 10+
    // applies to esbuild (transitively pulled in by Vite). Without this,
    // every `pnpm install` and `pnpm run dev` exits non-zero. npm ignores
    // this file, so it's safe to write unconditionally.
    scaffold::write_text(
        &tau_dir.join("pnpm-workspace.yaml"),
        scaffold::GAME_PNPM_WORKSPACE,
    )?;

    // `.gitignore` lives at the project root so `git init` in a fresh project
    // immediately ignores node_modules etc. We write it via `write_text` and
    // include it via `include_str!` (named `gitignore`, not `.gitignore`,
    // because cargo packages ignore dotfiles in src/).
    scaffold::write_text(&target.join(".gitignore"), scaffold::GAME_GITIGNORE)?;

    // Minimal tau.conf.json so the project's name/identifier are pinned in
    // source control rather than re-derived from the directory name on every
    // build. Users who want more (version, platforms, signing) edit this file.
    let conf = json!({
        "name": name,
        "identifier": config::default_identifier(name),
    });
    let pretty = serde_json::to_string_pretty(&conf)
        .context("encode tau.conf.json")?;
    scaffold::write_text(&target.join("tau.conf.json"), &format!("{}\n", pretty))?;

    Ok(())
}

/// Validate the user's project name. Rules:
/// - non-empty
/// - no path separators (no `..`, no `/`, no `\`)
/// - no leading dot (npm rejects names starting with `.` or `_`)
/// - cannot be `.` or `..`
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("project name cannot be empty");
    }
    if name == "." || name == ".." {
        bail!("project name cannot be `.` or `..`");
    }
    if name.contains('/') || name.contains('\\') {
        bail!("project name cannot contain path separators: `{}`", name);
    }
    if name.starts_with('.') {
        bail!("project name cannot start with a dot: `{}`", name);
    }
    Ok(())
}

/// Reduce a project name to a valid npm package name fragment: lowercase,
/// alphanumerics + hyphens. Falls back to `tau-app` if the input is all
/// special characters.
fn npm_slug(name: &str) -> String {
    let mut s: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.starts_with('-') {
        s.remove(0);
    }
    while s.ends_with('-') {
        s.pop();
    }
    if s.is_empty() {
        "tau-app".to_string()
    } else {
        s
    }
}

fn is_empty_dir(path: &Path) -> Result<bool> {
    if !path.is_dir() {
        // A file at the target path is not an "empty directory" — caller
        // treats this the same as "occupied".
        return Ok(false);
    }
    let mut iter = std::fs::read_dir(path)
        .with_context(|| format!("read dir {}", path.display()))?;
    Ok(iter.next().is_none())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_name_accepts_normal() {
        assert!(validate_name("my-game").is_ok());
        assert!(validate_name("game1").is_ok());
        assert!(validate_name("MyGame").is_ok());
    }

    #[test]
    fn validate_name_rejects_path_separators() {
        assert!(validate_name("foo/bar").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name(".").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name(".hidden").is_err());
    }

    #[test]
    fn npm_slug_lowercases_and_dashes() {
        assert_eq!(npm_slug("My Game"), "my-game");
        assert_eq!(npm_slug("My_Game!"), "my-game");
        assert_eq!(npm_slug("hello"), "hello");
        assert_eq!(npm_slug("---hi---"), "hi");
        assert_eq!(npm_slug("!!!"), "tau-app");
    }

    #[test]
    fn write_template_tree_produces_expected_files() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("demo");
        std::fs::create_dir(&target).unwrap();
        write_template_tree(&target, "Demo").unwrap();

        assert!(target.join("src").join("index.html").is_file());
        assert!(target.join("src").join("game.tsx").is_file());
        assert!(target.join("src").join("assets").is_dir());
        assert!(target.join(".tau").join("package.json").is_file());
        assert!(target.join(".tau").join("vite.config.js").is_file());
        assert!(target.join("tsconfig.json").is_file());
        assert!(target.join(".tau").join("tau.d.ts").is_file());
        assert!(target.join(".tau").join("pnpm-workspace.yaml").is_file());
        assert!(target.join(".gitignore").is_file());
        assert!(target.join("tau.conf.json").is_file());

        let html = std::fs::read_to_string(target.join("src").join("index.html")).unwrap();
        assert!(html.contains("<title>Demo</title>"), "title not substituted: {}", html);
        assert!(html.contains("./game.tsx"), "index.html should reference game.tsx: {}", html);
        assert!(html.contains("id=\"root\""), "React root mount point missing from index.html: {}", html);

        let pkg = std::fs::read_to_string(target.join(".tau").join("package.json")).unwrap();
        assert!(pkg.contains("\"name\": \"demo\""), "npm name not substituted: {}", pkg);
        assert!(pkg.contains("\"three\""));
        assert!(pkg.contains("\"react\""));
        assert!(pkg.contains("\"@react-three/fiber\""));
        assert!(pkg.contains("\"@react-three/drei\""));
        assert!(!pkg.contains("\"bitecs\""), "bitecs should not be in package.json after r3f pivot: {}", pkg);
        assert!(pkg.contains("\"@tauri-apps/plugin-fs\""));
        assert!(pkg.contains("\"@tauri-apps/plugin-dialog\""));
        assert!(pkg.contains("\"@tauri-apps/plugin-notification\""));
        assert!(pkg.contains("\"@tauri-apps/plugin-haptics\""));
        assert!(pkg.contains("\"@vitejs/plugin-react\""));
        assert!(pkg.contains("\"typescript\""));
        assert!(pkg.contains("\"vite\""));

        // The conf carries the project's *display* name and identifier. The
        // identifier is derived from the same slug logic config::resolve uses.
        let conf: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(target.join("tau.conf.json")).unwrap())
                .unwrap();
        assert_eq!(conf["name"], "Demo");
        assert_eq!(conf["identifier"], "com.tau.demo");
    }
}
