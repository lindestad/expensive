//! Configuration loading for the `expensive` binary.
//!
//! This module resolves CLI flags, optional TOML config, and OpenCode database
//! discovery into a single [`Config`]. The intended precedence is:
//! command-line arguments, config file values, then built-in defaults.

use std::{
    env, fmt, fs,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use clap::{Parser, ValueEnum};
use serde::Deserialize;

use crate::time_window::{DailyStart, WeekStart};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Scope {
    All,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ColorTheme {
    #[default]
    Aurora,
    Ember,
    Ocean,
    Forest,
    Graphite,
}

impl ColorTheme {
    pub const ALL: [Self; 5] = [
        Self::Aurora,
        Self::Ember,
        Self::Ocean,
        Self::Forest,
        Self::Graphite,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Self::Aurora => "aurora",
            Self::Ember => "ember",
            Self::Ocean => "ocean",
            Self::Forest => "forest",
            Self::Graphite => "graphite",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Aurora => "Aurora",
            Self::Ember => "Ember",
            Self::Ocean => "Ocean",
            Self::Forest => "Forest",
            Self::Graphite => "Graphite",
        }
    }
}

impl fmt::Display for ColorTheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key())
    }
}

impl FromStr for ColorTheme {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "aurora" => Ok(Self::Aurora),
            "ember" => Ok(Self::Ember),
            "ocean" => Ok(Self::Ocean),
            "forest" => Ok(Self::Forest),
            "graphite" => Ok(Self::Graphite),
            _ => Err(anyhow!(
                "unsupported color theme {value:?}; expected aurora, ember, ocean, forest, or graphite"
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ThemeScope {
    #[default]
    Calendar,
    All,
}

impl ThemeScope {
    pub fn key(self) -> &'static str {
        match self {
            Self::Calendar => "calendar",
            Self::All => "all",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Calendar => "Calendar only",
            Self::All => "Entire TUI",
        }
    }
}

impl fmt::Display for ThemeScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key())
    }
}

impl FromStr for ThemeScope {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "calendar" | "heatmap" => Ok(Self::Calendar),
            "all" | "tui" => Ok(Self::All),
            _ => Err(anyhow!(
                "unsupported theme scope {value:?}; expected calendar or all"
            )),
        }
    }
}

#[derive(Debug, Parser)]
#[command(author, version, about = "OpenCode token and cost dashboard")]
pub struct Cli {
    #[arg(long, value_name = "PATH")]
    pub db: Option<PathBuf>,

    #[arg(long, value_name = "HH:MM")]
    pub daily_start: Option<DailyStart>,

    #[arg(long, value_name = "monday|sunday")]
    pub week_start: Option<WeekStart>,

    #[arg(long, value_name = "SECONDS")]
    pub refresh: Option<u64>,

    #[arg(long)]
    pub no_refresh: bool,

    #[arg(long, value_enum)]
    pub scope: Option<Scope>,

    #[arg(long, value_enum)]
    pub color_theme: Option<ColorTheme>,

    #[arg(long, value_enum)]
    pub theme_scope: Option<ThemeScope>,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub db_path: PathBuf,
    pub config_path: Option<PathBuf>,
    pub daily_start: DailyStart,
    pub week_start: WeekStart,
    pub refresh_interval: Duration,
    pub auto_refresh: bool,
    pub scope: Scope,
    pub color_theme: ColorTheme,
    pub theme_scope: ThemeScope,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    daily_start: Option<String>,
    week_start: Option<String>,
    refresh_seconds: Option<u64>,
    scope: Option<String>,
    color_theme: Option<String>,
    theme_scope: Option<String>,
}

pub fn load(cli: Cli) -> Result<Config> {
    let file_config = load_file_config()?;
    let db_path = discover_db_path(cli.db.clone());
    resolve_config(cli, file_config, db_path, config_path())
}

