//! Node + package manager shim. Owns every shell-out to `node`/`pnpm`/`npm`
//! and to `vite` (via the chosen package manager's `exec` form).
//!
//! Tau itself never installs Node or a package manager — if the user is
//! missing one, we surface a clear error pointing them at the canonical
//! install instructions and bail. The single responsibility of this module
//! is "shell out cleanly and report nicely."

use anyhow::{anyhow, bail, Context, Result};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::input;
use crate::log::Logger;

/// Minimum Node version we expect. We don't pin Vite hard against this — Vite
/// 5 wants Node 18+, so we mirror that. Anything older silently misbehaves
/// with ESM, so it's worth a clear preflight error.
const MIN_NODE_MAJOR: u32 = 18;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Pnpm,
    Npm,
}

impl PackageManager {
    pub fn binary(self) -> &'static str {
        match self {
            PackageManager::Pnpm => "pnpm",
            PackageManager::Npm => "npm",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PackageManager::Pnpm => "pnpm",
            PackageManager::Npm => "npm",
        }
    }
}

/// Pick a package manager. Prefer pnpm (smaller node_modules, faster); fall
/// back to npm. We don't try to bootstrap pnpm on the user's behalf — if both
/// are missing, the user almost certainly has a broken Node install.
pub fn detect_package_manager() -> Result<PackageManager> {
    if has_binary("pnpm") {
        return Ok(PackageManager::Pnpm);
    }
    if has_binary("npm") {
        return Ok(PackageManager::Npm);
    }
    Err(anyhow!(
        "no JavaScript package manager found on PATH (looked for `pnpm` and `npm`).\n\
         Install Node.js 18+ from https://nodejs.org/ — npm ships with it.\n\
         For a faster experience: `npm install -g pnpm`."
    ))
}

/// Verify Node is installed and meets the minimum version. Called before any
/// command that depends on it (create, dev, build, add) so we fail fast with
/// a clear message rather than a confusing `vite: command not found` later.
pub fn ensure_node_present() -> Result<()> {
    let out = Command::new("node")
        .arg("--version")
        .output()
        .map_err(|_| {
            anyhow!(
                "Node.js is not installed (or not on PATH).\n\
                 Install it from https://nodejs.org/ — version {} or newer.",
                MIN_NODE_MAJOR
            )
        })?;
    if !out.status.success() {
        bail!("`node --version` failed: {}", String::from_utf8_lossy(&out.stderr));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let version = stdout.trim().trim_start_matches('v');
    let major: u32 = version
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if major < MIN_NODE_MAJOR {
        bail!(
            "Node {} is too old — tau needs {} or newer.\n\
             Update from https://nodejs.org/.",
            version,
            MIN_NODE_MAJOR
        );
    }
    Ok(())
}

/// Run `<pm> install` inside `cwd`. Streams stdio through. The pnpm 10+
/// `strict-dep-builds` gate (which fails the install when a transitive dep
/// like esbuild wants to run a build script) is handled by the `.npmrc`
/// `tau create` writes into `.tau/` — no per-command flag needed here.
pub fn install(pm: PackageManager, cwd: &Path, log: &Logger) -> Result<()> {
    let label = format!("{} install", pm.label());
    log.command(&label);
    let status = Command::new(pm.binary())
        .arg("install")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("failed to spawn `{}`", pm.label()))?;
    if !status.success() {
        bail!("{} exited with status {}", label, status);
    }
    Ok(())
}

/// Run `<pm> add <pkg>` inside `cwd`. Streams stdio through.
pub fn add(pm: PackageManager, cwd: &Path, pkg: &str, log: &Logger) -> Result<()> {
    let label = format!("{} add {}", pm.label(), pkg);
    log.command(&label);
    let status = Command::new(pm.binary())
        .arg("add")
        .arg(pkg)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .status()
        .with_context(|| format!("failed to spawn `{}`", pm.label()))?;
    if !status.success() {
        bail!("{} exited with status {}", label, status);
    }
    Ok(())
}

/// Spawn a Vite dev server (`<pm> run dev`) in the background. Returns the
/// child handle so the caller can wait/kill alongside `cargo tauri dev`.
/// stdout/stderr inherit — Vite's startup banner and HMR logs are useful
/// and go to the same terminal as Tauri's logs. stdin is redirected from
/// the TTY: Vite's interactive shortcuts (`r/u/o/c/q`) put the terminal
/// into a raw/extended-keys mode that doesn't get fully restored when we
/// kill Vite, leaving the user's terminal echoing CSI escape sequences
/// (`[[99;5u` etc.) on every Ctrl+C until they `reset(1)`. Detaching from
/// the TTY tells Vite to skip raw-mode setup entirely.
pub fn vite_dev(pm: PackageManager, cwd: &Path, log: &Logger) -> Result<Child> {
    let label = format!("{} run dev", pm.label());
    log.command(&label);
    let mut cmd = Command::new(pm.binary());
    cmd.arg("run").arg("dev").current_dir(cwd).stdin(Stdio::null());
    // Put the package manager into its own process group so we can later
    // signal the whole tree (pnpm/npm fork node, which forks vite — killing
    // just the pnpm pid would leave the node grandchild owning port 1420).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().with_context(|| format!("failed to spawn `{}`", pm.label()))
}

/// Run `<pm> run build` inside `cwd`. Streams stdio through.
pub fn vite_build(pm: PackageManager, cwd: &Path, log: &Logger) -> Result<()> {
    let label = format!("{} run build", pm.label());
    log.command(&label);
    let status = Command::new(pm.binary())
        .arg("run")
        .arg("build")
        .current_dir(cwd)
        .status()
        .with_context(|| format!("failed to spawn `{}`", pm.label()))?;
    if !status.success() {
        bail!("{} exited with status {}", label, status);
    }
    Ok(())
}

/// `tau add <pkg>` — discover the project and run `<pm> add <pkg>` in `.tau/`.
pub fn run_add(package: String, log: &Logger) -> Result<()> {
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let project = input::discover_project(&cwd).ok_or_else(|| {
        anyhow!(
            "no tau project found in `{}` or any parent. `tau add` only works inside a project created by `tau create`.",
            cwd.display()
        )
    })?;
    ensure_node_present()?;
    let pm = detect_package_manager()?;
    log.heading(&format!("Adding {}", package));
    log.detail("project", &project.root.display().to_string());
    add(pm, &project.tau_dir, &package, log)?;
    log.done(&format!("Installed {} in {}", package, project.tau_dir.display()));
    Ok(())
}

fn has_binary(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_manager_binary_strings() {
        assert_eq!(PackageManager::Pnpm.binary(), "pnpm");
        assert_eq!(PackageManager::Npm.binary(), "npm");
    }
}
