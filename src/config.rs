//! Configuration loading for the `expensive` binary.
//!
//! This module resolves CLI flags, optional TOML config, and OpenCode database
//! discovery into a single [`Config`]. The intended precedence is:
//! command-line arguments, config file values, then built-in defaults.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use serde::Deserialize;

use crate::time_window::DailyStart;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Scope {
    All,
}

#[derive(Debug, Parser)]
#[command(author, version, about = "OpenCode token and cost dashboard")]
pub struct Cli {
    #[arg(long, value_name = "PATH")]
    pub db: Option<PathBuf>,

    #[arg(long, value_name = "HH:MM")]
    pub daily_start: Option<DailyStart>,

    #[arg(long, value_name = "SECONDS")]
    pub refresh: Option<u64>,

    #[arg(long)]
    pub no_refresh: bool,

    #[arg(long, value_enum)]
    pub scope: Option<Scope>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub db_path: PathBuf,
    pub daily_start: DailyStart,
    pub refresh_interval: Duration,
    pub auto_refresh: bool,
    pub scope: Scope,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    daily_start: Option<String>,
    refresh_seconds: Option<u64>,
    scope: Option<String>,
}

pub fn load(cli: Cli) -> Result<Config> {
    let file_config = load_file_config()?;
    let db_path = discover_db_path(cli.db.clone());
    resolve_config(cli, file_config, db_path)
}

fn resolve_config(cli: Cli, file_config: FileConfig, db_path: PathBuf) -> Result<Config> {
    let daily_start = match (cli.daily_start, file_config.daily_start.as_deref()) {
        (Some(value), _) => value,
        (None, Some(value)) => value.parse()?,
        (None, None) => DailyStart::default(),
    };

    let refresh_seconds = cli.refresh.or(file_config.refresh_seconds).unwrap_or(60);
    if refresh_seconds == 0 {
        return Err(anyhow!("refresh interval must be at least 1 second"));
    }

    let scope = match (cli.scope, file_config.scope.as_deref()) {
        (Some(value), _) => value,
        (None, Some("all")) | (None, None) => Scope::All,
        (None, Some(value)) => {
            return Err(anyhow!(
                "unsupported scope {value:?}; v1 supports only \"all\""
            ))
        }
    };

    Ok(Config {
        db_path,
        daily_start,
        refresh_interval: Duration::from_secs(refresh_seconds),
        auto_refresh: !cli.no_refresh,
        scope,
    })
}

fn load_file_config() -> Result<FileConfig> {
    let Some(path) = config_path() else {
        return Ok(FileConfig::default());
    };
    if !path.exists() {
        return Ok(FileConfig::default());
    }

    let content =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("parsing {}", path.display()))
}

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|path| path.join("expensive").join("config.toml"))
}

fn discover_db_path(cli_path: Option<PathBuf>) -> PathBuf {
    if let Some(path) = cli_path {
        return path;
    }

    if let Ok(path) = env::var("OPENCODE_DB_PATH") {
        let path = path.trim();
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }

    if let Some(path) = opencode_db_path() {
        return path;
    }

    dirs::home_dir()
        .unwrap_or_else(|| Path::new(".").to_path_buf())
        .join(".local/share/opencode/opencode.db")
}

fn opencode_db_path() -> Option<PathBuf> {
    let output = Command::new("opencode")
        .args(["db", "path"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let path = stdout.trim();
    if path.is_empty() {
        None
    } else {
        Some(PathBuf::from(path))
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::*;

    fn cli() -> Cli {
        Cli {
            db: None,
            daily_start: None,
            refresh: None,
            no_refresh: false,
            scope: None,
        }
    }

    #[test]
    fn default_config_matches_dashboard_expectations() {
        let config = resolve_config(
            cli(),
            FileConfig::default(),
            PathBuf::from("/tmp/opencode.db"),
        )
        .unwrap();

        assert_eq!(config.db_path, PathBuf::from("/tmp/opencode.db"));
        assert_eq!(config.daily_start, DailyStart::default());
        assert_eq!(config.refresh_interval, Duration::from_secs(60));
        assert!(config.auto_refresh);
        assert_eq!(config.scope, Scope::All);
    }

    #[test]
    fn cli_values_override_file_config() {
        let mut cli = cli();
        cli.daily_start = Some("06:30".parse().unwrap());
        cli.refresh = Some(10);
        cli.no_refresh = true;
        cli.scope = Some(Scope::All);

        let file_config = FileConfig {
            daily_start: Some("04:00".to_string()),
            refresh_seconds: Some(60),
            scope: Some("all".to_string()),
        };

        let config = resolve_config(cli, file_config, PathBuf::from("/tmp/opencode.db")).unwrap();

        assert_eq!(
            config.daily_start,
            DailyStart {
                hour: 6,
                minute: 30
            }
        );
        assert_eq!(config.refresh_interval, Duration::from_secs(10));
        assert!(!config.auto_refresh);
        assert_eq!(config.scope, Scope::All);
    }

    #[test]
    fn rejects_zero_refresh_interval() {
        let mut cli = cli();
        cli.refresh = Some(0);

        let error = resolve_config(
            cli,
            FileConfig::default(),
            PathBuf::from("/tmp/opencode.db"),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("refresh interval"));
    }

    #[test]
    fn rejects_unknown_file_scope() {
        let file_config = FileConfig {
            scope: Some("current".to_string()),
            ..FileConfig::default()
        };

        let error = resolve_config(cli(), file_config, PathBuf::from("/tmp/opencode.db"))
            .unwrap_err()
            .to_string();

        assert!(error.contains("unsupported scope"));
    }
}
