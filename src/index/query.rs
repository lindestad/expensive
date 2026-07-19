use std::{collections::HashMap, path::Path};

use anyhow::{anyhow, Context, Result};
use chrono::Local;
use rusqlite::{params, Connection, OptionalExtension};

use crate::{
    config::Scope,
    db::{ModelUsage, PeriodCost, ProjectInfo, TokenBucket, UsageStats, UsageTotals},
    time_window::{Mode, PeriodKey},
};

use super::UsageIndex;

pub fn load_usage_range_scoped(
    path: &Path,
    mode: Mode,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
    include_token_buckets: bool,
    scope: &Scope,
    current_directory: &Path,
) -> Result<UsageStats> {
    let refreshed_at = Local::now();
    let snapshot_millis = refreshed_at.timestamp_millis().saturating_add(1);
    let query_end_millis = effective_end(end_millis, snapshot_millis);
    let index = UsageIndex::open(path)?;
    let project_id = resolve_scope(&index.connection, scope, current_directory)?;
    let (totals, models) = load_model_usage(
        &index.connection,
        start_millis,
        query_end_millis,
        project_id.as_deref(),
    )?;
    let token_buckets = if include_token_buckets && totals.messages > 0 {
        load_token_buckets(
            &index.connection,
            mode,
            start_millis,
            end_millis,
            query_end_millis,
            project_id.as_deref(),
        )?
    } else {
        Vec::new()
    };

    Ok(UsageStats {
        mode,
        refreshed_at,
        snapshot_millis,
        cutoff_millis: start_millis,
        end_millis,
        totals,
        models,
        token_buckets,
        comparison: None,
        projected_cost: None,
    })
}

pub fn load_usage_token_buckets_at_scoped(
    path: &Path,
    mode: Mode,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
    snapshot_millis: i64,
    scope: &Scope,
    current_directory: &Path,
) -> Result<Vec<TokenBucket>> {
    let index = UsageIndex::open(path)?;
    let project_id = resolve_scope(&index.connection, scope, current_directory)?;
    load_token_buckets(
        &index.connection,
        mode,
        start_millis,
        end_millis,
        effective_end(end_millis, snapshot_millis),
        project_id.as_deref(),
    )
}

