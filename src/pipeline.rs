//! End-to-end orchestration of a single wrap.
//!
//! The pipeline is intentionally linear: resolve config, scaffold a Tauri
//! project in a tempdir whose `frontendDist` points at the user's source
//! directory (no asset copying or HTML rewriting), then build & extract
//! artifacts for each requested platform.

use anyhow::{anyhow, Context, Result};
use globset::{Glob, GlobSetBuilder};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::cli::Cli;
use crate::filter;
use crate::input::Input;
use crate::log::Logger;
use crate::trace;
use crate::{build, config, scaffold};

pub fn run(args: Cli) -> Result<()> {
    let log = Logger::new(args.log_level());
    let inputs = Inputs::resolve(&args)?;

    log_header(&log, &inputs);

    let workdir = tempfile::Builder::new()
        .prefix("tau-")
        .tempdir()
        .context("failed to create temp working directory")?;
    let project_dir = workdir.path().to_path_buf();

    // For local-file inputs we materialize a filtered copy of the source
    // tree and point `frontendDist` at *that*, instead of the source dir
    // itself. Tauri's bundler walks the whole frontendDist directory, so
    // anything sitting next to `index.html` (`.git`, `node_modules`,
    // prior `build/` output, dev-only asset variants) would otherwise
    // ship inside the app. Two filtering modes:
    //   - tree_shake on (default): only files reachable from `index.html`
    //     plus user `include` matches plus the entry file itself ship.
    //     Everything else dropped, with a build-end warning summarizing
    //     what was dropped so users have a discoverable diagnostic path
    //     when something dynamic was missed.
    //   - tree_shake off: ship the whole tree minus `exclude` globs.
    //     The original "we don't rewrite, just point" mode.
    // Either way the materialized handle is held until after the build.
    let mut shake_report: Option<ShakeReport> = None;
    let _frontend = match &inputs.input {
        Input::File { source_root, index } => {
            let entry_rel = index
                .strip_prefix(source_root)
                .with_context(|| {
                    format!(
                        "index file {} is not under source root {}",
                        index.display(),
                        source_root.display()
                    )
                })?
                .to_path_buf();

            let materialized = if inputs.cfg.tree_shake {
                let trace_result = trace::trace(source_root, &entry_rel)
                    .context("trace reachable files")?;
                let allowed = build_allowlist(
                    source_root,
                    &entry_rel,
                    &trace_result,
                    &inputs.cfg.include,
                )?;
                let report = ShakeReport::compute(source_root, &allowed, &trace_result)?;
                let m = filter::materialize_allowlist(
                    source_root,
                    &allowed,
                    &inputs.cfg.exclude,
                )
                .context("filter source tree")?;
                shake_report = Some(report);
                m
            } else {
                filter::materialize(source_root, &inputs.cfg.exclude)
                    .context("filter source tree")?
            };

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

    if args.dry_run {
        let kept_scaffold = workdir.keep();
        log.done(&format!("Dry run: scaffold preserved at {}", kept_scaffold.display()));
        if let Some(m) = _frontend {
            let kept_frontend = m.dir.keep();
            log.done(&format!("Frontend preserved at {}", kept_frontend.display()));
        }
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

    if let Some(report) = &shake_report {
        report.print();
    }

    log.done(&format!("Done. Artifacts in {}", output_dir.display()));
    Ok(())
}

/// Compose the set of relative paths that should ship in the bundle:
/// files reached by the tracer, plus the entry file itself (always),
/// plus anything matching a user `include` glob.
fn build_allowlist(
    source_root: &Path,
    entry_rel: &Path,
    trace: &trace::TraceResult,
    include_patterns: &[String],
) -> Result<BTreeSet<PathBuf>> {
    let mut allowed: BTreeSet<PathBuf> = trace.reached.clone();
    allowed.insert(entry_rel.to_path_buf());

    if !include_patterns.is_empty() {
        let mut builder = GlobSetBuilder::new();
        for pat in include_patterns {
            let glob =
                Glob::new(pat).with_context(|| format!("invalid include pattern: {}", pat))?;
            builder.add(glob);
        }
        let matcher = builder.build().context("build include matcher")?;

        for entry in WalkDir::new(source_root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = match entry.path().strip_prefix(source_root) {
                Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
                _ => continue,
            };
            if matcher.is_match(&rel) {
                allowed.insert(rel);
            }
        }
    }
    Ok(allowed)
}

/// Build-end summary of what tree-shake dropped — printed *after* the
/// successful build so the warning is the last thing the user sees.
/// The point of this report is to make the failure mode discoverable:
/// if the app misbehaves at runtime, the user already saw the list of
/// dropped files and the suggested fix.
struct ShakeReport {
    dropped: Vec<DroppedFile>,
    total_dropped_bytes: u64,
    ambiguous: Vec<trace::AmbiguityNote>,
    warnings: Vec<String>,
}

struct DroppedFile {
    rel: PathBuf,
    size: u64,
}

impl ShakeReport {
    fn compute(
        source_root: &Path,
        allowed: &BTreeSet<PathBuf>,
        trace_result: &trace::TraceResult,
    ) -> Result<Self> {
        let mut dropped = Vec::new();
        let mut total = 0u64;
        for entry in WalkDir::new(source_root).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = match entry.path().strip_prefix(source_root) {
                Ok(r) if !r.as_os_str().is_empty() => r.to_path_buf(),
                _ => continue,
            };
            if !allowed.contains(&rel) {
                let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                total += size;
                dropped.push(DroppedFile { rel, size });
            }
        }
        dropped.sort_by(|a, b| b.size.cmp(&a.size));
        Ok(Self {
            dropped,
            total_dropped_bytes: total,
            ambiguous: trace_result.ambiguous.clone(),
            warnings: trace_result.warnings.clone(),
        })
    }

    fn print(&self) {
        if self.dropped.is_empty() && self.ambiguous.is_empty() {
            return;
        }
        println!();
        println!("─── tree-shake report ───");
        if !self.dropped.is_empty() {
            println!(
                "  dropped {} file(s) ({}) not reachable from index.html",
                self.dropped.len(),
                human_size(self.total_dropped_bytes),
            );
            for f in self.dropped.iter().take(10) {
                println!("    {:>10}  {}", human_size(f.size), f.rel.display());
            }
            if self.dropped.len() > 10 {
                println!("    ... ({} more)", self.dropped.len() - 10);
            }
        }
        if !self.ambiguous.is_empty() {
            println!();
            println!("  ambiguous reference sites (may load files dynamically):");
            for note in &self.ambiguous {
                println!("    - {} :: {}", note.file.display(), note.kind);
            }
        }
        if !self.warnings.is_empty() {
            println!();
            println!("  tracer warnings:");
            for w in self.warnings.iter().take(10) {
                println!("    - {}", w);
            }
            if self.warnings.len() > 10 {
                println!("    ... ({} more)", self.warnings.len() - 10);
            }
        }
        println!();
        println!(
            "  → if your app misbehaves at runtime, run `tau analyze <index.html>`"
        );
        println!(
            "    or add globs to `include` in tau.conf.json (or rebuild with --no-tree-shake)."
        );
        println!();
    }
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
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
