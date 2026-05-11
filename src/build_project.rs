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
use crate::cli::BuildFlags;
use crate::config::{self, Overrides};
use crate::input;
use crate::log::Logger;
use crate::scaffold;
use crate::tooling;

pub struct BuildArgs {
    pub build: BuildFlags,
    pub platform: Vec<String>,
    pub log: Logger,
}

impl BuildArgs {
    fn overrides(&self) -> Overrides {
        Overrides {
            name: self.build.name.clone(),
            identifier: self.build.identifier.clone(),
            output: self.build.output.clone(),
            config: self.build.config.clone(),
            platforms: self.platform.clone(),
            release: self.build.release,
        }
    }
}

pub fn run(args: BuildArgs) -> Result<()> {
    let log = &args.log;

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
        tooling::install(pm, &project.tau_dir, log)?;
    }

    log.heading("Bundling frontend");
    tooling::vite_build(pm, &project.tau_dir, log)?;
    if !project.dist_dir.is_dir() {
        bail!(
            "vite build completed but {} was not created",
            project.dist_dir.display()
        );
    }
    log.detail("dist", &project.dist_dir.display().to_string());

    // index_dir is the project root — that's where any tau.conf.json lives.
    let overrides = args.overrides();
    let mut cfg = config::resolve(&project.root, Some(&project.root), &overrides)?;
    config::apply_project_name_fallback(&mut cfg, &project.root, &overrides);

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
        build::ensure_targets(*platform, log)?;
        let artifacts = build::build_platform(&scaffold_dir, *platform, &cfg, log)?;
        for path in build::extract_artifacts(&artifacts, &output_dir, &cfg, *platform)? {
            log.artifact(&path);
        }
    }

    if args.build.keep_scaffold {
        let kept = workdir.keep();
        log.done(&format!("Scaffold preserved at {}", kept.display()));
    }
    log.done(&format!("Done. Artifacts in {}", output_dir.display()));
    Ok(())
}