pub fn load_period_costs_scoped(
    path: &Path,
    periods: &[PeriodKey],
    scope: &Scope,
    current_directory: &Path,
) -> Result<Vec<PeriodCost>> {
    if periods.is_empty() {
        return Ok(Vec::new());
    }
    let start_millis = periods
        .iter()
        .map(|period| period.start_millis)
        .min()
        .expect("periods is not empty");
    let end_millis = periods
        .iter()
        .map(|period| period.end_millis)
        .max()
        .expect("periods is not empty");
    let index = UsageIndex::open(path)?;
    let project_id = resolve_scope(&index.connection, scope, current_directory)?;
    let mut statement = index.connection.prepare(
        "SELECT occurred_at_ms, COALESCE(cost_microusd, 0)
         FROM usage_events
         WHERE occurred_at_ms >= ?1
           AND occurred_at_ms < ?2
           AND (?3 IS NULL OR project_id = ?3)",
    )?;
    let rows = statement.query_map(params![start_millis, end_millis, project_id], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut costs = periods
        .iter()
        .copied()
        .map(|period| (period, 0_i64))
        .collect::<HashMap<_, _>>();
    for row in rows {
        let (occurred_at_ms, cost_microusd) = row?;
        if let Some(period) = periods
            .iter()
            .copied()
            .find(|period| period.contains(occurred_at_ms))
        {
            *costs.entry(period).or_default() += cost_microusd;
        }
    }
    Ok(periods
        .iter()
        .copied()
        .map(|period| PeriodCost {
            period,
            cost: micros_to_usd(costs.remove(&period).unwrap_or(0)),
        })
        .collect())
}

pub fn list_projects(path: &Path) -> Result<Vec<ProjectInfo>> {
    let index = UsageIndex::open(path)?;
    query_projects(&index.connection)
}

fn load_model_usage(
    connection: &Connection,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
    project_id: Option<&str>,
) -> Result<(UsageTotals, Vec<ModelUsage>)> {
    let mut statement = connection.prepare(
        "SELECT provider, model, variant,
                COALESCE(SUM(messages), 0) AS messages,
                COALESCE(SUM(cost_microusd), 0) AS cost_microusd,
                COALESCE(SUM(total_tokens), 0) AS total_tokens,
                COALESCE(SUM(input_tokens), 0) AS input_tokens,
                COALESCE(SUM(output_tokens), 0) AS output_tokens,
                COALESCE(SUM(cache_read_tokens), 0) AS cache_read_tokens,
                COALESCE(SUM(cache_write_tokens), 0) AS cache_write_tokens
         FROM usage_events
         WHERE (?1 IS NULL OR occurred_at_ms >= ?1)
           AND (?2 IS NULL OR occurred_at_ms < ?2)
           AND (?3 IS NULL OR project_id = ?3)
         GROUP BY provider, model, variant
         ORDER BY cost_microusd DESC, total_tokens DESC",
    )?;
    let rows = statement.query_map(params![start_millis, end_millis, project_id], |row| {
        let provider: String = row.get("provider")?;
        let model_id: String = row.get("model")?;
        let variant: String = row.get("variant")?;
        Ok(ModelUsage {
            display_name: display_name(&provider, &model_id, &variant),
            provider,
            model_id,
            variant,
            totals: UsageTotals {
                messages: nonnegative(row.get("messages")?),
                cost: micros_to_usd(row.get("cost_microusd")?),
                total: nonnegative(row.get("total_tokens")?),
                input: nonnegative(row.get("input_tokens")?),
                output: nonnegative(row.get("output_tokens")?),
                cache_read: nonnegative(row.get("cache_read_tokens")?),
                cache_write: nonnegative(row.get("cache_write_tokens")?),
            },
        })
    })?;
    let mut totals = UsageTotals::default();
    let mut models = Vec::new();
    for row in rows {
        let model = row?;
        totals.add_model(&model);
        models.push(model);
    }
    Ok((totals, models))
}

fn load_token_buckets(
    connection: &Connection,
    mode: Mode,
    start_millis: Option<i64>,
    logical_end_millis: Option<i64>,
    query_end_millis: Option<i64>,
    project_id: Option<&str>,
) -> Result<Vec<TokenBucket>> {
    let Some((range_start, last_time)) = token_bucket_time_range(
        connection,
        mode,
        start_millis,
        logical_end_millis,
        query_end_millis,
        project_id,
    )?
    else {
        return Ok(Vec::new());
    };
    let span = token_bucket_span_millis(mode, range_start, last_time.max(range_start));
    let range_end = token_bucket_range_end(mode, range_start, last_time, logical_end_millis, span);
    if span <= 0 || range_end <= range_start {
        return Ok(Vec::new());
    }
    let bucket_count = ((range_end - range_start + span - 1) / span) as usize;
    let mut buckets = (0..bucket_count)
        .map(|idx| {
            let start = range_start + idx as i64 * span;
            TokenBucket {
                start_millis: start,
                end_millis: (start + span).min(range_end),
                tokens: 0,
                cost: 0.0,
            }
        })
        .collect::<Vec<_>>();
    let mut statement = connection.prepare(
        "SELECT (occurred_at_ms - ?4) / ?5 AS bucket_idx,
                COALESCE(SUM(total_tokens), 0) AS tokens,
                COALESCE(SUM(cost_microusd), 0) AS cost_microusd
         FROM usage_events
         WHERE (?1 IS NULL OR occurred_at_ms >= ?1)
           AND (?2 IS NULL OR occurred_at_ms < ?2)
           AND (?3 IS NULL OR project_id = ?3)
           AND occurred_at_ms >= ?4
           AND occurred_at_ms < ?6
         GROUP BY bucket_idx
         ORDER BY bucket_idx",
    )?;
    let rows = statement.query_map(
        params![
            start_millis,
            query_end_millis,
            project_id,
            range_start,
            span,
            range_end
        ],
        |row| {
            Ok((
                row.get::<_, i64>("bucket_idx")?,
                row.get::<_, i64>("tokens")?,
                row.get::<_, i64>("cost_microusd")?,
            ))
        },
    )?;
    for row in rows {
        let (bucket_idx, tokens, cost_microusd) = row?;
        if let Some(bucket) = usize::try_from(bucket_idx)
            .ok()
            .and_then(|idx| buckets.get_mut(idx))
        {
            bucket.tokens = nonnegative(tokens);
            bucket.cost = micros_to_usd(cost_microusd);
        }
    }
    Ok(buckets)
}

fn token_bucket_time_range(
    connection: &Connection,
    mode: Mode,
    start_millis: Option<i64>,
    logical_end_millis: Option<i64>,
    query_end_millis: Option<i64>,
    project_id: Option<&str>,
) -> Result<Option<(i64, i64)>> {
    if matches!(mode, Mode::Daily | Mode::Weekly | Mode::Monthly) {
        if let Some(start_millis) = start_millis {
            return Ok(Some((
                start_millis,
                logical_end_millis.unwrap_or(start_millis),
            )));
        }
    }
    connection
        .query_row(
            "SELECT MIN(occurred_at_ms), MAX(occurred_at_ms)
             FROM usage_events
             WHERE (?1 IS NULL OR occurred_at_ms >= ?1)
               AND (?2 IS NULL OR occurred_at_ms < ?2)
               AND (?3 IS NULL OR project_id = ?3)",
            params![start_millis, query_end_millis, project_id],
            |row| {
                let start: Option<i64> = row.get(0)?;
                let end: Option<i64> = row.get(1)?;
                Ok(start.zip(end))
            },
        )
        .context("reading indexed usage bounds")
}

fn resolve_scope(
    connection: &Connection,
    scope: &Scope,
    current_directory: &Path,
) -> Result<Option<String>> {
    match scope {
        Scope::All => Ok(None),
        Scope::Project(id) => connection
            .query_row(
                "SELECT project_id FROM (
                    SELECT id AS project_id, 0 AS priority FROM projects WHERE id = ?1
                    UNION ALL
                    SELECT project_id, 1 AS priority FROM source_projects WHERE native_id = ?1
                 ) ORDER BY priority LIMIT 1",
                [id],
                |row| row.get(0),
            )
            .optional()?
            .map(Some)
            .ok_or_else(|| anyhow!("configured project {id:?} no longer exists")),
        Scope::Current => {
            find_project_for_directory(&query_projects(connection)?, current_directory)
                .map(|project| Some(project.id.clone()))
                .ok_or_else(|| {
                    anyhow!(
                        "no indexed project matches current directory {}",
                        current_directory.display()
                    )
                })
        }
    }
}

