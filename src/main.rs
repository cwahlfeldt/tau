mod analyze;
mod build;
mod cache;
mod cli;
mod config;
mod dev;
mod filter;
mod input;
mod log;
mod pipeline;
mod scaffold;
mod signing;
mod trace;

use anyhow::{Context, Result};
use clap::Parser;

use crate::cli::{CacheAction, Cli, Command};

fn main() -> Result<()> {
    let args = Cli::parse();
    match args.command {
        Some(Command::Cache { action }) => run_cache(&action),
        Some(Command::Analyze { index, config, quiet, verbose }) => {
            analyze::run(analyze::AnalyzeArgs { index, config, quiet, verbose })
        }
        Some(Command::Dev {
            index,
            platform,
            name,
            identifier,
            config,
            keep_scaffold,
            quiet,
            verbose,
        }) => dev::run(dev::DevArgs {
            index,
            platform,
            name,
            identifier,
            config,
            keep_scaffold,
            quiet,
            verbose,
        }),
        None => pipeline::run(args),
    }
}

fn run_cache(action: &CacheAction) -> Result<()> {
    let dir = cache::dir().context("could not resolve cache directory")?;
    match action {
        CacheAction::Size => {
            let bytes = cache::size_bytes(&dir)?;
            println!("{}", dir.display());
            println!("{}", cache::format_size(bytes));
        }
        CacheAction::Clear => {
            let bytes = cache::size_bytes(&dir).unwrap_or(0);
            cache::clear(&dir)?;
            println!("cleared {} ({})", dir.display(), cache::format_size(bytes));
        }
        CacheAction::Prune { days, dry_run } => {
            let max_age = std::time::Duration::from_secs(days.saturating_mul(60 * 60 * 24));
            let report = cache::prune(&dir, max_age, *dry_run)?;
            let prefix = if report.dry_run { "would remove" } else { "removed" };
            println!(
                "{} {} files ({}) older than {} days from {}",
                prefix,
                report.files_removed,
                cache::format_size(report.bytes_freed),
                days,
                dir.display()
            );
        }
    }
    Ok(())
}
