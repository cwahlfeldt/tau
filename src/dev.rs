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
//!
//! Tauri serves files directly from `frontendDist` in legacy mode. Reload
//! the webview to pick up changes — no watcher, no livereload shim.

use anyhow::{anyhow, bail, Context, Result};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::Child;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::build::{ensure_targets, MobileFlavor, TauriCmd};
use crate::cache;
use crate::cli::Cli;
use crate::config::{self, Platform};
use crate::input::{self, Input, ProjectRoot};
use crate::log::{Level, Logger};
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
    pub quiet: bool,
    pub verbose: bool,
}

pub fn run(args: DevArgs) -> Result<()> {
    let level = if args.quiet {
        Level::Quiet
    } else if args.verbose {
        Level::Verbose
    } else {
        Level::Normal
    };
    let log = Logger::new(level);

    let platform = resolve_platform(args.platform.as_deref())?;

    // Decide which mode to run in. Explicit positional argument always wins —
    // if the user passes `tau dev path/to/index.html`, never try to discover.
    if let Some(index_path) = &args.index {
        return run_legacy(&args, index_path.clone(), platform, &log);
    }
    let cwd = std::env::current_dir()?;
    if let Some(project) = input::discover_project(&cwd) {
        return run_project(&args, project, platform, &log);
    }
    Err(anyhow!(
        "no tau project found in `{}` or any parent, and no index path was provided.\n\
         Run `tau create <name>` to scaffold one, or pass `tau dev <path/to/index.html>` to wrap an existing file.",
        cwd.display()
    ))
}

/// Project mode: Vite + Tauri devUrl.
fn run_project(args: &DevArgs, project: ProjectRoot, platform: Platform, log: &Logger) -> Result<()> {
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
    let synthetic = synthesize_cli(args, platform);
    let mut cfg = config::resolve(&project.root, Some(&project.root), &synthetic)?;
    if synthetic.name.is_none() && cfg.name == config::DEFAULT_NAME {
        if let Some(stem) = project.root.file_name().and_then(|s| s.to_str()) {
            cfg.name = stem.to_string();
            if synthetic.identifier.is_none() {
                cfg.identifier = config::default_identifier(&cfg.name);
            }
        }
    }
    cfg.platforms = vec![platform];

    log.detail("app", &cfg.name);
    log.detail("identifier", &cfg.identifier);
    log.detail("platform", platform.as_str());

    let workdir = tempfile::Builder::new()
        .prefix("tau-dev-")
        .tempdir()
        .context("failed to create temp working directory")?;
    let scaffold_dir = workdir.path().to_path_buf();
    scaffold::create_for_dev_server(&scaffold_dir, &cfg, DEV_URL)?;
    log.detail("scaffold", &scaffold_dir.display().to_string());

    ensure_targets(platform, log)?;
    let target_dir = cache::dir()?;

    // Install a SIGINT handler before spawning children. Without this, Rust's
    // default behavior is to die on the first Ctrl+C, leaving Vite/Tauri
    // orphaned (still owning port 1420 and the webview). With the handler,
    // we observe Ctrl+C as a flag flip and orchestrate a coordinated kill.
    // ctrlc::set_handler returns Err if a handler is already installed —
    // that's fine, dev mode is one-per-process.
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let flag = shutdown.clone();
        let _ = ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
        });
    }

    // Spawn Vite first; Tauri only points the webview at it once it's up.
    log.heading("Starting Vite dev server");
    let mut vite_child = tooling::vite_dev(pm, &project.tau_dir, log)?;
    let mut vite_guard = ChildGuard::new(&mut vite_child);

    if let Err(e) = wait_for_dev_server(DEV_HOST, DEV_PORT, Duration::from_secs(15)) {
        return Err(e.context("Vite dev server didn't come up"));
    }
    log.detail("vite", &format!("ready at {}", DEV_URL));

    log.heading("Starting Tauri webview");
    let tauri = TauriCmd::new(&scaffold_dir, &target_dir, log);
    let mut tauri_child = match MobileFlavor::from_platform(platform) {
        None => tauri.spawn_dev_desktop()?,
        Some(flavor) => tauri.spawn_dev_mobile(flavor)?,
    };

    // Poll loop: exit when *either* Tauri exits on its own (closed window,
    // crash, etc.) or the user hits Ctrl+C. `try_wait()` returns Ok(Some(_))
    // once the process is reaped; it never blocks. A 100ms tick keeps Ctrl+C
    // responsive without burning CPU.
    let status = loop {
        if shutdown.load(Ordering::SeqCst) {
            log.detail("shutdown", "Ctrl+C received, stopping…");
            let _ = tauri_child.kill();
            // Wait for Tauri to actually go away so the cargo build state
            // isn't left half-written. The cargo subprocess catches SIGTERM
            // and tears down its own children (the running webview app).
            let _ = tauri_child.wait();
            break None;
        }
        match tauri_child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(anyhow::Error::new(e).context("failed to poll cargo tauri dev")),
        }
    };

    // Tauri is gone. Kill Vite explicitly — without this it lingers as a
    // background process owning port 1420, which silently breaks the next
    // dev session.
    vite_guard.kill();

    if args.keep_scaffold {
        let kept = workdir.keep();
        log.done(&format!("Scaffold preserved at {}", kept.display()));
    }

    match status {
        Some(s) if !s.success() => bail!("cargo tauri dev exited with status {}", s),
        _ => Ok(()),
    }
}

