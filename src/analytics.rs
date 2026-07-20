//! Shared usage-analysis pipeline for interactive and headless views.

use anyhow::Result;
use chrono::{DateTime, Local};

use crate::{
    config::Config,
    db::{UsageComparison, UsageStats},
    index,
    time_window::{self, CalendarScale, Mode, PeriodKey},
};

pub fn load_dashboard(config: &Config, mode: Mode) -> Result<UsageStats> {
    let cutoff_millis =
        time_window::cutoff_millis(mode, Local::now(), config.daily_start, config.week_start)?;
    let mut stats = index::load_usage_range_scoped_with_options(
        &config.index_path,
        mode,
        cutoff_millis,
        None,
        false,
        query_options(config),
    )?;
    apply_model_aliases(config, &mut stats);
    if let Some(scale) = CalendarScale::from_mode(mode) {
        let now = local_from_millis(stats.snapshot_millis.saturating_sub(1))?;
        let period =
            time_window::current_period(scale, now, config.daily_start, config.week_start)?;
        attach_period_comparison(config, &mut stats, period)?;
    }
    Ok(stats)
}

pub fn load_period(config: &Config, period: PeriodKey) -> Result<UsageStats> {
    load_period_with_buckets(config, period, false)
}

pub fn load_period_with_buckets(
    config: &Config,
    period: PeriodKey,
    include_buckets: bool,
) -> Result<UsageStats> {
    let mut stats = index::load_usage_range_scoped_with_options(
        &config.index_path,
        period.mode(),
        Some(period.start_millis),
        Some(period.end_millis),
        include_buckets,
        query_options(config),
    )?;
    apply_model_aliases(config, &mut stats);
    attach_period_comparison(config, &mut stats, period)?;
    Ok(stats)
}

pub fn load_range(
    config: &Config,
    mode: Mode,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
    include_buckets: bool,
) -> Result<UsageStats> {
    let mut stats = index::load_usage_range_scoped_with_options(
        &config.index_path,
        mode,
        start_millis,
        end_millis,
        include_buckets,
        query_options(config),
    )?;
    apply_model_aliases(config, &mut stats);
    if let (Some(start_millis), Some(end_millis)) = (start_millis, end_millis) {
        attach_equal_range_comparison(config, &mut stats, start_millis, end_millis)?;
        stats.projected_cost = projected_cost_for_range(&stats, start_millis, end_millis);
    }
    Ok(stats)
}

pub fn apply_model_aliases(config: &Config, stats: &mut UsageStats) {
    for model in &mut stats.models {
        model.display_name =
            config
                .aliases
                .display_name(&model.provider, &model.model_id, &model.variant);
    }
    if let Some(comparison) = &mut stats.comparison {
        for model in &mut comparison.models {
            model.display_name =
                config
                    .aliases
                    .display_name(&model.provider, &model.model_id, &model.variant);
        }
    }
}

fn attach_period_comparison(
    config: &Config,
    stats: &mut UsageStats,
    period: PeriodKey,
) -> Result<()> {
    let previous_period = time_window::shift_period(period, -1)?;
    attach_comparison(
        config,
        stats,
        previous_period.start_millis,
        previous_period.end_millis,
    )?;
    stats.projected_cost = projected_cost_for_range(stats, period.start_millis, period.end_millis);
    Ok(())
}

fn attach_equal_range_comparison(
    config: &Config,
    stats: &mut UsageStats,
    start_millis: i64,
    end_millis: i64,
) -> Result<()> {
    let duration = end_millis.saturating_sub(start_millis);
    attach_comparison(
        config,
        stats,
        start_millis.saturating_sub(duration),
        start_millis,
    )
}

fn attach_comparison(
    config: &Config,
    stats: &mut UsageStats,
    start_millis: i64,
    end_millis: i64,
) -> Result<()> {
    let mut previous = index::load_usage_range_scoped_with_options(
        &config.index_path,
        stats.mode,
        Some(start_millis),
        Some(end_millis),
        false,
        query_options(config),
    )?;
    apply_model_aliases(config, &mut previous);
    stats.comparison = Some(UsageComparison {
        start_millis,
        end_millis,
        totals: previous.totals,
        models: previous.models,
    });
    Ok(())
}

fn query_options(config: &Config) -> index::UsageQueryOptions<'_> {
    index::UsageQueryOptions {
        scope: &config.scope,
        current_directory: &config.current_directory,
        hidden_providers: &config.hidden_providers,
        estimate_api_cost: config.estimate_api_cost,
    }
}

fn projected_cost_for_range(stats: &UsageStats, start_millis: i64, end_millis: i64) -> Option<f64> {
    let observed = stats.snapshot_millis.clamp(start_millis, end_millis);
    if observed <= start_millis || observed >= end_millis {
        return None;
    }
    let elapsed = observed.saturating_sub(start_millis) as f64;
    let duration = end_millis.saturating_sub(start_millis) as f64;
    (elapsed > 0.0).then_some(stats.totals.cost * duration / elapsed)
}

fn local_from_millis(millis: i64) -> Result<DateTime<Local>> {
    DateTime::from_timestamp_millis(millis)
        .map(|value| value.with_timezone(&Local))
        .ok_or_else(|| anyhow::anyhow!("timestamp is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{UsageStats, UsageTotals};

    #[test]
    fn projects_active_range_cost_at_current_run_rate() {
        let stats = UsageStats {
            mode: Mode::Daily,
            refreshed_at: Local::now(),
            snapshot_millis: 5_000,
            cutoff_millis: Some(1_000),
            end_millis: Some(9_000),
            totals: UsageTotals {
                cost: 2.0,
                ..UsageTotals::default()
            },
            models: Vec::new(),
            token_buckets: Vec::new(),
            comparison: None,
            projected_cost: None,
        };

        assert_eq!(projected_cost_for_range(&stats, 1_000, 9_000), Some(4.0));
        assert_eq!(projected_cost_for_range(&stats, 1_000, 5_000), None);
    }
}
