//! End-to-end orchestration of a single wrap.
//!
//! The pipeline is intentionally linear: resolve config, scaffold a Tauri
//! project in a tempdir whose `frontendDist` points at the user's source
//! directory (no asset copying or HTML rewriting), then build & extract
//! artifacts for each requested platform.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

use crate::cli::{BuildFlags, Cli};
use crate::config::Overrides;
use crate::filter;
use crate::input::Input;
use crate::log::Logger;
use crate::{build, config, scaffold};

/// Arguments for the `tau build <index>` subcommand. Mirrors what the
/// top-level wrap form picks up from `Cli`.
pub struct BuildArgs {
    pub index: PathBuf,
    pub build: BuildFlags,
    pub platform: Vec<String>,
    pub dry_run: bool,
    pub log: Logger,
}

pub fn run(args: Cli) -> Result<()> {
    let log = Logger::new(args.common.level());
    let raw = args
        .index
        .as_ref()
        .ok_or_else(|| anyhow!("an index.html path or URL is required"))?
        .clone();
    let inputs = Inputs::resolve(&raw, &args.build, &args.platform)?;
    execute(inputs, args.dry_run, args.build.keep_scaffold, log)
}

pub fn run_build(args: BuildArgs) -> Result<()> {
    let inputs = Inputs::resolve(&args.index, &args.build, &args.platform)?;
    execute(inputs, args.dry_run, args.build.keep_scaffold, args.log)
}

fn execute(inputs: Inputs, dry_run: bool, keep_scaffold: bool, log: Logger) -> Result<()> {
    log_header(&log, &inputs);

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
    let _frontend = match &inputs.input {
        Input::File { source_root, .. } => {
            let materialized = filter::materialize(source_root, &inputs.cfg.exclude)
                .context("filter source tree")?;
            log.detail(
                "frontend",
                &format!(
                    "{} ({} files, {} excluded)",
                    materialized.path().display(),
                    materialized.copied,
                    materialized.excluded
                ),
            );
            scaffold::create_for_source(&project_dir, &inputs.cfg, materialized.path())?;
            Some(materialized)
        }
        Input::Url(url) => {
            scaffold::create_for_url(&project_dir, &inputs.cfg, url)?;
            None
        }
    };
    log.detail("scaffold", &project_dir.display().to_string());

    if dry_run {
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

    if keep_scaffold {
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
    fn resolve(raw: &std::path::Path, build_flags: &BuildFlags, platforms: &[String]) -> Result<Self> {
        let raw_str = raw
            .to_str()
            .ok_or_else(|| anyhow!("index argument is not valid UTF-8"))?;
        let input = Input::parse(raw_str)?;

        let cwd = std::env::current_dir()?;
        let index_dir = match &input {
            Input::File { source_root, .. } => Some(source_root.as_path()),
            Input::Url(_) => None,
        };
        let overrides = Overrides {
            name: build_flags.name.clone(),
            identifier: build_flags.identifier.clone(),
            output: build_flags.output.clone(),
            config: build_flags.config.clone(),
            platforms: platforms.to_vec(),
            release: build_flags.release,
        };
        let cfg = config::resolve(&cwd, index_dir, &overrides)?;

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
