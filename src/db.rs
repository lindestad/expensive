//! Aggregation over OpenCode's local SQLite database.
//!
//! OpenCode stores usage details in JSON inside assistant message rows. This
//! module reads those rows directly and produces the same cost/token categories
//! the TUI displays, grouped by provider, model, and variant.

use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use rusqlite::{params, Connection, OpenFlags};

use crate::time_window::{Mode, PeriodKey};

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
    pub cutoff_millis: Option<i64>,
    pub end_millis: Option<i64>,
    pub totals: UsageTotals,
    pub models: Vec<ModelUsage>,
    pub token_buckets: Vec<TokenBucket>,
}

#[derive(Clone, Debug, Default)]
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

pub fn load_usage(path: &Path, mode: Mode, cutoff_millis: Option<i64>) -> Result<UsageStats> {
    load_usage_range(path, mode, cutoff_millis, None)
}

pub fn load_usage_between(
    path: &Path,
    mode: Mode,
    start_millis: i64,
    end_millis: i64,
) -> Result<UsageStats> {
    load_usage_range(path, mode, Some(start_millis), Some(end_millis))
}

fn load_usage_range(
    path: &Path,
    mode: Mode,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
) -> Result<UsageStats> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;

    let (totals, models) = load_model_usage(&connection, start_millis, end_millis)?;
    let token_buckets = load_token_buckets(&connection, mode, start_millis, end_millis)?;

    Ok(UsageStats {
        mode,
        refreshed_at: Local::now(),
        cutoff_millis: start_millis,
        end_millis,
        totals,
        models,
        token_buckets,
    })
}

fn load_model_usage(
    connection: &Connection,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
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
        GROUP BY provider, model_id, variant
        ORDER BY cost DESC
        "#,
    )?;

    let rows = statement.query_map(params![start_millis, end_millis], |row| {
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
    end_millis: Option<i64>,
) -> Result<Vec<TokenBucket>> {
    let Some((first_time, last_time)) = usage_time_bounds(connection, start_millis, end_millis)?
    else {
        return Ok(Vec::new());
    };
    let range_start = start_millis.unwrap_or(first_time);
    let span = token_bucket_span_millis(mode, range_start, last_time.max(range_start));
    let range_end = token_bucket_range_end(mode, range_start, last_time, end_millis, span);
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
            (time_created - ?3) / ?4 AS bucket_idx,
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
            AND time_created >= ?3
            AND time_created < ?5
        GROUP BY bucket_idx
        ORDER BY bucket_idx ASC
        "#,
    )?;

    let rows = statement.query_map(
        params![start_millis, end_millis, range_start, span, range_end],
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

fn usage_time_bounds(
    connection: &Connection,
    start_millis: Option<i64>,
    end_millis: Option<i64>,
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
        "#,
    )?;

    let bounds = statement.query_row(params![start_millis, end_millis], |row| {
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
        Mode::Daily => 2 * HOUR,
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

    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;

    let mut statement = connection.prepare(
        r#"
        SELECT
            time_created,
            COALESCE(json_extract(data, '$.cost'), 0) AS cost
        FROM message
        WHERE json_extract(data, '$.role') = 'assistant'
            AND time_created >= ?1
            AND time_created < ?2
        "#,
    )?;

    let mut costs = periods
        .iter()
        .copied()
        .map(|period| (period, 0.0))
        .collect::<HashMap<_, _>>();

    let rows = statement.query_map(params![start_millis, end_millis], |row| {
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
    fn aggregates_assistant_messages_by_model_and_variant() {
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        connection
            .execute(
                "CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                )",
                [],
            )
            .unwrap();

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
        connection
            .execute(
                "CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                )",
                [],
            )
            .unwrap();

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
        assert_eq!(stats.token_buckets.len(), 12);
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
    fn buckets_daily_tokens_by_two_hour_window() {
        const HOUR: i64 = 60 * 60 * 1000;
        let file = NamedTempFile::new().unwrap();
        let connection = Connection::open(file.path()).unwrap();
        create_message_table(&connection);

        insert_usage_message(&connection, "first", HOUR / 2, "m", 1.0);
        insert_usage_message(&connection, "second", HOUR + HOUR / 2, "m", 1.0);
        insert_usage_message(&connection, "outside", 4 * HOUR, "m", 1.0);
        drop(connection);

        let stats = load_usage_between(file.path(), Mode::Daily, 0, 4 * HOUR).unwrap();

        assert_eq!(stats.token_buckets.len(), 2);
        assert_eq!(stats.token_buckets[0].tokens, 8);
        assert_eq!(stats.token_buckets[1].tokens, 0);
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
            .execute(
                "CREATE TABLE message (
                    id TEXT PRIMARY KEY,
                    session_id TEXT NOT NULL,
                    time_created INTEGER NOT NULL,
                    time_updated INTEGER NOT NULL,
                    data TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
    }
}
