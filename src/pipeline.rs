//! End-to-end orchestration of a single wrap.
//!
//! The pipeline is intentionally linear: resolve config, scaffold a Tauri
//! project in a tempdir whose `frontendDist` points at the user's source
//! directory (no asset copying or HTML rewriting), then build & extract
//! artifacts for each requested platform.

use anyhow::{Context, Result};
use std::path::PathBuf;

use crate::cli::BuildFlags;
use crate::config::Overrides;
use crate::filter;
use crate::input::Input;
use crate::log::Logger;
use crate::{build, config, scaffold};

/// Arguments for the `tau build <index>` subcommand and the top-level
/// `tau <index>` wrap form. `main.rs` normalizes both into this shape.
pub struct BuildArgs {
    pub index: PathBuf,
    pub build: BuildFlags,
    pub platform: Vec<String>,
    pub dry_run: bool,
    pub log: Logger,
}

pub fn run(args: BuildArgs) -> Result<()> {
    let overrides = Overrides {
        name: args.build.name.clone(),
        identifier: args.build.identifier.clone(),
        output: args.build.output.clone(),
        config: args.build.config.clone(),
        platforms: args.platform.clone(),
        release: args.build.release,
    };
    let (input, cfg) = resolve_inputs(&args.index, &overrides)?;
    log_header(&args.log, &input, &cfg);

    let workdir = tempfile::Builder::new()
        .prefix("tau-")
        .tempdir()
        .context("failed to create temp working directory")?;
    let project_dir = workdir.path().to_path_buf();

    // For local-file inputs we materialize a filtered copy of the source
    // tree and point `frontendDist` at *that*, instead of the source dir
    // itself. Without this, anything sitting next to `index.html` (`.git`,
    // `node_modules`, prior build output, etc.) ends up embedded in the
    // bundle, since Tauri's bundler walks the whole frontendDist directory.
    // The handle is held until after the build so the tempdir survives.
    let _frontend = match &input {
        Input::File { source_root, .. } => {
            let materialized = filter::materialize(source_root, &cfg.exclude)
                .context("filter source tree")?;
            args.log.detail(
                "frontend",
                &format!(
                    "{} ({} files, {} excluded)",
                    materialized.path().display(),
                    materialized.copied,
                    materialized.excluded
                ),
            );
            scaffold::create_for_source(&project_dir, &cfg, materialized.path())?;
            Some(materialized)
        }
        Input::Url(url) => {
            scaffold::create_for_url(&project_dir, &cfg, url)?;
            None
        }
    };
    args.log.detail("scaffold", &project_dir.display().to_string());

    if args.dry_run {
        let kept = workdir.keep();
        args.log
            .done(&format!("Dry run: scaffold preserved at {}", kept.display()));
        return Ok(());
    }

    let output_dir = PathBuf::from(&cfg.output);
    std::fs::create_dir_all(&output_dir)?;

    for platform in &cfg.platforms {
        args.log.heading(&format!("Building {}", platform.as_str()));
        build::ensure_targets(*platform, &args.log)?;
        let artifacts = build::build_platform(&project_dir, *platform, &cfg, &args.log)?;
        for path in build::extract_artifacts(&artifacts, &output_dir, &cfg, *platform)? {
            args.log.artifact(&path);
        }
    }

    if args.build.keep_scaffold {
        let kept = workdir.keep();
        args.log
            .done(&format!("Scaffold preserved at {}", kept.display()));
    }

    args.log
        .done(&format!("Done. Artifacts in {}", output_dir.display()));
    Ok(())
}

/// Parse the raw positional argument and resolve the layered config.
/// Shared by the wrap/build pipeline above and `tau dev`.
pub fn resolve_inputs(raw: &std::path::Path, overrides: &Overrides) -> Result<(Input, config::Config)> {
    let raw_str = raw
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("index argument is not valid UTF-8"))?;
    let input = Input::parse(raw_str)?;

    let cwd = std::env::current_dir()?;
    let index_dir = match &input {
        Input::File { source_root, .. } => Some(source_root.as_path()),
        Input::Url(_) => None,
    };
    let cfg = config::resolve(&cwd, index_dir, overrides)?;
    Ok((input, cfg))
}

fn log_header(log: &Logger, input: &Input, cfg: &config::Config) {
    log.heading("tau");
    log.detail("app", &cfg.name);
    log.detail("identifier", &cfg.identifier);
    log.detail("source", &input.label());
    log.detail("profile", cfg.profile.dir_name());
    let platforms = cfg
        .platforms
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    log.detail("platforms", &platforms);
}
