//! End-to-end orchestration of a single wrap.
//!
//! The pipeline is intentionally linear: resolve config, discover assets
//! (or skip for URL inputs), scaffold a Tauri project in a tempdir, then
//! build & extract artifacts for each requested platform. Each stage is
//! implemented in its own module — `pipeline::run` is the only place
//! that knows the order they fire in.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

use crate::cli::Cli;
use crate::input::Input;
use crate::log::Logger;
use crate::{build, config, discover, scaffold};

pub fn run(args: Cli) -> Result<()> {
    let log = Logger::new(args.log_level());
    let inputs = Inputs::resolve(&args)?;

    log_header(&log, &inputs);

    let workdir = tempfile::Builder::new()
        .prefix("tau-")
        .tempdir()
        .context("failed to create temp working directory")?;
    let project_dir = workdir.path().to_path_buf();

    match &inputs.input {
        Input::File { index_path, source_root } => {
            let discovered = discover::discover(index_path, source_root, &inputs.cfg)?;
            log.detail("assets", &format!("{} files", discovered.assets.len()));
            scaffold::create(&project_dir, &inputs.cfg, &discovered)?;
        }
        Input::Url(url) => {
            scaffold::create_for_url(&project_dir, &inputs.cfg, url)?;
        }
    }
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

/// Validated inputs for the pipeline: a classified `Input` (file path
/// with source root, or remote URL) and the resolved `Config`.
struct Inputs {
    input: Input,
    cfg: config::Config,
}

impl Inputs {
    fn resolve(args: &Cli) -> Result<Self> {
        // `subcommand_negates_reqs` makes `index` optional at the clap layer;
        // when we reach this branch it must be present.
        let raw = args
            .index
            .as_ref()
            .ok_or_else(|| anyhow!("an index.html path or URL is required"))?;
        let raw_str = raw
            .to_str()
            .ok_or_else(|| anyhow!("index argument is not valid UTF-8"))?;
        let input = Input::parse(raw_str)?;

        let cwd = std::env::current_dir()?;
        let index_dir = match &input {
            Input::File { source_root, .. } => Some(source_root.as_path()),
            Input::Url(_) => None,
        };
        let cfg = config::resolve(&cwd, index_dir, args)?;

        Ok(Self { input, cfg })
    }
}

fn log_header(log: &Logger, inputs: &Inputs) {
    let cfg = &inputs.cfg;
    log.heading("tau");
    log.detail("app", &cfg.name);
    log.detail("identifier", &cfg.identifier);
    log.detail("source", &inputs.input.label());
    log.detail("profile", cfg.profile.dir_name());
    let platforms = cfg
        .platforms
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    log.detail("platforms", &platforms);
}
