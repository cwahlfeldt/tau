//! Tiny structured logger. Centralizes the `==>` / four-space-indent format
//! that was previously sprinkled across modules, and lets library code stay
//! free of `println!` calls (so it can be tested without capturing stdout).

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Quiet,
    Normal,
    Verbose,
}

#[derive(Debug, Clone)]
pub struct Logger {
    level: Level,
}

impl Logger {
    pub fn new(level: Level) -> Self {
        Self { level }
    }

    /// Heading like `==> Building macos`. Always shown unless quiet.
    pub fn heading(&self, msg: &str) {
        if self.level >= Level::Normal {
            println!("==> {}", msg);
        }
    }

    /// Indented step line like `    rustup target add ...`. Normal+.
    pub fn step(&self, msg: &str) {
        if self.level >= Level::Normal {
            println!("    {}", msg);
        }
    }

    /// `key: value` style detail line. Normal+.
    pub fn detail(&self, key: &str, value: &str) {
        if self.level >= Level::Normal {
            println!("    {:<11} {}", format!("{}:", key), value);
        }
    }

    /// Shell command being executed. Normal+.
    pub fn command(&self, cmd: &str) {
        if self.level >= Level::Normal {
            println!("    $ {}", cmd);
        }
    }

    /// Indented arrow like `    -> /path/to/artifact`. Normal+.
    pub fn artifact(&self, path: &std::path::Path) {
        if self.level >= Level::Normal {
            println!("    -> {}", path.display());
        }
    }

    /// Final status line. Always shown unless quiet.
    pub fn done(&self, msg: &str) {
        if self.level >= Level::Normal {
            println!("\n{}", msg);
        }
    }
}