/// Legacy mode: today's behavior (no Vite).
fn run_legacy(args: &DevArgs, index: PathBuf, platform: Platform, log: &Logger) -> Result<()> {
    let raw = index.to_str().context("index argument is not valid UTF-8")?;
    let input = Input::parse(raw)?;

    let synthetic = synthesize_cli(args, platform);
    let cwd = std::env::current_dir()?;
    let index_dir = match &input {
        Input::File { source_root, .. } => Some(source_root.as_path()),
        Input::Url(_) => None,
    };
    let mut cfg = config::resolve(&cwd, index_dir, &synthetic)?;
    cfg.platforms = vec![platform];

    log.heading("tau dev");
    log.detail("app", &cfg.name);
    log.detail("identifier", &cfg.identifier);
    log.detail("source", &input.label());
    log.detail("platform", platform.as_str());

    let workdir = tempfile::Builder::new()
        .prefix("tau-dev-")
        .tempdir()
        .context("failed to create temp working directory")?;
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

    // Same Ctrl+C orchestration as project mode (one child here, but the
    // graceful-shutdown problem is the same).
    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let flag = shutdown.clone();
        let _ = ctrlc::set_handler(move || {
            flag.store(true, Ordering::SeqCst);
        });
    }

    let tauri = TauriCmd::new(&project_dir, &target_dir, log);
    let mut child = match MobileFlavor::from_platform(platform) {
        None => tauri.spawn_dev_desktop()?,
        Some(flavor) => tauri.spawn_dev_mobile(flavor)?,
    };

    let status = loop {
        if shutdown.load(Ordering::SeqCst) {
            log.detail("shutdown", "Ctrl+C received, stopping…");
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(anyhow::Error::new(e).context("failed to poll cargo tauri dev")),
        }
    };

    if args.keep_scaffold {
        let kept = workdir.keep();
        log.done(&format!("Scaffold preserved at {}", kept.display()));
    }

    match status {
        Some(s) if !s.success() => bail!("cargo tauri dev exited with status {}", s),
        _ => Ok(()),
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

fn synthesize_cli(args: &DevArgs, platform: Platform) -> Cli {
    Cli {
        index: None,
        release: false,
        platform: vec![platform.as_str().to_string()],
        name: args.name.clone(),
        identifier: args.identifier.clone(),
        output: None,
        config: args.config.clone(),
        dry_run: false,
        keep_scaffold: false,
        quiet: args.quiet,
        verbose: args.verbose,
        command: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_args() -> DevArgs {
        DevArgs {
            index: Some(PathBuf::from("index.html")),
            platform: None,
            name: None,
            identifier: None,
            config: None,
            keep_scaffold: false,
            quiet: false,
            verbose: false,
        }
    }

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

    #[test]
    fn synthetic_cli_clears_release_output_dryrun() {
        let cli = synthesize_cli(&empty_args(), Platform::Linux);
        assert!(!cli.release);
        assert!(cli.output.is_none());
        assert!(!cli.dry_run);
        assert!(!cli.keep_scaffold);
        assert!(cli.command.is_none());
        assert!(cli.index.is_none());
    }

    #[test]
    fn synthetic_cli_carries_one_platform() {
        let cli = synthesize_cli(&empty_args(), Platform::Ios);
        assert_eq!(cli.platform, vec!["ios".to_string()]);
    }

    #[test]
    fn synthetic_cli_passes_through_overrides() {
        let mut args = empty_args();
        args.name = Some("DevApp".to_string());
        args.identifier = Some("com.example.dev".to_string());
        args.config = Some(PathBuf::from("/tmp/tau.conf.json"));
        args.quiet = true;

        let cli = synthesize_cli(&args, Platform::Macos);
        assert_eq!(cli.name.as_deref(), Some("DevApp"));
        assert_eq!(cli.identifier.as_deref(), Some("com.example.dev"));
        assert_eq!(cli.config.as_deref(), Some(std::path::Path::new("/tmp/tau.conf.json")));
        assert!(cli.quiet);
        assert!(!cli.verbose);
    }
}
