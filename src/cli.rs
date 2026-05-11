use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

use crate::log::Level;

/// Top-level CLI. The default (subcommand-less) invocation wraps an
/// `index.html` (or remote URL) — this preserves the original
/// `tau <index>` form. Subcommands are reserved for adjacent operations
/// (e.g. `cache`, `create`, `dev`, `build`, `add`).
#[derive(Parser, Debug)]
#[command(
    name = "tau",
    version,
    about = "Wrap a local index.html or a remote URL into a desktop or mobile app",
    subcommand_negates_reqs = true,
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Path to a local index.html, or an http(s) URL to wrap directly
    /// (required when not using a subcommand)
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
    /// Scaffold a new tau game project (three.js + Vite, ready to dev).
    Create {
        /// Project name. Becomes the directory name and the default app name.
        name: String,
    },
    /// Run a tau project in dev mode (Vite + Tauri webview), or wrap a
    /// specific index.html / URL the legacy way if one is provided.
    Dev {
        /// Optional path to a local index.html, or an http(s) URL. Omit to
        /// auto-detect a tau project from the current directory.
        index: Option<PathBuf>,

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
    /// Build a tau project for distribution (Vite production build + Tauri bundle).
    Build {
        #[command(flatten)]
        build: BuildFlags,

        /// Comma-separated list of target platforms: macos, windows, linux, android, ios
        #[arg(short, long, value_delimiter = ',')]
        platform: Vec<String>,
    },
    /// Add a JavaScript dependency to the current tau project.
    Add {
        /// Package name (passed through to pnpm/npm). Examples: `cannon-es`, `three@0.169.0`.
        package: String,
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
