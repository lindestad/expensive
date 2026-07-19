//! Configuration loading for the `expensive` binary.
//!
//! This module resolves CLI flags, optional TOML config, and OpenCode database
//! discovery into a single [`Config`]. The intended precedence is:
//! command-line arguments, config file values, then built-in defaults.

use std::{
    collections::BTreeMap,
    env, fmt, fs,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    str::FromStr,
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::time_window::{DailyStart, WeekStart};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Scope {
    All,
    Current,
    Project(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelAliases {
    pub providers: BTreeMap<String, String>,
    pub models: BTreeMap<String, String>,
}

impl ModelAliases {
    pub fn display_name(&self, provider: &str, model_id: &str, variant: &str) -> String {
        let provider = cleaned(provider, "unknown");
        let model_id = cleaned(model_id, "unknown");
        let variant = cleaned(variant, "default");
        let full_model_id = format!("{provider}/{model_id}");
        let base = self
            .models
            .get(&full_model_id)
            .or_else(|| self.models.get(model_id))
            .cloned()
            .unwrap_or_else(|| {
                let provider = self
                    .providers
                    .get(provider)
                    .map(String::as_str)
                    .unwrap_or(provider);
                if provider == "unknown" {
                    model_id.to_string()
                } else {
                    format!("{provider}/{model_id}")
                }
            });

        if variant == "default" {
            base
        } else {
            format!("{base} ({variant})")
        }
    }
}

fn cleaned<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    let value = value.trim();
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

impl Scope {
    pub fn key(&self) -> String {
        match self {
            Self::All => "all".to_string(),
            Self::Current => "current".to_string(),
            Self::Project(id) => format!("project:{id}"),
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key())
    }
}

impl FromStr for Scope {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let value = value.trim();
        match value.to_ascii_lowercase().as_str() {
            "all" => Ok(Self::All),
            "current" | "cwd" => Ok(Self::Current),
            _ => value
                .strip_prefix("project:")
                .filter(|id| !id.trim().is_empty())
                .map(|id| Self::Project(id.trim().to_string()))
                .ok_or_else(|| {
                    anyhow!("unsupported scope {value:?}; expected all, current, or project:<id>")
                }),
        }
    }
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
    #[arg(long, value_name = "PATH", global = true)]
    pub db: Option<PathBuf>,

    /// Expensive's local normalized usage index.
    #[arg(long, value_name = "PATH", global = true)]
    pub index: Option<PathBuf>,

    #[arg(long, value_name = "HH:MM")]
    pub daily_start: Option<DailyStart>,

    #[arg(long, value_name = "monday|sunday")]
    pub week_start: Option<WeekStart>,

    #[arg(long, value_name = "SECONDS")]
    pub refresh: Option<u64>,

    #[arg(long)]
    pub no_refresh: bool,

    #[arg(long, value_name = "all|current|project:ID", global = true)]
    pub scope: Option<Scope>,

    #[arg(long, value_enum)]
    pub color_theme: Option<ColorTheme>,

    #[arg(long, value_enum)]
    pub theme_scope: Option<ThemeScope>,

    #[command(subcommand)]
    pub command: Option<CliCommand>,
}

#[derive(Clone, Debug, Eq, PartialEq, Subcommand)]
pub enum CliCommand {
    /// Check database access, schema compatibility, and optional capabilities.
    Doctor,
    /// Write a machine-readable usage report to stdout.
    Report(ReportArgs),
}

#[derive(Clone, Debug, Eq, PartialEq, Args)]
pub struct ReportArgs {
    /// Bucket/window shape used when explicit bounds are omitted.
    #[arg(long, value_enum, default_value = "daily")]
    pub period: ReportPeriod,

    /// Inclusive local or RFC 3339 lower bound.
    #[arg(long, value_name = "DATE|DATETIME")]
    pub from: Option<String>,

    /// Exclusive local or RFC 3339 upper bound.
    #[arg(long, value_name = "DATE|DATETIME")]
    pub to: Option<String>,

    /// Pretty-print JSON instead of emitting one compact line.
    #[arg(long)]
    pub pretty: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ReportPeriod {
    #[default]
    Daily,
    Weekly,
    Monthly,
    All,
}

impl ReportPeriod {
    pub fn mode(self) -> crate::time_window::Mode {
        match self {
            Self::Daily => crate::time_window::Mode::Daily,
            Self::Weekly => crate::time_window::Mode::Weekly,
            Self::Monthly => crate::time_window::Mode::Monthly,
            Self::All => crate::time_window::Mode::AllTime,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub db_path: PathBuf,
    pub index_path: PathBuf,
    pub codex_home: PathBuf,
    pub pi_sessions_root: PathBuf,
    pub current_directory: PathBuf,
    pub config_path: Option<PathBuf>,
    pub daily_start: DailyStart,
    pub week_start: WeekStart,
    pub refresh_interval: Duration,
    pub auto_refresh: bool,
    pub show_comparison: bool,
    pub scope: Scope,
    pub color_theme: ColorTheme,
    pub theme_scope: ThemeScope,
    pub aliases: ModelAliases,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    daily_start: Option<String>,
    week_start: Option<String>,
    refresh_seconds: Option<u64>,
    auto_refresh: Option<bool>,
    show_comparison: Option<bool>,
    scope: Option<String>,
    color_theme: Option<String>,
    theme_scope: Option<String>,
    #[serde(default)]
    provider_aliases: BTreeMap<String, String>,
    #[serde(default)]
    model_aliases: BTreeMap<String, String>,
}

pub fn load(cli: Cli) -> Result<Config> {
    let file_config = load_file_config()?;
    let db_path = discover_db_path(cli.db.clone());
    let index_path = discover_index_path(cli.index.clone());
    let current_directory = env::current_dir().context("reading current directory")?;
    resolve_config(
        cli,
        file_config,
        db_path,
        index_path,
        config_path(),
        current_directory,
    )
}

fn resolve_config(
    cli: Cli,
    file_config: FileConfig,
    db_path: PathBuf,
    index_path: PathBuf,
    config_path: Option<PathBuf>,
    current_directory: PathBuf,
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
    let auto_refresh = if cli.no_refresh {
        false
    } else {
        file_config.auto_refresh.unwrap_or(true)
    };
    let show_comparison = file_config.show_comparison.unwrap_or(false);

    let scope = match (cli.scope, file_config.scope.as_deref()) {
        (Some(value), _) => value,
        (None, Some(value)) => value.parse()?,
        (None, None) => Scope::All,
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
        index_path,
        codex_home: discover_codex_home(),
        pi_sessions_root: discover_pi_sessions_root(),
        current_directory,
        config_path,
        daily_start,
        week_start,
        refresh_interval: Duration::from_secs(refresh_seconds),
        auto_refresh,
        show_comparison,
        scope,
        color_theme,
        theme_scope,
        aliases: ModelAliases {
            providers: file_config.provider_aliases,
            models: file_config.model_aliases,
        },
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

pub fn save(config: &Config) -> Result<()> {
    let path = config
        .config_path
        .as_ref()
        .ok_or_else(|| anyhow!("config path is unavailable"))?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }

    fs::write(path, format_config(config)).with_context(|| format!("writing {}", path.display()))
}

fn format_config(config: &Config) -> String {
    let mut content = format!(
        concat!(
            "daily_start = \"{}\"\n",
            "week_start = \"{}\"\n",
            "refresh_seconds = {}\n",
            "auto_refresh = {}\n",
            "show_comparison = {}\n",
            "color_theme = \"{}\"\n",
            "theme_scope = \"{}\"\n",
            "scope = \"{}\"\n",
        ),
        config.daily_start,
        config.week_start,
        config.refresh_interval.as_secs(),
        config.auto_refresh,
        config.show_comparison,
        config.color_theme,
        config.theme_scope,
        config.scope,
    );
    let aliases = SerializableAliases {
        provider_aliases: &config.aliases.providers,
        model_aliases: &config.aliases.models,
    };
    if !config.aliases.providers.is_empty() || !config.aliases.models.is_empty() {
        content.push('\n');
        content.push_str(&toml::to_string(&aliases).expect("serializing string alias maps"));
    }
    content
}

#[derive(Serialize)]
struct SerializableAliases<'a> {
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    provider_aliases: &'a BTreeMap<String, String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    model_aliases: &'a BTreeMap<String, String>,
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

fn discover_index_path(cli_path: Option<PathBuf>) -> PathBuf {
    if let Some(path) = cli_path {
        return path;
    }
    if let Ok(path) = env::var("EXPENSIVE_INDEX_PATH") {
        let path = path.trim();
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    dirs::data_local_dir()
        .or_else(dirs::data_dir)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
        .unwrap_or_else(|| Path::new(".").to_path_buf())
        .join("expensive")
        .join("usage.sqlite3")
}

fn discover_codex_home() -> PathBuf {
    env::var("CODEX_HOME")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
        .unwrap_or_else(|| Path::new(".codex").to_path_buf())
}

fn discover_pi_sessions_root() -> PathBuf {
    if let Ok(path) = env::var("PI_CODING_AGENT_SESSION_DIR") {
        let path = path.trim();
        if !path.is_empty() {
            return PathBuf::from(path);
        }
    }
    env::var("PI_CODING_AGENT_DIR")
        .ok()
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".pi/agent")))
        .unwrap_or_else(|| Path::new(".pi/agent").to_path_buf())
        .join("sessions")
}

fn opencode_db_path() -> Option<PathBuf> {
    let output = ProcessCommand::new("opencode")
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
    use std::{fs, path::PathBuf, time::Duration};

    use super::*;

    fn cli() -> Cli {
        Cli {
            db: None,
            index: None,
            daily_start: None,
            week_start: None,
            refresh: None,
            no_refresh: false,
            scope: None,
            color_theme: None,
            theme_scope: None,
            command: None,
        }
    }

    #[test]
    fn default_config_matches_dashboard_expectations() {
        let config = resolve_config(
            cli(),
            FileConfig::default(),
            PathBuf::from("/tmp/opencode.db"),
            PathBuf::from("/tmp/expensive.sqlite3"),
            Some(PathBuf::from("/tmp/config.toml")),
            PathBuf::from("/tmp/project"),
        )
        .unwrap();

        assert_eq!(config.db_path, PathBuf::from("/tmp/opencode.db"));
        assert_eq!(config.index_path, PathBuf::from("/tmp/expensive.sqlite3"));
        assert_eq!(config.current_directory, PathBuf::from("/tmp/project"));
        assert_eq!(config.config_path, Some(PathBuf::from("/tmp/config.toml")));
        assert_eq!(config.daily_start, DailyStart::default());
        assert_eq!(config.week_start, WeekStart::default());
        assert_eq!(config.refresh_interval, Duration::from_secs(60));
        assert!(config.auto_refresh);
        assert!(!config.show_comparison);
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
            auto_refresh: Some(true),
            show_comparison: Some(true),
            scope: Some("all".to_string()),
            color_theme: Some("ember".to_string()),
            theme_scope: Some("calendar".to_string()),
            provider_aliases: BTreeMap::new(),
            model_aliases: BTreeMap::new(),
        };

        let config = resolve_config(
            cli,
            file_config,
            PathBuf::from("/tmp/opencode.db"),
            PathBuf::from("/tmp/expensive.sqlite3"),
            None,
            PathBuf::from("/tmp/project"),
        )
        .unwrap();

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
        assert!(config.show_comparison);
        assert_eq!(config.scope, Scope::All);
        assert_eq!(config.color_theme, ColorTheme::Ocean);
        assert_eq!(config.theme_scope, ThemeScope::All);
    }

    #[test]
    fn file_config_supports_theme_and_week_start() {
        let file_config = FileConfig {
            week_start: Some("sunday".to_string()),
            auto_refresh: Some(false),
            show_comparison: Some(true),
            color_theme: Some("forest".to_string()),
            theme_scope: Some("all".to_string()),
            ..FileConfig::default()
        };

        let config = resolve_config(
            cli(),
            file_config,
            PathBuf::from("/tmp/opencode.db"),
            PathBuf::from("/tmp/expensive.sqlite3"),
            None,
            PathBuf::from("/tmp/project"),
        )
        .unwrap();

        assert_eq!(config.week_start, WeekStart::Sunday);
        assert!(!config.auto_refresh);
        assert!(config.show_comparison);
        assert_eq!(config.color_theme, ColorTheme::Forest);
        assert_eq!(config.theme_scope, ThemeScope::All);
    }

    #[test]
    fn saves_editable_config_values() {
        let tempdir = tempfile::tempdir().unwrap();
        let path = tempdir.path().join("expensive").join("config.toml");
        let config = Config {
            db_path: PathBuf::from("/tmp/opencode.db"),
            index_path: PathBuf::from("/tmp/expensive.sqlite3"),
            codex_home: PathBuf::from("/tmp/codex"),
            pi_sessions_root: PathBuf::from("/tmp/pi/sessions"),
            current_directory: PathBuf::from("/tmp/project"),
            config_path: Some(path.clone()),
            daily_start: "05:30".parse().unwrap(),
            week_start: WeekStart::Sunday,
            refresh_interval: Duration::from_secs(15),
            auto_refresh: false,
            show_comparison: true,
            scope: Scope::All,
            color_theme: ColorTheme::Forest,
            theme_scope: ThemeScope::All,
            aliases: ModelAliases {
                providers: BTreeMap::from([("github-copilot".to_string(), "gc".to_string())]),
                models: BTreeMap::from([("github-copilot/gpt-test".to_string(), "gt".to_string())]),
            },
        };

        save(&config).unwrap();

        let content = fs::read_to_string(path).unwrap();
        assert!(content.contains(r#"daily_start = "05:30""#));
        assert!(content.contains(r#"week_start = "sunday""#));
        assert!(content.contains("refresh_seconds = 15"));
        assert!(content.contains("auto_refresh = false"));
        assert!(content.contains("show_comparison = true"));
        assert!(content.contains(r#"color_theme = "forest""#));
        assert!(content.contains(r#"theme_scope = "all""#));
        assert!(content.contains(r#"scope = "all""#));
        let parsed: FileConfig = toml::from_str(&content).unwrap();
        assert_eq!(parsed.provider_aliases["github-copilot"], "gc");
        assert_eq!(parsed.model_aliases["github-copilot/gpt-test"], "gt");
    }

    #[test]
    fn model_aliases_prefer_full_model_then_provider_prefix() {
        let aliases = ModelAliases {
            providers: BTreeMap::from([("github-copilot".to_string(), "gc".to_string())]),
            models: BTreeMap::from([(
                "github-copilot/gpt-special".to_string(),
                "special".to_string(),
            )]),
        };

        assert_eq!(
            aliases.display_name("github-copilot", "gpt-test", "default"),
            "gc/gpt-test"
        );
        assert_eq!(
            aliases.display_name("github-copilot", "gpt-special", "high"),
            "special (high)"
        );
    }

    #[test]
    fn rejects_zero_refresh_interval() {
        let mut cli = cli();
        cli.refresh = Some(0);

        let error = resolve_config(
            cli,
            FileConfig::default(),
            PathBuf::from("/tmp/opencode.db"),
            PathBuf::from("/tmp/expensive.sqlite3"),
            None,
            PathBuf::from("/tmp/project"),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("refresh interval"));
    }

    #[test]
    fn supports_current_and_project_scopes() {
        let file_config = FileConfig {
            scope: Some("current".to_string()),
            ..FileConfig::default()
        };

        let config = resolve_config(
            cli(),
            file_config,
            PathBuf::from("/tmp/opencode.db"),
            PathBuf::from("/tmp/expensive.sqlite3"),
            None,
            PathBuf::from("/tmp/project"),
        )
        .unwrap();
        assert_eq!(config.scope, Scope::Current);

        assert_eq!(
            "project:abc".parse::<Scope>().unwrap(),
            Scope::Project("abc".to_string())
        );
    }

    #[test]
    fn rejects_unknown_theme_values() {
        let file_config = FileConfig {
            color_theme: Some("sparkles".to_string()),
            ..FileConfig::default()
        };

        let error = resolve_config(
            cli(),
            file_config,
            PathBuf::from("/tmp/opencode.db"),
            PathBuf::from("/tmp/expensive.sqlite3"),
            None,
            PathBuf::from("/tmp/project"),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("unsupported color theme"));
    }
}
