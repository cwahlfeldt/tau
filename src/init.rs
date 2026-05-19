//! `tau init` — drop a starter `tau.conf.json` into the current directory.
//!
//! No index.html, no .gitignore, no scaffolding. The whole point of `tau` is
//! "no configuration required until necessary"; this command exists only for
//! the moment a user decides to pin their app name and bundle identifier.

use anyhow::{bail, Context, Result};
use serde_json::json;

use crate::config::{default_identifier, CONFIG_FILE, DEFAULT_NAME, DEFAULT_VERSION};
use crate::log::Logger;

pub struct InitArgs {
    pub name: Option<String>,
    pub identifier: Option<String>,
    pub force: bool,
    pub log: Logger,
}

pub fn run(args: InitArgs) -> Result<()> {
    let cwd = std::env::current_dir().context("could not determine current directory")?;
    write_conf(&cwd, &args)?;
    args.log.heading("tau init");
    args.log.detail("wrote", &cwd.join(CONFIG_FILE).display().to_string());
    args.log.done("Drop your built site here and run `tau .`");
    Ok(())
}

fn write_conf(cwd: &std::path::Path, args: &InitArgs) -> Result<()> {
    let target = cwd.join(CONFIG_FILE);
    if target.exists() && !args.force {
        bail!(
            "{} already exists; pass --force to overwrite",
            target.display()
        );
    }

    let name = args
        .name
        .clone()
        .or_else(|| cwd_dir_name(cwd))
        .unwrap_or_else(|| DEFAULT_NAME.to_string());
    let identifier = args
        .identifier
        .clone()
        .unwrap_or_else(|| default_identifier(&name));

    let conf = json!({
        "name": name,
        "version": DEFAULT_VERSION,
        "identifier": identifier,
    });
    let pretty = serde_json::to_string_pretty(&conf)
        .context("failed to serialize tau.conf.json")?;
    std::fs::write(&target, pretty)
        .with_context(|| format!("failed to write {}", target.display()))?;
    Ok(())
}

fn cwd_dir_name(cwd: &std::path::Path) -> Option<String> {
    cwd.file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn quiet_args(name: Option<String>, identifier: Option<String>, force: bool) -> InitArgs {
        InitArgs {
            name,
            identifier,
            force,
            log: Logger::new(crate::log::Level::Quiet),
        }
    }

    fn read(target: &PathBuf) -> serde_json::Value {
        let raw = std::fs::read_to_string(target).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn writes_conf_with_cwd_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("my-cool-app");
        std::fs::create_dir(&dir).unwrap();
        let args = quiet_args(None, None, false);
        write_conf(&dir, &args).unwrap();

        let v = read(&dir.join(CONFIG_FILE));
        assert_eq!(v["name"], "my-cool-app");
        assert_eq!(v["identifier"], "com.tau.mycoolapp");
        assert_eq!(v["version"], DEFAULT_VERSION);
    }

    #[test]
    fn respects_explicit_name_and_identifier() {
        let tmp = tempfile::tempdir().unwrap();
        let args = quiet_args(
            Some("My Game".to_string()),
            Some("io.example.game".to_string()),
            false,
        );
        write_conf(tmp.path(), &args).unwrap();

        let v = read(&tmp.path().join(CONFIG_FILE));
        assert_eq!(v["name"], "My Game");
        assert_eq!(v["identifier"], "io.example.game");
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(CONFIG_FILE), "existing").unwrap();
        let args = quiet_args(None, None, false);
        let err = write_conf(tmp.path(), &args).unwrap_err();
        assert!(err.to_string().contains("--force"), "got: {}", err);
        // Existing file should be untouched.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(CONFIG_FILE)).unwrap(),
            "existing"
        );
    }

    #[test]
    fn overwrites_with_force() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join(CONFIG_FILE);
        std::fs::write(&target, "existing").unwrap();
        let args = quiet_args(Some("Replaced".to_string()), None, true);
        write_conf(tmp.path(), &args).unwrap();

        let v = read(&target);
        assert_eq!(v["name"], "Replaced");
    }
}