fn query_projects(connection: &Connection) -> Result<Vec<ProjectInfo>> {
    let mut statement = connection.prepare(
        "SELECT id, COALESCE(NULLIF(name, ''), id), worktree
         FROM projects ORDER BY name COLLATE NOCASE, worktree COLLATE NOCASE",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ProjectInfo {
            id: row.get(0)?,
            name: row.get(1)?,
            worktree: row.get(2)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("listing indexed projects")
}

fn find_project_for_directory<'a>(
    projects: &'a [ProjectInfo],
    directory: &Path,
) -> Option<&'a ProjectInfo> {
    let directory = comparable_path(directory);
    projects
        .iter()
        .filter_map(|project| {
            let worktree = comparable_path(Path::new(&project.worktree));
            directory
                .starts_with(&worktree)
                .then_some((worktree.components().count(), project))
        })
        .max_by_key(|(depth, _)| *depth)
        .map(|(_, project)| project)
}

fn comparable_path(path: &Path) -> std::path::PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn token_bucket_span_millis(mode: Mode, start_millis: i64, end_millis: i64) -> i64 {
    const HOUR: i64 = 60 * 60 * 1_000;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    match mode {
        Mode::Daily => HOUR,
        Mode::Weekly | Mode::Monthly => DAY,
        Mode::AllTime => {
            let span = end_millis.saturating_sub(start_millis);
            if span <= 60 * DAY {
                DAY
            } else if span <= 52 * WEEK {
                WEEK
            } else {
                30 * DAY
            }
        }
    }
}

