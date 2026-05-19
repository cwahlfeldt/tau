//! `tau dev` — fast iteration via `cargo tauri dev`.
//!
//! Scaffolds a temp Tauri project pointing `frontendDist` at the user's
//! source tree (or a stub for URL inputs) and spawns the dev process.
//! Ctrl+C kills the whole process tree so nothing is left orphaned.

use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::process::Child;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::build::{ensure_targets, MobileFlavor, TauriCmd};
use crate::cache;
use crate::config::{Overrides, Platform};
use crate::input::Input;
use crate::log::Logger;
use crate::pipeline;
use crate::scaffold;

pub struct DevArgs {
    pub index: PathBuf,
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
    let log = &args.log;

    let overrides = args.overrides(platform);
    let (input, mut cfg) = pipeline::resolve_inputs(&args.index, &overrides)?;
    // `resolve()` already produces this single-platform list from the
    // overrides above, but pin it explicitly so any future change to
    // `resolve_platforms` can't silently widen dev to multiple targets.
    cfg.platforms = vec![platform];

    log.heading("tau dev");
    log.detail("source", &input.label());
    log.detail("app", &cfg.name);
    log.detail("identifier", &cfg.identifier);
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

    let shutdown = install_shutdown_flag();
    let status = run_tauri_dev(&project_dir, &target_dir, platform, &shutdown, log)?;

    if args.keep_scaffold {
        let kept = workdir.keep();
        log.done(&format!("Scaffold preserved at {}", kept.display()));
    }
    match status {
        Some(s) if !s.success() => bail!("cargo tauri dev exited with status {}", s),
        _ => Ok(()),
    }
}

/// Install a SIGINT handler that flips an atomic flag. Without it, the
/// default Rust behavior is to die on the first Ctrl+C, leaving the Tauri
/// dev process orphaned. The flag lets the poll loop observe Ctrl+C and
/// orchestrate a coordinated kill of the whole process tree.
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
fn run_tauri_dev(
    scaffold_dir: &std::path::Path,
    target_dir: &std::path::Path,
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
            terminate_tree(&mut child);
            return Ok(None);
        }
        match child.try_wait() {
            Ok(Some(s)) => return Ok(Some(s)),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => return Err(anyhow::Error::new(e).context("failed to poll cargo tauri dev")),
        }
    }
}

/// Send SIGTERM to the entire process group, give it a grace period, then
/// SIGKILL anything still alive. The child must have been spawned with
/// `process_group(0)` — otherwise this only signals the direct child and
/// orphaned grandchildren live on.
///
/// On Windows there's no process-group equivalent here yet, so we fall back
/// to `child.kill()` (which has the same orphan problem; tracked separately).
fn terminate_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // SAFETY: killpg with a valid pgid is safe; the kernel returns an
        // error rather than misbehaving if the group is already gone.
        unsafe {
            libc::killpg(pid, libc::SIGTERM);
        }
        // Grace period — cargo cleans up cooperatively on SIGTERM
        // (closes ports, flushes build state). 1.5s is enough for the
        // signal to propagate to children.
        for _ in 0..15 {
            if let Ok(Some(_)) = child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // Anything still alive gets the hard signal.
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
        let _ = child.wait();
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
        let _ = child.wait();
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
            index: PathBuf::from("index.html"),
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
