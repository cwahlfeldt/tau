//! `tau build` — production build for a tau project.
//!
//! Flow:
//!   1. Discover the project (cwd + ancestors).
//!   2. Run `vite build` inside `.tau/` → `.tau/dist/` (tree-shaken, hashed).
//!   3. Generate a fresh Tauri scaffold whose `frontendDist` points at
//!      `.tau/dist/` (absolute path).
//!   4. Drive `cargo tauri build` per requested platform — same code path
//!      the legacy wrap flow uses.

use anyhow::{anyhow, bail, Context, Result};
use std::path::PathBuf;

use crate::build;
use crate::cli::Cli;
use crate::config;
use crate::input;
use crate::log::{Level, Logger};
use crate::scaffold;
use crate::tooling;

pub struct BuildArgs {
    pub release: bool,
    pub platform: Vec<String>,
    pub name: Option<String>,
    pub identifier: Option<String>,
    pub output: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub keep_scaffold: bool,
    pub quiet: bool,
    pub verbose: bool,
}

pub fn run(args: BuildArgs) -> Result<()> {
    let level = if args.quiet { Level::Quiet } else if args.verbose { Level::Verbose } else { Level::Normal };
    let log = Logger::new(level);

    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let project = input::discover_project(&cwd).ok_or_else(|| {
        anyhow!(
            "no tau project found in `{}` or any parent.\n\
             Run `tau create <name>` to scaffold one, or use `tau <path/to/index.html>` to wrap an existing static site.",
            cwd.display()
        )
    })?;

    log.heading("tau build");
    log.detail("project", &project.root.display().to_string());

    tooling::ensure_node_present()?;
    let pm = tooling::detect_package_manager()?;
    log.detail("package manager", pm.label());

    // If node_modules doesn't exist yet (e.g. user cloned the project from
    // git), install before building. We don't try to be clever about lockfile
    // freshness — the package manager handles that.
    if !project.tau_dir.join("node_modules").is_dir() {
        log.detail("install", "node_modules missing — installing first");
        tooling::install(pm, &project.tau_dir, &log)?;
    }

    log.heading("Bundling frontend");
    tooling::vite_build(pm, &project.tau_dir, &log)?;
    if !project.dist_dir.is_dir() {
        bail!(
            "vite build completed but {} was not created",
            project.dist_dir.display()
        );
    }
    log.detail("dist", &project.dist_dir.display().to_string());

    // Synthesize a Cli so we can reuse the same three-tier config resolver.
    // index_dir is the project root — that's where any tau.conf.json lives.
    let synthetic = synthesize_cli(&args);
    let cfg = {
        let mut c = config::resolve(&project.root, Some(&project.root), &synthetic)?;
        // If neither --name nor tau.conf.json supplied a name, fall back to
        // the project directory name. The legacy wrap flow doesn't have this
        // luxury (it doesn't know the project), so config::resolve still
        // defaults to "WrappedApp" — only override here.
        if synthetic.name.is_none() && c.name == config::DEFAULT_NAME {
            if let Some(stem) = project.root.file_name().and_then(|s| s.to_str()) {
                c.name = stem.to_string();
                if synthetic.identifier.is_none() {
                    c.identifier = config::default_identifier(&c.name);
                }
            }
        }
        c
    };

    log.detail("app", &cfg.name);
    log.detail("identifier", &cfg.identifier);
    log.detail("profile", cfg.profile.dir_name());
    let plats = cfg.platforms.iter().map(|p| p.as_str()).collect::<Vec<_>>().join(", ");
    log.detail("platforms", &plats);

    let workdir = tempfile::Builder::new()
        .prefix("tau-build-")
        .tempdir()
        .context("failed to create temp working directory")?;
    let scaffold_dir = workdir.path().to_path_buf();
    scaffold::create_for_source(&scaffold_dir, &cfg, &project.dist_dir)?;
    log.detail("scaffold", &scaffold_dir.display().to_string());

    let output_dir = PathBuf::from(&cfg.output);
    let output_dir = if output_dir.is_absolute() {
        output_dir
    } else {
        project.root.join(output_dir)
    };
    std::fs::create_dir_all(&output_dir)?;

    for platform in &cfg.platforms {
        log.heading(&format!("Building {}", platform.as_str()));
        build::ensure_targets(*platform, &log)?;
        let artifacts = build::build_platform(&scaffold_dir, *platform, &cfg, &log)?;
        for path in build::extract_artifacts(&artifacts, &output_dir, &cfg, *platform)? {
            log.artifact(&path);
        }
    }

    if args.keep_scaffold {
        let kept = workdir.keep();
        log.done(&format!("Scaffold preserved at {}", kept.display()));
    }
    log.done(&format!("Done. Artifacts in {}", output_dir.display()));
    Ok(())
}

fn synthesize_cli(args: &BuildArgs) -> Cli {
    Cli {
        index: None,
        release: args.release,
        platform: args.platform.clone(),
        name: args.name.clone(),
        identifier: args.identifier.clone(),
        output: args.output.clone(),
        config: args.config.clone(),
        dry_run: false,
        keep_scaffold: false,
        quiet: args.quiet,
        verbose: args.verbose,
        command: None,
    }
}

/// `tau add <pkg>` — wraps the package manager's `add` command. Lives here
/// because the only thing it shares with build is "discover project, run a
/// command in .tau/."
pub fn run_add(package: String, quiet: bool, verbose: bool) -> Result<()> {
    let level = if quiet { Level::Quiet } else if verbose { Level::Verbose } else { Level::Normal };
    let log = Logger::new(level);
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    let project = input::discover_project(&cwd).ok_or_else(|| {
        anyhow!(
            "no tau project found in `{}` or any parent. `tau add` only works inside a project created by `tau create`.",
            cwd.display()
        )
    })?;
    tooling::ensure_node_present()?;
    let pm = tooling::detect_package_manager()?;
    log.heading(&format!("Adding {}", package));
    log.detail("project", &project.root.display().to_string());
    tooling::add(pm, &project.tau_dir, &package, &log)?;
    log.done(&format!("Installed {} in {}", package, project.tau_dir.display()));
    Ok(())
}
