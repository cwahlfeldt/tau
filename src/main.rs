mod build;
mod cache;
mod cli;
mod config;
mod dev;
mod filter;
mod init;
mod input;
mod log;
mod pipeline;
mod scaffold;
mod signing;

use anyhow::{anyhow, Result};
use clap::Parser;

use crate::cli::{Cli, Command};
use crate::log::Logger;

fn main() -> Result<()> {
    let args = Cli::parse();
    let log = Logger::new(args.common.level());
    match args.command {
        Some(Command::Cache { action }) => cache::run_command(&action),
        Some(Command::Dev {
            index,
            platform,
            name,
            identifier,
            config,
            keep_scaffold,
        }) => dev::run(dev::DevArgs {
            index,
            platform,
            name,
            identifier,
            config,
            keep_scaffold,
            log,
        }),
        Some(Command::Build {
            index,
            build,
            platform,
            dry_run,
        }) => pipeline::run(pipeline::BuildArgs { index, build, platform, dry_run, log }),
        Some(Command::Init {
            name,
            identifier,
            force,
        }) => init::run(init::InitArgs { name, identifier, force, log }),
        None => {
            let index = args
                .index
                .ok_or_else(|| anyhow!("an index.html path or URL is required"))?;
            pipeline::run(pipeline::BuildArgs {
                index,
                build: args.build,
                platform: args.platform,
                dry_run: args.dry_run,
                log,
            })
        }
    }
}
