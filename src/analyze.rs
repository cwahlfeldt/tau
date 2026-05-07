//! `tau analyze` — read-only inspection of which files would ship in a
//! build right now. Runs the same tracer the build pipeline uses, then
//! groups every file in the source tree into reached / unreached /
//! always-excluded buckets and prints a sorted-by-size report ending
//! with a paste-ready `include` JSON snippet.
//!
//! Useful when:
//!   - You're about to flip on `treeShake` and want to see what would drop.
//!   - The build warned that something looked dropped at runtime — analyze
//!     surfaces the candidates so you can pick what to add to `include`.

use anyhow::{Context, Result};
use globset::{Glob, GlobSetBuilder};
use std::collections::BTreeSet;
use std::path::PathBuf;
use walkdir::WalkDir;

use crate::cli::Cli;
use crate::config::{self, Platform};
use crate::input::Input;
use crate::log::{Level, Logger};
use crate::trace;

pub struct AnalyzeArgs {
    pub index: PathBuf,
    pub config: Option<PathBuf>,
    pub quiet: bool,
    pub verbose: bool,
}

pub fn run(args: AnalyzeArgs) -> Result<()> {
    let level = if args.quiet {
        Level::Quiet
    } else if args.verbose {
        Level::Verbose
    } else {
        Level::Normal
    };
    let log = Logger::new(level);

    let raw = args
        .index
        .to_str()
        .context("index argument is not valid UTF-8")?;
    let input = Input::parse(raw)?;

    let (source_root, entry_rel) = match &input {
        Input::File { source_root, index } => {
            let rel = index
                .strip_prefix(source_root)
                .with_context(|| {
                    format!(
                        "index file {} is not under source root {}",
                        index.display(),
                        source_root.display()
                    )
                })?
                .to_path_buf();
            (source_root.clone(), rel)
        }
        Input::Url(_) => {
            anyhow::bail!("`tau analyze` requires a local index.html, not a URL");
        }
    };

    let synthetic = synthesize_cli(&args);
    let cwd = std::env::current_dir()?;
    let cfg = config::resolve(&cwd, Some(source_root.as_path()), &synthetic)?;

    log.heading("tau analyze");
    log.detail("source", &source_root.display().to_string());
    log.detail("entry", &entry_rel.display().to_string());

    let trace_result = trace::trace(&source_root, &entry_rel)?;

    let exclude_matcher = build_matcher(&cfg.exclude)?;
    let include_matcher = build_matcher(&cfg.include)?;

    let mut all_files: Vec<FileInfo> = Vec::new();
    for entry in WalkDir::new(&source_root).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let abs = entry.path();
        let rel = match abs.strip_prefix(&source_root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        all_files.push(FileInfo { rel, size });
    }

    let mut reached: BTreeSet<PathBuf> = trace_result.reached.clone();
    // include matches act like additional reached files for reporting.
    let mut force_kept: BTreeSet<PathBuf> = BTreeSet::new();
    for f in &all_files {
        if include_matcher.is_match(&f.rel) {
            force_kept.insert(f.rel.clone());
            reached.insert(f.rel.clone());
        }
    }

    // Final shipped set = (reached ∪ include) minus exclude.
    let mut shipped: Vec<&FileInfo> = Vec::new();
    let mut dropped_by_exclude: Vec<&FileInfo> = Vec::new();
    let mut dropped_unreached: Vec<&FileInfo> = Vec::new();
    for f in &all_files {
        let is_reached = reached.contains(&f.rel);
        let is_excluded = exclude_matcher.is_match(&f.rel);
        if is_reached && is_excluded {
            dropped_by_exclude.push(f);
        } else if is_reached {
            shipped.push(f);
        } else if is_excluded {
            // Excluded by default rules (.git, node_modules, etc.) —
            // don't bother reporting these as "candidates"; they were
            // never going to ship.
        } else {
            dropped_unreached.push(f);
        }
    }

    shipped.sort_by(|a, b| b.size.cmp(&a.size));
    dropped_unreached.sort_by(|a, b| b.size.cmp(&a.size));
    dropped_by_exclude.sort_by(|a, b| b.size.cmp(&a.size));

    let total_shipped: u64 = shipped.iter().map(|f| f.size).sum();
    let total_dropped: u64 = dropped_unreached.iter().map(|f| f.size).sum();

    println!();
    println!("Files reachable from {}:", entry_rel.display());
    println!("  shipped:   {} files ({})", shipped.len(), human_size(total_shipped));
    println!(
        "  unreached: {} files ({}) — would be excluded by tree-shake",
        dropped_unreached.len(),
        human_size(total_dropped)
    );
    if !dropped_by_exclude.is_empty() {
        let total_xx: u64 = dropped_by_exclude.iter().map(|f| f.size).sum();
        println!(
            "  reached-but-excluded: {} files ({}) — `exclude` overlay drops these",
            dropped_by_exclude.len(),
            human_size(total_xx)
        );
    }
    if !force_kept.is_empty() {
        println!(
            "  force-kept by `include`: {} files",
            force_kept.len()
        );
    }
    println!();

    if !trace_result.warnings.is_empty() {
        println!("Tracer warnings:");
        for w in &trace_result.warnings {
            println!("  - {}", w);
        }
        println!();
    }

    if !trace_result.ambiguous.is_empty() {
        println!("Ambiguous reference sites (review and add `include` if needed):");
        for note in &trace_result.ambiguous {
            println!("  - {} :: {}", note.file.display(), note.kind);
        }
        println!();
    }

    if !dropped_unreached.is_empty() {
        println!("Top unreached files (by size):");
        for f in dropped_unreached.iter().take(20) {
            println!("  {:>10}  {}", human_size(f.size), f.rel.display());
        }
        println!();
        println!("If any of these are loaded dynamically and you need them at runtime,");
        println!("add globs to `include` in tau.conf.json. Example:");
        println!("{{");
        println!("  \"include\": [");
        let sample: Vec<&FileInfo> = dropped_unreached.iter().take(5).copied().collect();
        for (i, f) in sample.iter().enumerate() {
            let comma = if i + 1 < sample.len() { "," } else { "" };
            println!("    {:?}{}", f.rel.to_string_lossy(), comma);
        }
        println!("  ]");
        println!("}}");
        println!();
    }

    Ok(())
}

struct FileInfo {
    rel: PathBuf,
    size: u64,
}

fn build_matcher(patterns: &[String]) -> Result<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pat in patterns {
        let glob = Glob::new(pat).with_context(|| format!("invalid glob: {}", pat))?;
        builder.add(glob);
    }
    builder.build().context("build glob matcher")
}

fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}

/// Mint a `Cli` from `AnalyzeArgs` so we can reuse `config::resolve`.
fn synthesize_cli(args: &AnalyzeArgs) -> Cli {
    Cli {
        index: None,
        release: false,
        no_tree_shake: false,
        platform: vec![Platform::host().as_str().to_string()],
        name: None,
        identifier: None,
        output: None,
        config: args.config.clone(),
        dry_run: false,
        keep_scaffold: false,
        quiet: args.quiet,
        verbose: args.verbose,
        command: None,
    }
}
