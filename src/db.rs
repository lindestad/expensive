//! Aggregation over OpenCode's local SQLite database.
//!
//! OpenCode stores usage details in JSON inside assistant message rows. This
//! module reads those rows directly and produces the same cost/token categories
//! the TUI displays, grouped by provider, model, and variant.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use rusqlite::{params, Connection, OpenFlags};

use crate::{
    config::Scope,
    time_window::{Mode, PeriodKey},
};

#[derive(Clone, Debug, Default)]
pub struct UsageTotals {
    pub messages: u64,
    pub cost: f64,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

impl UsageTotals {
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }

    fn add_model(&mut self, model: &ModelUsage) {
        self.messages += model.totals.messages;
        self.cost += model.totals.cost;
        self.input += model.totals.input;
        self.output += model.totals.output;
        self.cache_read += model.totals.cache_read;
        self.cache_write += model.totals.cache_write;
    }
}

#[derive(Clone, Debug)]
pub struct ModelUsage {
    pub provider: String,
    pub model_id: String,
    pub variant: String,
    pub display_name: String,
    pub totals: UsageTotals,
}

#[derive(Clone, Debug)]
pub struct UsageStats {
    pub mode: Mode,
    pub refreshed_at: DateTime<Local>,
    /// Exclusive upper bound shared by summary and deferred graph queries.
    pub snapshot_millis: i64,
    pub cutoff_millis: Option<i64>,
    pub end_millis: Option<i64>,
    pub totals: UsageTotals,
    pub models: Vec<ModelUsage>,
    pub token_buckets: Vec<TokenBucket>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TokenBucket {
    pub start_millis: i64,
    pub end_millis: i64,
    pub tokens: u64,
}

#[derive(Clone, Debug)]
pub struct PeriodCost {
    pub period: PeriodKey,
    pub cost: f64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub worktree: String,
}

#[derive(Clone, Copy)]
struct ScopeContext<'a> {
    scope: &'a Scope,
    current_directory: &'a Path,
}

