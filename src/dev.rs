//! `tau dev` — fast iteration via `cargo tauri dev`. Reuses config
//! resolution, asset discovery, and scaffolding from the wrap pipeline;
//! replaces the build+extract tail with a long-lived interactive
//! `cargo tauri dev` session.
//!
//! Hot reload: a filesystem watcher rooted at the user's source dir
//! refreshes `dist/` (assets + rewritten index.html) on change and bumps
//! a marker file. A small IIFE injected into the page polls the marker
//! and triggers `location.reload()` when it changes.

use anyhow::{Context, Result};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::build::{ensure_targets, MobileFlavor, TauriCmd};
use crate::cache;
use crate::cli::Cli;
use crate::config::{self, Config, Platform};
use crate::discover;
use crate::input::Input;
use crate::log::{Level, Logger};
use crate::scaffold;

/// Debounce window for filesystem events. Editors tend to emit a flurry
/// of writes for a single save (e.g. atomic-rename, .swp churn); we
/// coalesce anything inside this window into a single refresh.
const DEBOUNCE: Duration = Duration::from_millis(150);

pub struct DevArgs {
    pub index: PathBuf,
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

    let raw = args
        .index
        .to_str()
        .context("index argument is not valid UTF-8")?;
    let input = Input::parse(raw)?;

    let synthetic = synthesize_cli(&args, platform);
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

    // Optional watcher handle — only set for File inputs. URL inputs have
    // nothing local to watch.
    let mut watcher_state: Option<(Arc<Mutex<bool>>, thread::JoinHandle<()>)> = None;

    match &input {
        Input::File { index_path, source_root } => {
            let discovered =
                discover::discover_with_mode(index_path, source_root, &cfg, true)?;
            log.detail("assets", &format!("{} files", discovered.assets.len()));
            scaffold::create_with_mode(&project_dir, &cfg, &discovered, true)?;
            log.detail("scaffold", &project_dir.display().to_string());

            let token_path = scaffold::reload_token_path(&project_dir);
            write_token(&token_path, 0)?;

            let stop = Arc::new(Mutex::new(false));
            let handle = spawn_watcher(
                source_root.clone(),
                index_path.clone(),
                project_dir.clone(),
                cfg.clone(),
                token_path,
                stop.clone(),
                log.clone(),
            )?;
            watcher_state = Some((stop, handle));
        }
        Input::Url(url) => {
            scaffold::create_for_url(&project_dir, &cfg, url)?;
            log.detail("scaffold", &project_dir.display().to_string());
        }
    }

    ensure_targets(platform, &log)?;

    let target_dir = cache::dir()?;
    log.detail("cache", &target_dir.display().to_string());

    let tauri = TauriCmd::new(&project_dir, &target_dir, &log);
    let mut child = match MobileFlavor::from_platform(platform) {
        None => tauri.spawn_dev_desktop()?,
        Some(flavor) => tauri.spawn_dev_mobile(flavor)?,
    };

    // Block on the dev process. When the user Ctrl-Cs, the child exits
    // and we tear down the watcher (if any).
    let status = child.wait().context("failed to wait on cargo tauri dev")?;

    if let Some((stop, handle)) = watcher_state {
        *stop.lock().unwrap() = true;
        let _ = handle.join();
    }

    if args.keep_scaffold {
        let kept = workdir.keep();
        log.done(&format!("Scaffold preserved at {}", kept.display()));
    }

    if !status.success() {
        anyhow::bail!("cargo tauri dev exited with status {}", status);
    }
    Ok(())
}

fn spawn_watcher(
    source_root: PathBuf,
    index_path: PathBuf,
    project_dir: PathBuf,
    cfg: Config,
    token_path: PathBuf,
    stop: Arc<Mutex<bool>>,
    log: Logger,
) -> Result<thread::JoinHandle<()>> {
    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
    let mut watcher: RecommendedWatcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("failed to create filesystem watcher")?;
    watcher
        .watch(&source_root, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", source_root.display()))?;

    let handle = thread::spawn(move || {
        // Move the watcher into the thread so it lives as long as we do.
        let _watcher = watcher;
        let mut counter: u64 = 1;
        let mut pending: Option<Instant> = None;

        loop {
            if *stop.lock().unwrap() {
                return;
            }

            // If a refresh is pending, sleep just long enough to debounce.
            let timeout = match pending {
                Some(at) => {
                    let now = Instant::now();
                    if now >= at {
                        Duration::from_millis(0)
                    } else {
                        at - now
                    }
                }
                None => Duration::from_millis(250),
            };

            match rx.recv_timeout(timeout) {
                Ok(Ok(event)) => {
                    if !is_relevant(&event, &project_dir) {
                        continue;
                    }
                    pending = Some(Instant::now() + DEBOUNCE);
                }
                Ok(Err(e)) => {
                    log.detail("watcher", &format!("error: {}", e));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(deadline) = pending {
                        if Instant::now() >= deadline {
                            pending = None;
                            if let Err(e) = refresh(&index_path, &source_root, &cfg, &project_dir) {
                                log.detail("reload", &format!("failed: {}", e));
                                continue;
                            }
                            counter += 1;
                            if let Err(e) = write_token(&token_path, counter) {
                                log.detail("reload", &format!("token write failed: {}", e));
                                continue;
                            }
                            log.detail("reload", &format!("#{}", counter));
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });

    Ok(handle)
}

/// Re-run discover + scaffold-frontend to push the user's latest source
/// edits into the live `dist/`. We deliberately do not touch `src-tauri/`
/// — only the frontend changes during dev.
fn refresh(index_path: &Path, source_root: &Path, cfg: &Config, project_dir: &Path) -> Result<()> {
    let discovered = discover::discover_with_mode(index_path, source_root, cfg, true)?;
    scaffold::refresh_frontend(project_dir, &discovered, true)
}

/// Filter out events that came from inside our own scaffold tempdir
/// (writes we just performed) or from common editor noise. Without this
/// the reload loop can re-trigger itself if the source root happens to
/// contain the scaffold (it shouldn't, but defense-in-depth).
fn is_relevant(event: &Event, project_dir: &Path) -> bool {
    for path in &event.paths {
        if path.starts_with(project_dir) {
            return false;
        }
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            if name.ends_with('~') || name.ends_with(".swp") || name.ends_with(".tmp") {
                return false;
            }
        }
    }
    !event.paths.is_empty()
}

fn write_token(path: &Path, counter: u64) -> Result<()> {
    std::fs::write(path, counter.to_string())
        .with_context(|| format!("write reload token: {}", path.display()))
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
            index: PathBuf::from("index.html"),
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

    #[test]
    fn is_relevant_skips_paths_inside_project_dir() {
        let project = PathBuf::from("/tmp/tau-dev-xyz");
        let event = Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![project.join("dist/foo.js")],
            attrs: Default::default(),
        };
        assert!(!is_relevant(&event, &project));
    }

    #[test]
    fn is_relevant_skips_editor_swap_files() {
        let project = PathBuf::from("/tmp/tau-dev-xyz");
        let event = Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("/src/index.html.swp")],
            attrs: Default::default(),
        };
        assert!(!is_relevant(&event, &project));
    }

    #[test]
    fn is_relevant_passes_normal_source_changes() {
        let project = PathBuf::from("/tmp/tau-dev-xyz");
        let event = Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("/src/app.js")],
            attrs: Default::default(),
        };
        assert!(is_relevant(&event, &project));
    }
}
