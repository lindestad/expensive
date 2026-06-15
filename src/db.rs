use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
use rusqlite::{params, Connection, OpenFlags};

use crate::time_window::Mode;

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
    pub totals: UsageTotals,
    pub models: Vec<ModelUsage>,
}

pub fn load_usage(path: &Path, mode: Mode, cutoff_millis: Option<i64>) -> Result<UsageStats> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening {}", path.display()))?;

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
        GROUP BY provider, model_id, variant
        ORDER BY cost DESC
        "#,
    )?;

    let rows = statement.query_map(params![cutoff_millis], |row| {
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

    Ok(UsageStats {
        mode,
        refreshed_at: Local::now(),
        cutoff_millis,
        totals,
        models,
    })
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
}
