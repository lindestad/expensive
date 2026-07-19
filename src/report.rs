//! Headless JSON reporting.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone};
use serde::Serialize;

use crate::{
    analytics,
    config::{Config, ReportArgs},
    db::{ModelUsage, TokenBucket, UsageComparison, UsageStats, UsageTotals},
    time_window::{self, CalendarScale, Mode},
};

#[derive(Serialize)]
struct JsonReport {
    schema_version: u8,
    expensive_version: &'static str,
    generated_at: String,
    database: String,
    scope: String,
    current_directory: String,
    window: ReportWindow,
    totals: UsageTotals,
    comparison: Option<UsageComparison>,
    projected_cost: Option<f64>,
    biggest_cost_increase: Option<ModelIncrease>,
    models: Vec<ModelUsage>,
    buckets: Vec<TokenBucket>,
}

#[derive(Serialize)]
struct ReportWindow {
    mode: &'static str,
    start_millis: Option<i64>,
    start: Option<String>,
    end_millis: Option<i64>,
    end: Option<String>,
    observed_through_millis: i64,
}

#[derive(Serialize)]
struct ModelIncrease {
    model: String,
    cost_delta: f64,
}

pub fn run(config: &Config, args: &ReportArgs) -> Result<()> {
    let report = build_report(config, args)?;
    let output = if args.pretty {
        serde_json::to_string_pretty(&report)
    } else {
        serde_json::to_string(&report)
    }
    .context("serializing usage report")?;
    println!("{output}");
    Ok(())
}

fn build_report(config: &Config, args: &ReportArgs) -> Result<JsonReport> {
    let mode = args.period.mode();
    let requested_start = args.from.as_deref().map(parse_bound).transpose()?;
    let requested_end = args.to.as_deref().map(parse_bound).transpose()?;
    if let (Some(start), Some(end)) = (requested_start, requested_end) {
        if start >= end {
            bail!("--from must be earlier than --to");
        }
    }

    let stats = if requested_start.is_some() || requested_end.is_some() {
        analytics::load_range(config, mode, requested_start, requested_end, true)?
    } else if mode == Mode::AllTime {
        analytics::load_range(config, mode, None, None, true)?
    } else {
        let scale = CalendarScale::from_mode(mode).expect("finite report periods have a scale");
        let period = time_window::current_period(
            scale,
            Local::now(),
            config.daily_start,
            config.week_start,
        )?;
        analytics::load_period_with_buckets(config, period, true)?
    };

    Ok(json_report(config, stats))
}

fn json_report(config: &Config, stats: UsageStats) -> JsonReport {
    let biggest_cost_increase =
        stats
            .biggest_cost_increase()
            .map(|(model, cost_delta)| ModelIncrease {
                model: model.to_string(),
                cost_delta,
            });
    JsonReport {
        schema_version: 1,
        expensive_version: env!("CARGO_PKG_VERSION"),
        generated_at: stats.refreshed_at.to_rfc3339(),
        database: config.db_path.display().to_string(),
        scope: config.scope.key(),
        current_directory: config.current_directory.display().to_string(),
        window: ReportWindow {
            mode: mode_key(stats.mode),
            start_millis: stats.cutoff_millis,
            start: stats.cutoff_millis.and_then(format_millis),
            end_millis: stats.end_millis,
            end: stats.end_millis.and_then(format_millis),
            observed_through_millis: stats.snapshot_millis,
        },
        totals: stats.totals,
        comparison: stats.comparison,
        projected_cost: stats.projected_cost,
        biggest_cost_increase,
        models: stats.models,
        buckets: stats.token_buckets,
    }
}

fn parse_bound(value: &str) -> Result<i64> {
    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return Ok(value.timestamp_millis());
    }
    if let Ok(value) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
        return resolve_local(value);
    }
    if let Ok(value) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M") {
        return resolve_local(value);
    }
    if let Ok(value) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return resolve_local(
            value
                .and_hms_opt(0, 0, 0)
                .expect("midnight is a valid naive time"),
        );
    }
    Err(anyhow!(
        "invalid date bound {value:?}; expected YYYY-MM-DD, local YYYY-MM-DDTHH:MM[:SS], or RFC 3339"
    ))
}

fn resolve_local(value: NaiveDateTime) -> Result<i64> {
    match Local.from_local_datetime(&value) {
        LocalResult::Single(value) => Ok(value.timestamp_millis()),
        LocalResult::Ambiguous(first, second) => Ok(first.min(second).timestamp_millis()),
        LocalResult::None => Err(anyhow!("local time {value} does not exist")),
    }
}

fn format_millis(millis: i64) -> Option<String> {
    DateTime::from_timestamp_millis(millis).map(|value| value.with_timezone(&Local).to_rfc3339())
}

fn mode_key(mode: Mode) -> &'static str {
    match mode {
        Mode::Daily => "daily",
        Mode::Weekly => "weekly",
        Mode::Monthly => "monthly",
        Mode::AllTime => "all",
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf, time::Duration};

    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::config::{ColorTheme, ModelAliases, ReportPeriod, Scope, ThemeScope};
    use crate::time_window::{DailyStart, WeekStart};

    #[test]
    fn parses_supported_date_bounds() {
        assert!(parse_bound("2026-07-19").is_ok());
        assert!(parse_bound("2026-07-19T12:30").is_ok());
        assert!(parse_bound("2026-07-19T12:30:45+02:00").is_ok());
        assert!(parse_bound("July 19").is_err());
    }

    #[test]
    fn builds_scoped_alias_aware_json_report() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(include_str!("../tests/fixtures/opencode.sql"))
            .unwrap();
        drop(connection);
        let config = Config {
            db_path: file.path().to_path_buf(),
            current_directory: PathBuf::from("/work/project-a"),
            config_path: None,
            daily_start: DailyStart::default(),
            week_start: WeekStart::default(),
            refresh_interval: Duration::from_secs(60),
            auto_refresh: false,
            scope: Scope::Project("project-a".to_string()),
            color_theme: ColorTheme::default(),
            theme_scope: ThemeScope::default(),
            aliases: ModelAliases {
                providers: BTreeMap::from([("github-copilot".to_string(), "gc".to_string())]),
                models: BTreeMap::new(),
            },
        };
        let args = ReportArgs {
            period: ReportPeriod::All,
            from: None,
            to: None,
            pretty: false,
        };

        let report = build_report(&config, &args).unwrap();
        let value = serde_json::to_value(report).unwrap();

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["scope"], "project:project-a");
        assert_eq!(value["totals"]["messages"], 1);
        assert_eq!(value["models"][0]["display_name"], "gc/gpt-test");
        assert_eq!(value["buckets"][0]["cost"], 1.25);
    }
}