fn resolve_config(
    cli: Cli,
    file_config: FileConfig,
    db_path: PathBuf,
    config_path: Option<PathBuf>,
) -> Result<Config> {
    let daily_start = match (cli.daily_start, file_config.daily_start.as_deref()) {
        (Some(value), _) => value,
        (None, Some(value)) => value.parse()?,
        (None, None) => DailyStart::default(),
    };

    let week_start = match (cli.week_start, file_config.week_start.as_deref()) {
        (Some(value), _) => value,
        (None, Some(value)) => value.parse()?,
        (None, None) => WeekStart::default(),
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

    let color_theme = match (cli.color_theme, file_config.color_theme.as_deref()) {
        (Some(value), _) => value,
        (None, Some(value)) => value.parse()?,
        (None, None) => ColorTheme::default(),
    };

    let theme_scope = match (cli.theme_scope, file_config.theme_scope.as_deref()) {
        (Some(value), _) => value,
        (None, Some(value)) => value.parse()?,
        (None, None) => ThemeScope::default(),
    };

    Ok(Config {
        db_path,
        config_path,
        daily_start,
        week_start,
        refresh_interval: Duration::from_secs(refresh_seconds),
        auto_refresh: !cli.no_refresh,
        scope,
        color_theme,
        theme_scope,
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

pub fn config_path() -> Option<PathBuf> {
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
            week_start: None,
            refresh: None,
            no_refresh: false,
            scope: None,
            color_theme: None,
            theme_scope: None,
        }
    }

    #[test]
    fn default_config_matches_dashboard_expectations() {
        let config = resolve_config(
            cli(),
            FileConfig::default(),
            PathBuf::from("/tmp/opencode.db"),
            Some(PathBuf::from("/tmp/config.toml")),
        )
        .unwrap();

        assert_eq!(config.db_path, PathBuf::from("/tmp/opencode.db"));
        assert_eq!(config.config_path, Some(PathBuf::from("/tmp/config.toml")));
        assert_eq!(config.daily_start, DailyStart::default());
        assert_eq!(config.week_start, WeekStart::default());
        assert_eq!(config.refresh_interval, Duration::from_secs(60));
        assert!(config.auto_refresh);
        assert_eq!(config.scope, Scope::All);
        assert_eq!(config.color_theme, ColorTheme::Aurora);
        assert_eq!(config.theme_scope, ThemeScope::Calendar);
    }

    #[test]
    fn cli_values_override_file_config() {
        let mut cli = cli();
        cli.daily_start = Some("06:30".parse().unwrap());
        cli.week_start = Some(WeekStart::Sunday);
        cli.refresh = Some(10);
        cli.no_refresh = true;
        cli.scope = Some(Scope::All);
        cli.color_theme = Some(ColorTheme::Ocean);
        cli.theme_scope = Some(ThemeScope::All);

        let file_config = FileConfig {
            daily_start: Some("04:00".to_string()),
            week_start: Some("monday".to_string()),
            refresh_seconds: Some(60),
            scope: Some("all".to_string()),
            color_theme: Some("ember".to_string()),
            theme_scope: Some("calendar".to_string()),
        };

        let config =
            resolve_config(cli, file_config, PathBuf::from("/tmp/opencode.db"), None).unwrap();

        assert_eq!(
            config.daily_start,
            DailyStart {
                hour: 6,
                minute: 30
            }
        );
        assert_eq!(config.week_start, WeekStart::Sunday);
        assert_eq!(config.refresh_interval, Duration::from_secs(10));
        assert!(!config.auto_refresh);
        assert_eq!(config.scope, Scope::All);
        assert_eq!(config.color_theme, ColorTheme::Ocean);
        assert_eq!(config.theme_scope, ThemeScope::All);
    }

    #[test]
    fn file_config_supports_theme_and_week_start() {
        let file_config = FileConfig {
            week_start: Some("sunday".to_string()),
            color_theme: Some("forest".to_string()),
            theme_scope: Some("all".to_string()),
            ..FileConfig::default()
        };

        let config =
            resolve_config(cli(), file_config, PathBuf::from("/tmp/opencode.db"), None).unwrap();

        assert_eq!(config.week_start, WeekStart::Sunday);
        assert_eq!(config.color_theme, ColorTheme::Forest);
        assert_eq!(config.theme_scope, ThemeScope::All);
    }

    #[test]
    fn rejects_zero_refresh_interval() {
        let mut cli = cli();
        cli.refresh = Some(0);

        let error = resolve_config(
            cli,
            FileConfig::default(),
            PathBuf::from("/tmp/opencode.db"),
            None,
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

        let error = resolve_config(cli(), file_config, PathBuf::from("/tmp/opencode.db"), None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("unsupported scope"));
    }

    #[test]
    fn rejects_unknown_theme_values() {
        let file_config = FileConfig {
            color_theme: Some("sparkles".to_string()),
            ..FileConfig::default()
        };

        let error = resolve_config(cli(), file_config, PathBuf::from("/tmp/opencode.db"), None)
            .unwrap_err()
            .to_string();

        assert!(error.contains("unsupported color theme"));
    }
}
