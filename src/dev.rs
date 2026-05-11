//! `tau dev` — fast iteration via `cargo tauri dev`.
//!
//! Two modes:
//!
//! - **Project mode** (no positional `index`, `discover_project` succeeds):
//!   spawn Vite in `.tau/`, wait for it to come up on 127.0.0.1:1420, then
//!   scaffold a Tauri project whose `devUrl` points at Vite. Tauri loads the
//!   webview from there during dev — Vite handles HMR, asset serving,
//!   bare-import resolution.
//!
//! - **Legacy mode** (positional `index` provided, or no project found):
//!   today's behavior — scaffold pointing `frontendDist` at the user's source
//!   tree, spawn `cargo tauri dev`. No Vite. No HMR (just reload the webview).

use anyhow::{anyhow, bail, Context, Result};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::build::{ensure_targets, MobileFlavor, TauriCmd};
use crate::cache;
use crate::config::{self, Overrides, Platform};
use crate::input::{self, Input, ProjectRoot};
use crate::log::Logger;
use crate::scaffold;
use crate::tooling;

/// Vite's bind. Hard-coded to match the template `vite.config.js`. If we ever
/// make the port configurable, both ends need to move together.
const DEV_HOST: &str = "127.0.0.1";
const DEV_PORT: u16 = 1420;
const DEV_URL: &str = "http://127.0.0.1:1420";

pub struct DevArgs {
    pub index: Option<PathBuf>,
    pub platform: Option<String>,
    pub name: Option<String>,
    pub identifier: Option<String>,
    pub config: Option<PathBuf>,
    pub keep_scaffold: bool,
    pub log: Logger,
}

impl DevArgs {
    fn overrides(&self, platform: Platform) -> Overrides {
        Overrides {
            name: self.name.clone(),
            identifier: self.identifier.clone(),
            output: None,
            config: self.config.clone(),
            platforms: vec![platform.as_str().to_string()],
            release: false,
        }
    }
}

pub fn run(args: DevArgs) -> Result<()> {
    let platform = resolve_platform(args.platform.as_deref())?;

    // Explicit positional argument always wins — if the user passes
    // `tau dev path/to/index.html`, never try to discover.
    if let Some(index_path) = &args.index {
        return run_legacy(&args, index_path.clone(), platform);
    }
    let cwd = std::env::current_dir()?;
    if let Some(project) = input::discover_project(&cwd) {
        return run_project(&args, project, platform);
    }
    Err(anyhow!(
        "no tau project found in `{}` or any parent, and no index path was provided.\n\
         Run `tau create <name>` to scaffold one, or pass `tau dev <path/to/index.html>` to wrap an existing file.",
        cwd.display()
    ))
}

/// Project mode: Vite + Tauri devUrl.
fn run_project(args: &DevArgs, project: ProjectRoot, platform: Platform) -> Result<()> {
    let log = &args.log;
    log.heading("tau dev");
    log.detail("project", &project.root.display().to_string());

    tooling::ensure_node_present()?;
    let pm = tooling::detect_package_manager()?;
    log.detail("package manager", pm.label());

    if !project.tau_dir.join("node_modules").is_dir() {
        log.detail("install", "node_modules missing — installing first");
        tooling::install(pm, &project.tau_dir, log)?;
    }

    // Resolve config relative to the project root, not cwd. This means
    // `tau dev` in a subdirectory still finds tau.conf.json at the root.
    let overrides = args.overrides(platform);
    let mut cfg = config::resolve(&project.root, Some(&project.root), &overrides)?;
    config::apply_project_name_fallback(&mut cfg, &project.root, &overrides);
    cfg.platforms = vec![platform];

    log_header(log, &cfg, platform);

    let workdir = make_workdir("tau-dev-")?;
    let scaffold_dir = workdir.path().to_path_buf();
    scaffold::create_for_dev_server(&scaffold_dir, &cfg, DEV_URL)?;
    log.detail("scaffold", &scaffold_dir.display().to_string());

    ensure_targets(platform, log)?;
    let target_dir = cache::dir()?;

    let shutdown = install_shutdown_flag();

    // Spawn Vite first; Tauri only points the webview at it once it's up.
    log.heading("Starting Vite dev server");
    let mut vite_child = tooling::vite_dev(pm, &project.tau_dir, log)?;
    let mut vite_guard = ChildGuard::new(&mut vite_child);

    wait_for_dev_server(DEV_HOST, DEV_PORT, Duration::from_secs(15))
        .context("Vite dev server didn't come up")?;
    log.detail("vite", &format!("ready at {}", DEV_URL));

    let status = spawn_and_wait_tauri_dev(&scaffold_dir, &target_dir, platform, &shutdown, log)?;

    // Kill Vite explicitly — without this it lingers as a background process
    // owning port 1420, which silently breaks the next dev session.
    vite_guard.kill();

    finalize(workdir, args.keep_scaffold, log);
    check_status(status)
}