#[derive(Clone, Debug)]
pub struct DatabaseDiagnostics {
    pub path: String,
    pub sqlite_version: String,
    pub json_functions: bool,
    pub assistant_messages: Option<u64>,
    pub project_scope: bool,
    pub opencode_versions: Vec<String>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

impl DatabaseDiagnostics {
    pub fn is_compatible(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn load_usage(path: &Path, mode: Mode, cutoff_millis: Option<i64>) -> Result<UsageStats> {
    load_usage_range(path, mode, cutoff_millis, None, true, None)
}

pub fn load_usage_summary(
    path: &Path,
    mode: Mode,
    cutoff_millis: Option<i64>,
) -> Result<UsageStats> {
    load_usage_range(path, mode, cutoff_millis, None, false, None)
}

pub fn load_usage_summary_scoped(
    path: &Path,
    mode: Mode,
    cutoff_millis: Option<i64>,
    scope: &Scope,
    current_directory: &Path,
) -> Result<UsageStats> {
    load_usage_range(
        path,
        mode,
        cutoff_millis,
        None,
        false,
        Some(ScopeContext {
            scope,
            current_directory,
        }),
    )
}

pub fn load_usage_between(
    path: &Path,
    mode: Mode,
    start_millis: i64,
    end_millis: i64,
) -> Result<UsageStats> {
    load_usage_range(path, mode, Some(start_millis), Some(end_millis), true, None)
}

pub fn load_usage_summary_between(
    path: &Path,
    mode: Mode,
    start_millis: i64,
    end_millis: i64,
) -> Result<UsageStats> {
    load_usage_range(
        path,
        mode,
        Some(start_millis),
        Some(end_millis),
        false,
        None,
    )
}

pub fn load_usage_summary_between_scoped(
    path: &Path,
    mode: Mode,
    start_millis: i64,
    end_millis: i64,
    scope: &Scope,
    current_directory: &Path,
) -> Result<UsageStats> {
    load_usage_range(
        path,
        mode,
        Some(start_millis),
        Some(end_millis),
        false,
        Some(ScopeContext {
            scope,
            current_directory,
        }),
    )
}

pub fn load_usage_token_buckets(
    path: &Path,
    mode: Mode,
    cutoff_millis: Option<i64>,
) -> Result<Vec<TokenBucket>> {
    load_token_buckets_range(path, mode, cutoff_millis, None, snapshot_now(), None)
}

pub fn load_usage_token_buckets_between(
    path: &Path,
    mode: Mode,
    start_millis: i64,
    end_millis: i64,
) -> Result<Vec<TokenBucket>> {
    load_token_buckets_range(
        path,
        mode,
        Some(start_millis),
        Some(end_millis),
        snapshot_now(),
        None,
    )
}

pub fn load_usage_token_buckets_at(
    path: &Path,
    mode: Mode,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
    snapshot_millis: i64,
) -> Result<Vec<TokenBucket>> {
    load_token_buckets_range(path, mode, start_millis, end_millis, snapshot_millis, None)
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
    load_token_buckets_range(
        path,
        mode,
        start_millis,
        end_millis,
        snapshot_millis,
        Some(ScopeContext {
            scope,
            current_directory,
        }),
    )
}

fn load_usage_range(
    path: &Path,
    mode: Mode,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
    include_token_buckets: bool,
    scope: Option<ScopeContext<'_>>,
) -> Result<UsageStats> {
    let refreshed_at = Local::now();
    let snapshot_millis = refreshed_at.timestamp_millis().saturating_add(1);
    let query_end_millis = effective_end(end_millis, snapshot_millis);
    let connection = open_database(path)?;
    let project_id = resolve_scope(&connection, scope)?;

    let (totals, models) = load_model_usage(
        &connection,
        start_millis,
        query_end_millis,
        project_id.as_deref(),
    )?;
    let token_buckets = if include_token_buckets && totals.messages > 0 {
        load_token_buckets(
            &connection,
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
    })
}

fn load_token_buckets_range(
    path: &Path,
    mode: Mode,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
    snapshot_millis: i64,
    scope: Option<ScopeContext<'_>>,
) -> Result<Vec<TokenBucket>> {
    let connection = open_database(path)?;
    let query_end_millis = effective_end(end_millis, snapshot_millis);
    let project_id = resolve_scope(&connection, scope)?;

    load_token_buckets(
        &connection,
        mode,
        start_millis,
        end_millis,
        query_end_millis,
        project_id.as_deref(),
    )
}

fn load_model_usage(
    connection: &Connection,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
    project_id: Option<&str>,
) -> Result<(UsageTotals, Vec<ModelUsage>)> {
    let mut statement = connection.prepare(
        r#"
        SELECT
            COALESCE(json_extract(data, '$.providerID'), 'unknown') AS provider,
            COALESCE(json_extract(data, '$.modelID'), 'unknown') AS model_id,
            COALESCE(json_extract(data, '$.variant'), 'default') AS variant,
            COUNT(*) AS messages,
            COALESCE(SUM(COALESCE(json_extract(data, '$.cost'), 0)), 0) AS cost,
            COALESCE(SUM(COALESCE(json_extract(data, '$.tokens.input'), 0)), 0) AS input,
            COALESCE(SUM(COALESCE(json_extract(data, '$.tokens.output'), 0)), 0) AS output,
            COALESCE(SUM(COALESCE(json_extract(data, '$.tokens.cache.read'), 0)), 0) AS cache_read,
            COALESCE(SUM(COALESCE(json_extract(data, '$.tokens.cache.write'), 0)), 0) AS cache_write
        FROM message
        WHERE json_extract(data, '$.role') = 'assistant'
            AND (?1 IS NULL OR time_created >= ?1)
            AND (?2 IS NULL OR time_created < ?2)
            AND (?3 IS NULL OR EXISTS (
                SELECT 1 FROM session
                WHERE session.id = message.session_id
                    AND session.project_id = ?3
            ))
        GROUP BY provider, model_id, variant
        ORDER BY cost DESC
        "#,
    )?;

    let rows = statement.query_map(params![start_millis, end_millis, project_id], |row| {
        let provider: String = row.get("provider")?;
        let model_id: String = row.get("model_id")?;
        let variant: String = row.get("variant")?;
        let totals = UsageTotals {
            messages: read_u64(row, "messages")?,
            cost: row.get("cost")?,
            input: read_u64(row, "input")?,
            output: read_u64(row, "output")?,
            cache_read: read_u64(row, "cache_read")?,
            cache_write: read_u64(row, "cache_write")?,
        };

        Ok(ModelUsage {
            display_name: display_name(&provider, &model_id, &variant),
            provider,
            model_id,
            variant,
            totals,
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
            }
        })
        .collect::<Vec<_>>();

    let mut statement = connection.prepare(
        r#"
        SELECT
            (time_created - ?4) / ?5 AS bucket_idx,
            COALESCE(SUM(
                COALESCE(json_extract(data, '$.tokens.input'), 0)
                + COALESCE(json_extract(data, '$.tokens.output'), 0)
                + COALESCE(json_extract(data, '$.tokens.cache.read'), 0)
                + COALESCE(json_extract(data, '$.tokens.cache.write'), 0)
            ), 0) AS tokens
        FROM message
        WHERE json_extract(data, '$.role') = 'assistant'
            AND (?1 IS NULL OR time_created >= ?1)
            AND (?2 IS NULL OR time_created < ?2)
            AND (?3 IS NULL OR EXISTS (
                SELECT 1 FROM session
                WHERE session.id = message.session_id
                    AND session.project_id = ?3
            ))
            AND time_created >= ?4
            AND time_created < ?6
        GROUP BY bucket_idx
        ORDER BY bucket_idx ASC
        "#,
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
            let bucket_idx: i64 = row.get("bucket_idx")?;
            let tokens = read_u64(row, "tokens")?;
            Ok((bucket_idx, tokens))
        },
    )?;

    for row in rows {
        let (bucket_idx, tokens) = row?;
        if bucket_idx < 0 {
            continue;
        }
        if let Some(bucket) = buckets.get_mut(bucket_idx as usize) {
            bucket.tokens = tokens;
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

    usage_time_bounds(connection, start_millis, query_end_millis, project_id)
}

fn usage_time_bounds(
    connection: &Connection,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
    project_id: Option<&str>,
) -> Result<Option<(i64, i64)>> {
    let mut statement = connection.prepare(
        r#"
        SELECT
            MIN(time_created) AS first_time,
            MAX(time_created) AS last_time
        FROM message
        WHERE json_extract(data, '$.role') = 'assistant'
            AND (?1 IS NULL OR time_created >= ?1)
            AND (?2 IS NULL OR time_created < ?2)
            AND (?3 IS NULL OR EXISTS (
                SELECT 1 FROM session
                WHERE session.id = message.session_id
                    AND session.project_id = ?3
            ))
        "#,
    )?;

    let bounds = statement.query_row(params![start_millis, end_millis, project_id], |row| {
        let first_time: Option<i64> = row.get("first_time")?;
        let last_time: Option<i64> = row.get("last_time")?;
        Ok(first_time.zip(last_time))
    })?;

    Ok(bounds)
}

fn token_bucket_span_millis(mode: Mode, start_millis: i64, end_millis: i64) -> i64 {
    const HOUR: i64 = 60 * 60 * 1000;
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
    const HOUR: i64 = 60 * 60 * 1000;
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

pub fn load_period_costs(path: &Path, periods: &[PeriodKey]) -> Result<Vec<PeriodCost>> {
    load_period_costs_with_scope(path, periods, None)
}

pub fn load_period_costs_scoped(
    path: &Path,
    periods: &[PeriodKey],
    scope: &Scope,
    current_directory: &Path,
) -> Result<Vec<PeriodCost>> {
    load_period_costs_with_scope(
        path,
        periods,
        Some(ScopeContext {
            scope,
            current_directory,
        }),
    )
}

fn load_period_costs_with_scope(
    path: &Path,
    periods: &[PeriodKey],
    scope: Option<ScopeContext<'_>>,
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

    let connection = open_database(path)?;
    let project_id = resolve_scope(&connection, scope)?;

    let mut statement = connection.prepare(
        r#"
        SELECT
            time_created,
            COALESCE(json_extract(data, '$.cost'), 0) AS cost
        FROM message
        WHERE json_extract(data, '$.role') = 'assistant'
            AND time_created >= ?1
            AND time_created < ?2
            AND (?3 IS NULL OR EXISTS (
                SELECT 1 FROM session
                WHERE session.id = message.session_id
                    AND session.project_id = ?3
            ))
        "#,
    )?;

    let mut costs = periods
        .iter()
        .copied()
        .map(|period| (period, 0.0))
        .collect::<HashMap<_, _>>();

    let rows = statement.query_map(params![start_millis, end_millis, project_id], |row| {
        let time_created: i64 = row.get("time_created")?;
        let cost: f64 = row.get("cost")?;
        Ok((time_created, cost))
    })?;

    for row in rows {
        let (time_created, cost) = row?;
        if let Some(period) = periods
            .iter()
            .copied()
            .find(|period| period.contains(time_created))
        {
            *costs.entry(period).or_insert(0.0) += cost;
        }
    }

    Ok(periods
        .iter()
        .copied()
        .map(|period| PeriodCost {
            period,
            cost: costs.remove(&period).unwrap_or(0.0),
        })
        .collect())
}

pub fn list_projects(path: &Path) -> Result<Vec<ProjectInfo>> {
    let connection = open_database(path)?;
    query_projects(&connection)
}

pub fn project_for_directory(path: &Path, directory: &Path) -> Result<Option<ProjectInfo>> {
    let projects = list_projects(path)?;
    Ok(find_project_for_directory(&projects, directory).cloned())
}

fn resolve_scope(
    connection: &Connection,
    scope: Option<ScopeContext<'_>>,
) -> Result<Option<String>> {
    let Some(scope) = scope else {
        return Ok(None);
    };
    match scope.scope {
        Scope::All => Ok(None),
        Scope::Project(id) => {
            let exists = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM project WHERE id = ?1)",
                [id],
                |row| row.get::<_, bool>(0),
            )?;
            if exists {
                Ok(Some(id.clone()))
            } else {
                Err(anyhow!(
                    "configured OpenCode project {id:?} no longer exists"
                ))
            }
        }
        Scope::Current => {
            let projects = query_projects(connection)?;
            find_project_for_directory(&projects, scope.current_directory)
                .map(|project| Some(project.id.clone()))
                .ok_or_else(|| {
                    anyhow!(
                        "no OpenCode project matches current directory {}",
                        scope.current_directory.display()
                    )
                })
        }
    }
}

fn query_projects(connection: &Connection) -> Result<Vec<ProjectInfo>> {
    let session_columns = table_columns(connection, "session")?;
    let project_columns = table_columns(connection, "project")?;
    let project_scope = ["id", "project_id"]
        .iter()
        .all(|column| session_columns.contains(*column))
        && ["id", "worktree", "name"]
            .iter()
            .all(|column| project_columns.contains(*column));
    if !project_scope {
        return Err(anyhow!(
            "OpenCode database schema does not support project scoping"
        ));
    }

    let mut statement = connection.prepare(
        "SELECT id, COALESCE(NULLIF(name, ''), id), worktree FROM project ORDER BY name COLLATE NOCASE, worktree COLLATE NOCASE",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(ProjectInfo {
            id: row.get(0)?,
            name: row.get(1)?,
            worktree: row.get(2)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("listing OpenCode projects")
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

pub fn diagnose(path: &Path) -> Result<DatabaseDiagnostics> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;
    let sqlite_version = connection
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .context("reading SQLite version")?;
    let json_functions = connection
        .query_row("SELECT json_valid('{\"ok\":true}')", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|value| value == 1)
        .unwrap_or(false);
    let message_columns = table_columns(&connection, "message")?;
    let session_columns = table_columns(&connection, "session")?;
    let project_columns = table_columns(&connection, "project")?;
    let mut errors = usage_schema_errors(&message_columns, &session_columns, json_functions);
    let mut warnings = Vec::new();

    let assistant_messages = if errors.is_empty() {
        match connection.query_row(
            "SELECT COUNT(*) FROM message WHERE json_extract(data, '$.role') = 'assistant'",
            [],
            |row| read_u64(row, "COUNT(*)"),
        ) {
            Ok(count) => Some(count),
            Err(error) => {
                errors.push(format!("could not inspect assistant messages: {error}"));
                None
            }
        }
    } else {
        None
    };

    if message_columns.contains("data") && json_functions {
        let invalid_json: u64 = connection
            .query_row(
                "SELECT COUNT(*) FROM message WHERE NOT json_valid(data)",
                [],
                |row| read_u64(row, "COUNT(*)"),
            )
            .unwrap_or(0);
        if invalid_json > 0 {
            warnings.push(format!("{invalid_json} message rows contain invalid JSON"));
        }
    }

    let project_scope = ["id", "project_id"]
        .iter()
        .all(|column| session_columns.contains(*column))
        && ["id", "worktree", "name"]
            .iter()
            .all(|column| project_columns.contains(*column))
        && message_columns.contains("session_id");
    if !project_scope {
        warnings.push("project-scoped reports are unavailable for this schema".to_string());
    }

    let opencode_versions = if session_columns.contains("version") {
        let mut statement = connection.prepare(
            "SELECT DISTINCT version FROM session WHERE version IS NOT NULL AND version != '' ORDER BY version DESC LIMIT 5",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.filter_map(Result::ok).collect()
    } else {
        Vec::new()
    };

    Ok(DatabaseDiagnostics {
        path: path.display().to_string(),
        sqlite_version,
        json_functions,
        assistant_messages,
        project_scope,
        opencode_versions,
        errors,
        warnings,
    })
}

fn open_database(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;
    validate_usage_schema(&connection)
        .with_context(|| format!("checking OpenCode database schema in {}", path.display()))?;
    Ok(connection)
}

fn validate_usage_schema(connection: &Connection) -> Result<()> {
    let json_functions = connection
        .query_row("SELECT json_valid('{}')", [], |row| row.get::<_, i64>(0))
        .map(|value| value == 1)
        .unwrap_or(false);
    let message_columns = table_columns(connection, "message")?;
    let session_columns = table_columns(connection, "session")?;
    let errors = usage_schema_errors(&message_columns, &session_columns, json_functions);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(errors.join("; ")))
    }
}

fn usage_schema_errors(
    message_columns: &HashSet<String>,
    session_columns: &HashSet<String>,
    json_functions: bool,
) -> Vec<String> {
    let mut errors = Vec::new();
    if message_columns.is_empty() {
        errors.push("missing message table".to_string());
    } else {
        for column in ["data", "time_created"] {
            if !message_columns.contains(column) {
                errors.push(format!("message table is missing {column} column"));
            }
        }
    }
    if session_columns.is_empty() {
        errors.push("missing session table".to_string());
    } else {
        for column in ["id", "project_id"] {
            if !session_columns.contains(column) {
                errors.push(format!("session table is missing {column} column"));
            }
        }
    }
    if !json_functions {
        errors.push("SQLite JSON functions are unavailable".to_string());
    }
    errors
}

fn table_columns(connection: &Connection, table: &str) -> Result<HashSet<String>> {
    let mut statement = connection.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let rows = statement.query_map([table], |row| row.get::<_, String>(0))?;
    rows.collect::<rusqlite::Result<HashSet<_>>>()
        .context("reading database schema")
}

fn snapshot_now() -> i64 {
    Local::now().timestamp_millis().saturating_add(1)
}

fn effective_end(end_millis: Option<i64>, snapshot_millis: i64) -> Option<i64> {
    Some(
        end_millis
            .map(|end| end.min(snapshot_millis))
            .unwrap_or(snapshot_millis),
    )
}

fn read_u64(row: &rusqlite::Row<'_>, name: &str) -> rusqlite::Result<u64> {
    let value: i64 = row.get(name)?;
    Ok(value.max(0) as u64)
}

fn display_name(provider: &str, model_id: &str, variant: &str) -> String {
    let provider = clean_part(provider, "unknown");
    let model_id = clean_part(model_id, "unknown");
    let variant = clean_part(variant, "default");

    let base = if provider == "unknown" {
        model_id.to_string()
    } else {
        format!("{provider}/{model_id}")
    };

    if variant == "default" {
        base
    } else {
        format!("{base} ({variant})")
    }
}

fn clean_part<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    let value = value.trim();
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time_window::CalendarScale;
    use rusqlite::Connection;
    use tempfile::NamedTempFile;

    #[test]
    fn recognizes_supported_opencode_fixture() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(include_str!("../tests/fixtures/opencode.sql"))
            .unwrap();
        drop(connection);

        let diagnostics = diagnose(file.path()).unwrap();

        assert!(diagnostics.is_compatible());
        assert!(diagnostics.json_functions);
        assert!(diagnostics.project_scope);
        assert_eq!(diagnostics.assistant_messages, Some(1));
        assert_eq!(diagnostics.opencode_versions, vec!["1.2.3"]);
    }

    #[test]
    fn scopes_usage_to_selected_or_current_project() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute_batch(include_str!("../tests/fixtures/opencode.sql"))
            .unwrap();
        connection
            .execute_batch(
                r#"
                INSERT INTO project (id, worktree, name)
                VALUES ('project-b', '/work/project-b', 'Project B');
                INSERT INTO session (id, project_id, directory, title, version)
                VALUES ('session-b', 'project-b', '/work/project-b', 'Other', '1.2.3');
                INSERT INTO message (id, session_id, time_created, time_updated, data)
                VALUES (
                    'assistant-b',
                    'session-b',
                    1500,
                    1500,
                    '{"role":"assistant","cost":3.5,"tokens":{"input":1,"output":1,"cache":{"read":1,"write":1}},"modelID":"other","providerID":"provider"}'
                );
                "#,
            )
            .unwrap();
        drop(connection);

        let selected = load_usage_summary_scoped(
            file.path(),
            Mode::AllTime,
            None,
            &Scope::Project("project-a".to_string()),
            Path::new("/elsewhere"),
        )
        .unwrap();
        let current = load_usage_summary_scoped(
            file.path(),
            Mode::AllTime,
            None,
            &Scope::Current,
            Path::new("/work/project-b/src"),
        )
        .unwrap();

        assert_eq!(selected.totals.messages, 1);
        assert!((selected.totals.cost - 1.25).abs() < f64::EPSILON);
        assert_eq!(current.totals.messages, 1);
        assert!((current.totals.cost - 3.5).abs() < f64::EPSILON);
    }

    #[test]
    fn current_scope_prefers_the_deepest_matching_worktree() {
        let projects = vec![
            ProjectInfo {
                id: "outer".to_string(),
                name: "Outer".to_string(),
                worktree: "/work/project".to_string(),
            },
            ProjectInfo {
                id: "inner".to_string(),
                name: "Inner".to_string(),
                worktree: "/work/project/packages/app".to_string(),
            },
        ];

        let project =
            find_project_for_directory(&projects, Path::new("/work/project/packages/app/src"))
                .unwrap();

        assert_eq!(project.id, "inner");
    }

    #[test]
    fn deferred_graph_uses_summary_snapshot_upper_bound() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);
        let before_snapshot = snapshot_now() - 1_000;
        insert_usage_message(&connection, "initial", before_snapshot, "m", 1.0);
        drop(connection);

        let stats = load_usage_summary(file.path(), Mode::AllTime, None).unwrap();
        let connection = Connection::open(file.path()).unwrap();
        insert_usage_message(
            &connection,
            "after-summary",
            stats.snapshot_millis,
            "m",
            1.0,
        );
        drop(connection);

        let buckets = load_usage_token_buckets_at(
            file.path(),
            Mode::AllTime,
            None,
            None,
            stats.snapshot_millis,
        )
        .unwrap();

        assert_eq!(stats.totals.total_tokens(), 4);
        assert_eq!(buckets.iter().map(|bucket| bucket.tokens).sum::<u64>(), 4);
    }

