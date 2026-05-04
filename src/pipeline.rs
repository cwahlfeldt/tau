//! End-to-end orchestration of a single wrap.
//!
//! The pipeline is intentionally linear: resolve config, discover assets,
//! scaffold a Tauri project in a tempdir, then build & extract artifacts for
//! each requested platform. Each stage is implemented in its own module —
//! `pipeline::run` is the only place that knows the order they fire in.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

use crate::cli::Cli;
use crate::log::Logger;
use crate::{build, config, discover, scaffold};

pub fn run(args: Cli) -> Result<()> {
    let log = Logger::new(args.log_level());
    let inputs = Inputs::resolve(&args)?;

    log_header(&log, &inputs);

    let discovered = discover::discover(&inputs.index_path, &inputs.source_root, &inputs.cfg)?;
    log.detail("assets", &format!("{} files", discovered.assets.len()));

    let workdir = tempfile::Builder::new()
        .prefix("tau-")
        .tempdir()
        .context("failed to create temp working directory")?;
    let project_dir = workdir.path().to_path_buf();

    scaffold::create(&project_dir, &inputs.cfg, &discovered)?;
    log.detail("scaffold", &project_dir.display().to_string());

    if args.dry_run {
        let kept = workdir.keep();
        log.done(&format!("Dry run: scaffold preserved at {}", kept.display()));
        return Ok(());
    }

    let output_dir = PathBuf::from(&inputs.cfg.output);
    std::fs::create_dir_all(&output_dir)?;

    for platform in &inputs.cfg.platforms {
        log.heading(&format!("Building {}", platform.as_str()));
        build::ensure_targets(*platform, &log)?;
        let artifacts = build::build_platform(&project_dir, *platform, &inputs.cfg, &log)?;
        for path in build::extract_artifacts(&artifacts, &output_dir, &inputs.cfg, *platform)? {
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

/// Validated inputs for the pipeline: an existing index.html, the directory
/// it lives in (the asset root), and the resolved `Config`.
struct Inputs {
    index_path: PathBuf,
    source_root: PathBuf,
    cfg: config::Config,
}

impl Inputs {
    fn resolve(args: &Cli) -> Result<Self> {
        // `subcommand_negates_reqs` makes `index` optional at the clap layer;
        // when we reach this branch it must be present.
        let index_arg = args
            .index
            .as_ref()
            .ok_or_else(|| anyhow!("index.html path is required"))?;

        let index_path = std::fs::canonicalize(index_arg)
            .with_context(|| format!("index.html not found: {}", index_arg.display()))?;
        let source_root = index_path
            .parent()
            .context("could not determine source root from index.html path")?
            .to_path_buf();

        let cwd = std::env::current_dir()?;
        let cfg = config::resolve(&cwd, args)?;

        Ok(Self { index_path, source_root, cfg })
    }
}

fn log_header(log: &Logger, inputs: &Inputs) {
    let cfg = &inputs.cfg;
    log.heading("tau");
    log.detail("app", &cfg.name);
    log.detail("identifier", &cfg.identifier);
    log.detail("source", &inputs.source_root.display().to_string());
    log.detail("profile", cfg.profile.dir_name());
    let platforms = cfg
        .platforms
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    log.detail("platforms", &platforms);
}
