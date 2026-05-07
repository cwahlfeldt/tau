//! `tau dev` — fast iteration via `cargo tauri dev`. Reuses config
//! resolution and scaffolding from the wrap pipeline; replaces the
//! build+extract tail with a long-lived interactive `cargo tauri dev`
//! session.
//!
//! Tauri serves files directly from `frontendDist` (the user's source
//! directory). Reload the webview to pick up changes — no watcher,
//! no livereload shim.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::str::FromStr;

use crate::build::{ensure_targets, MobileFlavor, TauriCmd};
use crate::cache;
use crate::cli::Cli;
use crate::config::{self, Platform};
use crate::input::Input;
use crate::log::{Level, Logger};
use crate::scaffold;

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

    match &input {
        Input::File { source_root, .. } => {
            scaffold::create_for_source(&project_dir, &cfg, source_root)?;
        }
        Input::Url(url) => {
            scaffold::create_for_url(&project_dir, &cfg, url)?;
        }
    }
    log.detail("scaffold", &project_dir.display().to_string());

    ensure_targets(platform, &log)?;

    let target_dir = cache::dir()?;
    log.detail("cache", &target_dir.display().to_string());

    let tauri = TauriCmd::new(&project_dir, &target_dir, &log);
    let mut child = match MobileFlavor::from_platform(platform) {
        None => tauri.spawn_dev_desktop()?,
        Some(flavor) => tauri.spawn_dev_mobile(flavor)?,
    };

    let status = child.wait().context("failed to wait on cargo tauri dev")?;

    if args.keep_scaffold {
        let kept = workdir.keep();
        log.done(&format!("Scaffold preserved at {}", kept.display()));
    }

    if !status.success() {
        anyhow::bail!("cargo tauri dev exited with status {}", status);
    }
    Ok(())
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
}
