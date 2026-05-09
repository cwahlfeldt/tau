mod build;
mod build_project;
mod cache;
mod cli;
mod config;
mod create;
mod dev;
mod filter;
mod input;
mod log;
mod pipeline;
mod scaffold;
mod tooling;

use anyhow::{Context, Result};
use clap::Parser;

use crate::cli::{CacheAction, Cli, Command};
use crate::log::{Level, Logger};

fn main() -> Result<()> {
    let args = Cli::parse();
    match args.command {
        Some(Command::Cache { action }) => run_cache(&action),
        Some(Command::Create { name, quiet, verbose }) => {
            let log = Logger::new(level_from(quiet, verbose));
            create::run(name, &log)
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
        Some(Command::Build {
            release,
            platform,
            name,
            identifier,
            output,
            config,
            keep_scaffold,
            quiet,
            verbose,
        }) => build_project::run(build_project::BuildArgs {
            release,
            platform,
            name,
            identifier,
            output,
            config,
            keep_scaffold,
            quiet,
            verbose,
        }),
        Some(Command::Add { package, quiet, verbose }) => build_project::run_add(package, quiet, verbose),
        None => pipeline::run(args),
    }
}

fn level_from(quiet: bool, verbose: bool) -> Level {
    if quiet {
        Level::Quiet
    } else if verbose {
        Level::Verbose
    } else {
        Level::Normal
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