    #[test]
    fn aggregates_assistant_messages_by_model_and_variant() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);

        insert_message(
            &connection,
            "a",
            1000,
            r#"{
                "role":"assistant",
                "cost":1.25,
                "tokens":{"input":10,"output":20,"cache":{"read":30,"write":40}},
                "modelID":"gpt-test",
                "providerID":"provider",
                "variant":"default"
            }"#,
        );
        insert_message(
            &connection,
            "b",
            2000,
            r#"{
                "role":"assistant",
                "cost":2.5,
                "tokens":{"input":1,"output":2,"cache":{"read":3,"write":4}},
                "modelID":"gpt-test",
                "providerID":"provider",
                "variant":"high"
            }"#,
        );
        insert_message(&connection, "c", 3000, r#"{"role":"user"}"#);
        drop(connection);

        let stats = load_usage(file.path(), Mode::AllTime, None).unwrap();
        assert_eq!(stats.totals.messages, 2);
        assert_eq!(stats.totals.input, 11);
        assert_eq!(stats.totals.output, 22);
        assert_eq!(stats.totals.cache_read, 33);
        assert_eq!(stats.totals.cache_write, 44);
        assert_eq!(stats.totals.total_tokens(), 110);
        assert!((stats.totals.cost - 3.75).abs() < f64::EPSILON);
        assert_eq!(stats.models.len(), 2);
        assert_eq!(stats.models[0].display_name, "provider/gpt-test (high)");
        assert_eq!(stats.models[1].display_name, "provider/gpt-test");
        assert_eq!(stats.token_buckets.len(), 1);
        assert_eq!(stats.token_buckets[0].tokens, 110);
    }

    #[test]
    fn applies_cutoff() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);

        let data = r#"{
            "role":"assistant",
            "cost":1.0,
            "tokens":{"input":1,"output":1,"cache":{"read":1,"write":1}},
            "modelID":"m",
            "providerID":"p"
        }"#;
        insert_message(&connection, "old", 1000, data);
        insert_message(&connection, "new", 2000, data);
        drop(connection);

        let stats = load_usage(file.path(), Mode::Daily, Some(1500)).unwrap();
        assert_eq!(stats.totals.messages, 1);
        assert_eq!(stats.totals.total_tokens(), 4);
        assert_eq!(stats.token_buckets.len(), 24);
        assert_eq!(stats.token_buckets[0].tokens, 4);
    }

    #[test]
    fn applies_bounded_range() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);

        insert_usage_message(&connection, "before", 999, "m", 1.0);
        insert_usage_message(&connection, "inside", 1500, "m", 2.0);
        insert_usage_message(&connection, "end", 2000, "m", 4.0);
        drop(connection);

        let stats = load_usage_between(file.path(), Mode::Daily, 1000, 2000).unwrap();

        assert_eq!(stats.cutoff_millis, Some(1000));
        assert_eq!(stats.end_millis, Some(2000));
        assert_eq!(stats.totals.messages, 1);
        assert!((stats.totals.cost - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn summary_load_skips_token_buckets() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);

        insert_usage_message(&connection, "inside", 1500, "m", 2.0);
        drop(connection);

        let stats = load_usage_summary_between(file.path(), Mode::Daily, 1000, 2000).unwrap();
        let token_buckets =
            load_usage_token_buckets_between(file.path(), Mode::Daily, 1000, 2000).unwrap();

        assert_eq!(stats.totals.messages, 1);
        assert!(stats.token_buckets.is_empty());
        assert_eq!(token_buckets.len(), 1);
        assert_eq!(token_buckets[0].tokens, 4);
    }

    #[test]
    fn buckets_daily_tokens_by_hour_window() {
        const HOUR: i64 = 60 * 60 * 1000;
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);

        insert_usage_message(&connection, "first", HOUR / 2, "m", 1.0);
        insert_usage_message(&connection, "second", HOUR + HOUR / 2, "m", 1.0);
        insert_usage_message(&connection, "outside", 4 * HOUR, "m", 1.0);
        drop(connection);

        let stats = load_usage_between(file.path(), Mode::Daily, 0, 4 * HOUR).unwrap();

        assert_eq!(stats.token_buckets.len(), 4);
        assert_eq!(stats.token_buckets[0].tokens, 4);
        assert_eq!(stats.token_buckets[1].tokens, 4);
        assert_eq!(stats.token_buckets[2].tokens, 0);
        assert_eq!(stats.token_buckets[3].tokens, 0);
    }

    #[test]
    fn loads_period_costs_in_requested_order() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);

        insert_usage_message(&connection, "first", 1500, "m", 1.5);
        insert_usage_message(&connection, "second", 2500, "m", 2.5);
        insert_usage_message(&connection, "outside", 3000, "m", 4.0);
        insert_message(&connection, "user", 1500, r#"{"role":"user"}"#);
        drop(connection);

        let periods = vec![
            PeriodKey {
                scale: CalendarScale::Day,
                start_millis: 1000,
                end_millis: 2000,
            },
            PeriodKey {
                scale: CalendarScale::Day,
                start_millis: 2000,
                end_millis: 3000,
            },
        ];
        let costs = load_period_costs(file.path(), &periods).unwrap();

        assert_eq!(costs.len(), 2);
        assert_eq!(costs[0].period, periods[0]);
        assert_eq!(costs[1].period, periods[1]);
        assert!((costs[0].cost - 1.5).abs() < f64::EPSILON);
        assert!((costs[1].cost - 2.5).abs() < f64::EPSILON);
    }

    #[test]
    fn treats_missing_optional_usage_fields_as_zero() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);

        insert_message(
            &connection,
            "a",
            1000,
            r#"{"role":"assistant","modelID":"","providerID":"","cost":null}"#,
        );
        drop(connection);

        let stats = load_usage(file.path(), Mode::AllTime, None).unwrap();

        assert_eq!(stats.totals.messages, 1);
        assert_eq!(stats.totals.cost, 0.0);
        assert_eq!(stats.totals.total_tokens(), 0);
        assert_eq!(stats.models[0].display_name, "unknown");
    }

    #[test]
    fn sorts_models_by_cost_descending() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);

        insert_usage_message(&connection, "cheap", 1000, "cheap", 0.5);
        insert_usage_message(&connection, "expensive", 2000, "expensive", 2.0);
        drop(connection);

        let stats = load_usage(file.path(), Mode::AllTime, None).unwrap();

        assert_eq!(stats.models[0].display_name, "provider/expensive");
        assert_eq!(stats.models[1].display_name, "provider/cheap");
    }

    fn insert_message(connection: &Connection, id: &str, time: i64, data: &str) {
        connection
            .execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES (?1, 'session', ?2, ?2, ?3)",
                (id, time, data),
            )
            .unwrap();
    }

    fn insert_usage_message(
        connection: &Connection,
        id: &str,
        time: i64,
        model_id: &str,
        cost: f64,
    ) {
        let data = format!(
            r#"{{
                "role":"assistant",
                "cost":{cost},
                "tokens":{{"input":1,"output":1,"cache":{{"read":1,"write":1}}}},
                "modelID":"{model_id}",
                "providerID":"provider"
            }}"#
        );
        insert_message(connection, id, time, &data);
    }

    fn create_message_table(connection: &Connection) {
        connection
            .execute_batch(
                "CREATE TABLE session (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL
                );
                INSERT INTO session (id, project_id) VALUES ('session', 'project');
                CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                );",
            )
            .unwrap();
    }
}