/// Legacy mode: today's behavior (no Vite).
fn run_legacy(args: &DevArgs, index: PathBuf, platform: Platform) -> Result<()> {
    let log = &args.log;
    let raw = index.to_str().context("index argument is not valid UTF-8")?;
    let input = Input::parse(raw)?;

    let overrides = args.overrides(platform);
    let cwd = std::env::current_dir()?;
    let index_dir = match &input {
        Input::File { source_root, .. } => Some(source_root.as_path()),
        Input::Url(_) => None,
    };
    let mut cfg = config::resolve(&cwd, index_dir, &overrides)?;
    cfg.platforms = vec![platform];

    log.heading("tau dev");
    log.detail("source", &input.label());
    log_header(log, &cfg, platform);

    let workdir = make_workdir("tau-dev-")?;
    let project_dir = workdir.path().to_path_buf();

    match &input {
        Input::File { source_root, .. } => {
            scaffold::create_for_source(&project_dir, &cfg, source_root)?;
        }
        Input::Url(url) => {
            scaffold::create_for_url(&project_dir, &cfg, url)?;
        }
    }
    log.detail("scaffold", &project_dir.display().to_string());

    ensure_targets(platform, log)?;
    let target_dir = cache::dir()?;
    log.detail("cache", &target_dir.display().to_string());

    let shutdown = install_shutdown_flag();
    let status = spawn_and_wait_tauri_dev(&project_dir, &target_dir, platform, &shutdown, log)?;

    finalize(workdir, args.keep_scaffold, log);
    check_status(status)
}

fn log_header(log: &Logger, cfg: &config::Config, platform: Platform) {
    log.detail("app", &cfg.name);
    log.detail("identifier", &cfg.identifier);
    log.detail("platform", platform.as_str());
}

fn make_workdir(prefix: &str) -> Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(prefix)
        .tempdir()
        .context("failed to create temp working directory")
}

fn finalize(workdir: tempfile::TempDir, keep_scaffold: bool, log: &Logger) {
    if keep_scaffold {
        let kept = workdir.keep();
        log.done(&format!("Scaffold preserved at {}", kept.display()));
    }
}

fn check_status(status: Option<std::process::ExitStatus>) -> Result<()> {
    match status {
        Some(s) if !s.success() => bail!("cargo tauri dev exited with status {}", s),
        _ => Ok(()),
    }
}

/// Install a SIGINT handler that flips an atomic flag. Without it, the
/// default Rust behavior is to die on the first Ctrl+C, leaving Vite/Tauri
/// orphaned (still owning port 1420 and the webview). The flag lets the
/// poll loop observe Ctrl+C and orchestrate a coordinated kill.
/// `ctrlc::set_handler` returns Err if a handler is already installed —
/// that's fine, dev mode is one-per-process.
fn install_shutdown_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    let clone = flag.clone();
    let _ = ctrlc::set_handler(move || {
        clone.store(true, Ordering::SeqCst);
    });
    flag
}

