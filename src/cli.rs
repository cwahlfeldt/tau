use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

use crate::log::Level;

/// Top-level CLI. The default (subcommand-less) invocation wraps an
/// `index.html`, a directory containing one, or a remote URL. Subcommands
/// cover the adjacent operations: `dev`, `build`, `init`, `cache`.
#[derive(Parser, Debug)]
#[command(
    name = "tau",
    version,
    about = "Wrap a local index.html or a remote URL into a desktop or mobile app",
    subcommand_negates_reqs = true,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Path to a local index.html (or a directory containing one), or an
    /// http(s) URL to wrap directly (required when not using a subcommand)
    pub index: Option<PathBuf>,

    #[command(flatten)]
    pub build: BuildFlags,

    /// Comma-separated list of target platforms: macos, windows, linux, android, ios
    #[arg(short, long, value_delimiter = ',')]
    pub platform: Vec<String>,

    /// Generate the scaffold and print its path, but do not run the build.
    #[arg(long)]
    pub dry_run: bool,

    #[command(flatten)]
    pub common: CommonFlags,

    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Flags shared by every subcommand that produces output.
#[derive(Args, Debug, Default, Clone)]
pub struct CommonFlags {
    /// Suppress all non-error output.
    #[arg(short, long, conflicts_with = "verbose", global = true)]
    pub quiet: bool,

    /// Show extra output.
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

impl CommonFlags {
    pub fn level(&self) -> Level {
        if self.quiet {
            Level::Quiet
        } else if self.verbose {
            Level::Verbose
        } else {
            Level::Normal
        }
    }
}

/// Flags shared by the wrap/dev/build paths that drive a Tauri scaffold.
#[derive(Args, Debug, Default, Clone)]
pub struct BuildFlags {
    /// Build with the release profile (optimized + stripped). Unsigned.
    #[arg(long)]
    pub release: bool,

    /// Override the app name
    #[arg(long)]
    pub name: Option<String>,

    /// Override the bundle identifier (e.g. com.example.app)
    #[arg(long)]
    pub identifier: Option<String>,

    /// Override the output directory for built artifacts
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Path to a tau.conf.json (defaults to ./tau.conf.json)
    #[arg(long)]
    pub config: Option<PathBuf>,

    /// Keep the temporary scaffold directory after the build completes.
    #[arg(long)]
    pub keep_scaffold: bool,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Inspect or prune the shared CARGO_TARGET_DIR used to speed up rebuilds.
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// Run a wrapped app in dev mode against a local index.html, a directory
    /// containing one, or a remote URL.
    Dev {
        /// Path to a local index.html, a directory containing one, or an http(s) URL.
        index: PathBuf,

        /// Single target platform: macos, windows, linux, android, ios. Defaults to host.
        #[arg(short, long)]
        platform: Option<String>,

        /// Override the app name
        #[arg(long)]
        name: Option<String>,

        /// Override the bundle identifier (e.g. com.example.app)
        #[arg(long)]
        identifier: Option<String>,

        /// Path to a tau.conf.json (defaults to ./tau.conf.json)
        #[arg(long)]
        config: Option<PathBuf>,

        /// Keep the temporary scaffold directory after dev exits.
        #[arg(long)]
        keep_scaffold: bool,
    },
    /// Build a wrapped app for distribution. Mirrors the top-level `tau <index>` form.
    Build {
        /// Path to a local index.html, a directory containing one, or an http(s) URL.
        index: PathBuf,

        #[command(flatten)]
        build: BuildFlags,

        /// Comma-separated list of target platforms: macos, windows, linux, android, ios
        #[arg(short, long, value_delimiter = ',')]
        platform: Vec<String>,

        /// Generate the scaffold and print its path, but do not run the build.
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a starter tau.conf.json into the current directory.
    Init {
        /// Override the app name (defaults to the current directory name).
        #[arg(long)]
        name: Option<String>,

        /// Override the bundle identifier (defaults to com.tau.<slug>).
        #[arg(long)]
        identifier: Option<String>,

        /// Overwrite an existing tau.conf.json if present.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum CacheAction {
    /// Show the cache directory path and total size on disk.
    Size,
    /// Delete the entire cache directory. The next build will rebuild from scratch.
    Clear,
    /// Delete cache entries last touched more than `--days` ago (default: 30).
    Prune {
        /// Minimum age in days before an artifact is eligible for deletion.
        #[arg(long, default_value_t = 30)]
        days: u64,
        /// Print what would be deleted without actually deleting.
        #[arg(long)]
        dry_run: bool,
    },
}