fn token_bucket_range_end(
    mode: Mode,
    range_start: i64,
    last_event_millis: i64,
    end_millis: Option<i64>,
    bucket_span_millis: i64,
) -> i64 {
    const HOUR: i64 = 60 * 60 * 1_000;
    const DAY: i64 = 24 * HOUR;
    if let Some(end_millis) = end_millis {
        return end_millis;
    }
    let nominal_end = match mode {
        Mode::Daily => Some(range_start + DAY),
        Mode::Weekly => Some(range_start + 7 * DAY),
        Mode::Monthly => Some(range_start + 31 * DAY),
        Mode::AllTime => None,
    };
    let buckets_to_last_event = last_event_millis
        .saturating_sub(range_start)
        .checked_div(bucket_span_millis)
        .unwrap_or(0)
        .saturating_add(1);
    let last_bucket_end =
        range_start.saturating_add(buckets_to_last_event.saturating_mul(bucket_span_millis));
    nominal_end
        .map(|end| end.max(last_bucket_end))
        .unwrap_or(last_bucket_end)
}

fn effective_end(end_millis: Option<i64>, snapshot_millis: i64) -> Option<i64> {
    Some(
        end_millis
            .map(|end| end.min(snapshot_millis))
            .unwrap_or(snapshot_millis),
    )
}

fn nonnegative(value: i64) -> u64 {
    value.max(0) as u64
}

fn micros_to_usd(value: i64) -> f64 {
    value as f64 / 1_000_000.0
}

fn display_name(provider: &str, model: &str, variant: &str) -> String {
    let base = if provider.trim().is_empty() || provider == "unknown" {
        model.to_string()
    } else {
        format!("{provider}/{model}")
    };
    if variant.trim().is_empty() || variant == "default" {
        base
    } else {
        format!("{base} ({variant})")
    }
}

#[cfg(test)]
mod tests {
    use crate::index::{ArtifactRecord, CostKind, SourceKind, SourceRegistration, UsageEvent};

    use super::*;

    #[test]
    fn queries_authoritative_totals_and_source_neutral_projects() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite3");
        let mut index = UsageIndex::open(&path).unwrap();
        let source_id = index
            .register_source(&SourceRegistration {
                kind: SourceKind::Codex,
                source_key: "default".to_string(),
                display_name: "Codex".to_string(),
            })
            .unwrap();
        let project = super::super::ProjectRecord {
            id: "project".to_string(),
            name: "Project".to_string(),
            worktree: "/work/project".to_string(),
        };
        index.upsert_project(source_id, "native", &project).unwrap();
        index
            .replace_artifact_events(
                source_id,
                &ArtifactRecord {
                    key: "rollout".to_string(),
                    path: None,
                    device: None,
                    inode: None,
                    size: None,
                    modified_ns: None,
                    parsed_offset: 0,
                    boundary_hash: None,
                    full_hash: None,
                    cursor: None,
                    parser_version: 1,
                    scanned_at_ms: 1,
                },
                &[UsageEvent {
                    event_key: vec![1],
                    occurred_at_ms: 1_500,
                    project_id: Some(project.id.clone()),
                    provider: "openai".to_string(),
                    model: "gpt-test".to_string(),
                    variant: "high".to_string(),
                    messages: 1,
                    input_tokens: 6,
                    output_tokens: 2,
                    cache_read_tokens: 4,
                    cache_write_tokens: 0,
                    reasoning_tokens: 1,
                    total_tokens: 12,
                    cost_microusd: None,
                    cost_kind: CostKind::Unavailable,
                }],
            )
            .unwrap();

        let stats = load_usage_range_scoped(
            &path,
            Mode::Daily,
            Some(1_000),
            Some(2_000),
            true,
            &Scope::Project("native".to_string()),
            Path::new("/elsewhere"),
        )
        .unwrap();

        assert_eq!(stats.totals.total_tokens(), 12);
        assert_eq!(stats.totals.input, 6);
        assert_eq!(stats.totals.cache_read, 4);
        assert_eq!(stats.token_buckets[0].tokens, 12);
        assert_eq!(list_projects(&path).unwrap()[0].id, "project");
    }
}