/// Spawn `cargo tauri dev` (or its mobile equivalent) and poll the child
/// alongside the Ctrl+C flag. Returns the exit status, or `None` if we
/// killed the child ourselves on shutdown.
fn spawn_and_wait_tauri_dev(
    scaffold_dir: &Path,
    target_dir: &Path,
    platform: Platform,
    shutdown: &Arc<AtomicBool>,
    log: &Logger,
) -> Result<Option<std::process::ExitStatus>> {
    log.heading("Starting Tauri webview");
    let tauri = TauriCmd::new(scaffold_dir, target_dir, log);
    let mut child = match MobileFlavor::from_platform(platform) {
        None => tauri.spawn_dev_desktop()?,
        Some(flavor) => tauri.spawn_dev_mobile(flavor)?,
    };

    // Poll loop: exit when *either* Tauri exits on its own or the user hits
    // Ctrl+C. `try_wait()` returns Ok(Some(_)) once the process is reaped;
    // it never blocks. A 100ms tick keeps Ctrl+C responsive without burning CPU.
    loop {
        if shutdown.load(Ordering::SeqCst) {
            log.detail("shutdown", "Ctrl+C received, stopping…");
            let _ = child.kill();
            // Wait for Tauri to actually go away so cargo's build state isn't
            // left half-written. The cargo subprocess catches SIGTERM and tears
            // down its own children (the running webview app).
            let _ = child.wait();
            return Ok(None);
        }
        match child.try_wait() {
            Ok(Some(s)) => return Ok(Some(s)),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(anyhow::Error::new(e).context("failed to poll cargo tauri dev")),
        }
    }
}

/// Poll a TCP port until it accepts connections, or `timeout` elapses.
fn wait_for_dev_server(host: &str, port: u16, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let addr = format!("{}:{}", host, port);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(500)).is_ok() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    bail!("timed out waiting for dev server at {}", addr)
}

/// Make sure a child process is killed if we exit early. The actual `wait`
/// call still happens via the caller — this guard only fires if a `?` causes
/// the function to return without calling `kill()` explicitly.
struct ChildGuard<'a> {
    child: &'a mut Child,
    armed: bool,
}

impl<'a> ChildGuard<'a> {
    fn new(child: &'a mut Child) -> Self {
        Self { child, armed: true }
    }
    fn kill(&mut self) {
        if self.armed {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.armed = false;
        }
    }
}

impl Drop for ChildGuard<'_> {
    fn drop(&mut self) {
        self.kill();
    }
}

fn resolve_platform(arg: Option<&str>) -> Result<Platform> {
    match arg {
        Some(s) => Platform::from_str(s),
        None => Ok(Platform::host()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_platform_defaults_to_host() {
        assert_eq!(resolve_platform(None).unwrap(), Platform::host());
    }

    #[test]
    fn resolve_platform_parses_explicit() {
        assert_eq!(resolve_platform(Some("ios")).unwrap(), Platform::Ios);
        assert_eq!(resolve_platform(Some("android")).unwrap(), Platform::Android);
        assert_eq!(resolve_platform(Some("macos")).unwrap(), Platform::Macos);
    }

    #[test]
    fn resolve_platform_rejects_garbage() {
        assert!(resolve_platform(Some("ps5")).is_err());
    }

    fn empty_args() -> DevArgs {
        DevArgs {
            index: Some(PathBuf::from("index.html")),
            platform: None,
            name: None,
            identifier: None,
            config: None,
            keep_scaffold: false,
            log: Logger::new(crate::log::Level::Quiet),
        }
    }

    #[test]
    fn overrides_carries_one_platform() {
        let o = empty_args().overrides(Platform::Ios);
        assert_eq!(o.platforms, vec!["ios".to_string()]);
        assert!(!o.release);
        assert!(o.output.is_none());
    }

    #[test]
    fn overrides_passes_through_user_overrides() {
        let mut args = empty_args();
        args.name = Some("DevApp".to_string());
        args.identifier = Some("com.example.dev".to_string());
        args.config = Some(PathBuf::from("/tmp/tau.conf.json"));

        let o = args.overrides(Platform::Macos);
        assert_eq!(o.name.as_deref(), Some("DevApp"));
        assert_eq!(o.identifier.as_deref(), Some("com.example.dev"));
        assert_eq!(o.config.as_deref(), Some(std::path::Path::new("/tmp/tau.conf.json")));
    }
}
