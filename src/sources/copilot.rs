use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags, Row};
use serde::{Deserialize, Serialize};

use crate::{
    index::{
        project_id_for_worktree, ArtifactCheckpoint, ArtifactRecord, CostKind, ProjectRecord,
        SourceKind, SourceRegistration, UsageEvent, UsageIndex,
    },
    sources::{event_key, SyncMode, SyncReport, UsageSource},
};

const PARSER_VERSION: i64 = 1;
const SUPPORTED_SCHEMA_VERSION: i64 = 6;
const BOUNDARY_ROWS: i64 = 16;

pub struct CopilotSource {
    home: PathBuf,
}

impl CopilotSource {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }

    pub fn database_path(&self) -> PathBuf {
        self.home.join("session-store.db")
    }

    fn source_key(&self) -> String {
        self.home
            .canonicalize()
            .unwrap_or_else(|_| self.home.clone())
            .display()
            .to_string()
    }
}

impl UsageSource for CopilotSource {
    fn registration(&self) -> SourceRegistration {
        SourceRegistration {
            kind: SourceKind::Copilot,
            source_key: self.source_key(),
            display_name: "Copilot".to_string(),
        }
    }

    fn sync(&self, index: &mut UsageIndex, requested_mode: SyncMode) -> Result<SyncReport> {
        let source_id = index.register_source(&self.registration())?;
        let path = self.database_path();
        let connection = open_database(&path)?;
        validate_schema(&connection)?;

        let checkpoint = index.artifact_checkpoint(source_id, "database")?;
        let metadata =
            fs::metadata(&path).with_context(|| format!("reading {}", path.display()))?;
        let (device, inode) = file_identity(&metadata);
        let max_id = connection.query_row(
            "SELECT COALESCE(MAX(id), 0) FROM assistant_usage_events",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let watermark = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.parsed_offset.max(0))
            .unwrap_or(0);
        let parser_changed = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.parser_version != PARSER_VERSION)
            .unwrap_or(true);
        let schema_changed = checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.cursor.as_deref())
            .and_then(|cursor| serde_json::from_str::<Cursor>(cursor).ok())
            .map(|cursor| cursor.schema_version != SUPPORTED_SCHEMA_VERSION)
            .unwrap_or(checkpoint.is_some());
        let identity_changed = checkpoint
            .as_ref()
            .is_some_and(|checkpoint| file_identity_changed(checkpoint, device, inode));
        let boundary_changed = if let Some(checkpoint) = &checkpoint {
            let current = prefix_boundary_hash(&connection, watermark)?;
            checkpoint.boundary_hash.as_ref() != Some(&current)
        } else {
            false
        };
        let full_scan = requested_mode == SyncMode::Full
            || parser_changed
            || schema_changed
            || identity_changed
            || max_id < watermark
            || boundary_changed;

        if !full_scan && max_id == watermark {
            return Ok(SyncReport::default());
        }

        let scan_from = if full_scan { 0 } else { watermark };
        let mut statement = connection.prepare(
            "SELECT usage.id, usage.session_id, usage.model,
                    usage.input_tokens, usage.output_tokens,
                    usage.cache_read_tokens, usage.cache_write_tokens,
                    usage.reasoning_tokens, usage.total_nano_aiu,
                    usage.reasoning_effort, usage.token_details_json,
                    usage.created_at, sessions.cwd, sessions.repository
             FROM assistant_usage_events AS usage
             LEFT JOIN sessions ON sessions.id = usage.session_id
             WHERE usage.id > ?1
             ORDER BY usage.id",
        )?;
        let mut rows = statement.query([scan_from])?;
        let mut events = Vec::new();
        let mut projects = Vec::<(String, ProjectRecord)>::new();
        let mut scanned = 0usize;
        let mut skipped = 0usize;

        while let Some(row) = rows.next()? {
            scanned += 1;
            let raw = read_usage_row(row)?;
            match usage_event(&raw) {
                Ok(event) => {
                    if let Some(project) = project_from_row(&raw) {
                        projects.push((raw.session_id.clone(), project));
                    }
                    events.push(event);
                }
                Err(_) => skipped += 1,
            }
        }
        drop(rows);
        drop(statement);

        for (native_id, project) in projects {
            index.upsert_project(source_id, &native_id, &project)?;
        }
        let cursor = serde_json::to_string(&Cursor {
            schema_version: SUPPORTED_SCHEMA_VERSION,
        })?;
        let boundary_hash = prefix_boundary_hash(&connection, max_id)?;
        let artifact = database_artifact(
            &path,
            &metadata,
            max_id,
            boundary_hash,
            cursor,
            device,
            inode,
        );
        let change = if full_scan {
            index.replace_artifact_events(source_id, &artifact, &events)?
        } else {
            index.apply_artifact_changes(source_id, &artifact, &events, &[])?
        };

        Ok(SyncReport {
            change: Some(change),
            scanned,
            imported: events.len(),
            removed: 0,
            skipped,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Cursor {
    schema_version: i64,
}

#[derive(Clone, Debug)]
struct RawUsageRow {
    id: i64,
    session_id: String,
    model: String,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    cache_write_tokens: Option<i64>,
    reasoning_tokens: Option<i64>,
    total_nano_aiu: Option<i64>,
    reasoning_effort: Option<String>,
    token_details_json: Option<String>,
    created_at: Option<String>,
    cwd: Option<String>,
    repository: Option<String>,
}

fn read_usage_row(row: &Row<'_>) -> rusqlite::Result<RawUsageRow> {
    Ok(RawUsageRow {
        id: row.get(0)?,
        session_id: row.get(1)?,
        model: row.get(2)?,
        input_tokens: row.get(3)?,
        output_tokens: row.get(4)?,
        cache_read_tokens: row.get(5)?,
        cache_write_tokens: row.get(6)?,
        reasoning_tokens: row.get(7)?,
        total_nano_aiu: row.get(8)?,
        reasoning_effort: row.get(9)?,
        token_details_json: row.get(10)?,
        created_at: row.get(11)?,
        cwd: row.get(12)?,
        repository: row.get(13)?,
    })
}

fn usage_event(raw: &RawUsageRow) -> Result<UsageEvent> {
    let occurred_at_ms = raw
        .created_at
        .as_deref()
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.timestamp_millis())
        .context("Copilot usage event has an invalid timestamp")?;
    let tokens = parse_token_details(raw.token_details_json.as_deref())
        .unwrap_or_else(|| TokenCounts::from_columns(raw));
    let total_tokens = tokens
        .input
        .saturating_add(tokens.output)
        .saturating_add(tokens.cache_read)
        .saturating_add(tokens.cache_write);
    let native_key = format!("{}\0{}", raw.session_id, raw.id);
    let cost_microusd = raw.total_nano_aiu.and_then(nano_aiu_to_microusd);

    Ok(UsageEvent {
        event_key: event_key("copilot", &native_key),
        occurred_at_ms,
        project_id: project_from_row(raw).map(|project| project.id),
        provider: "github-copilot".to_string(),
        model: nonempty_or(&raw.model, "unknown").to_string(),
        variant: raw
            .reasoning_effort
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("default")
            .to_string(),
        messages: 1,
        input_tokens: tokens.input,
        output_tokens: tokens.output,
        cache_read_tokens: tokens.cache_read,
        cache_write_tokens: tokens.cache_write,
        cache_write_1h_tokens: 0,
        reasoning_tokens: tokens.reasoning,
        total_tokens,
        cost_microusd,
        cost_kind: if cost_microusd.is_some() {
            CostKind::Reported
        } else {
            CostKind::Unavailable
        },
        is_sidechain: false,
        has_detailed_cache: false,
    })
}

#[derive(Clone, Copy, Debug, Default)]
struct TokenCounts {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    reasoning: i64,
}

impl TokenCounts {
    fn from_columns(raw: &RawUsageRow) -> Self {
        let cache_read = nonnegative(raw.cache_read_tokens);
        let cache_write = nonnegative(raw.cache_write_tokens);
        Self {
            input: nonnegative(raw.input_tokens)
                .saturating_sub(cache_read)
                .saturating_sub(cache_write),
            output: nonnegative(raw.output_tokens),
            cache_read,
            cache_write,
            reasoning: nonnegative(raw.reasoning_tokens),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenDetail {
    token_type: String,
    token_count: i64,
}

fn parse_token_details(raw: Option<&str>) -> Option<TokenCounts> {
    let details = serde_json::from_str::<Vec<TokenDetail>>(raw?).ok()?;
    let mut tokens = TokenCounts::default();
    let mut recognized = false;
    for detail in details {
        let count = detail.token_count.max(0);
        match detail.token_type.as_str() {
            "input" => tokens.input = tokens.input.saturating_add(count),
            "output" => tokens.output = tokens.output.saturating_add(count),
            "cache_read" => tokens.cache_read = tokens.cache_read.saturating_add(count),
            "cache_write" => tokens.cache_write = tokens.cache_write.saturating_add(count),
            "reasoning" => tokens.reasoning = tokens.reasoning.saturating_add(count),
            _ => continue,
        }
        recognized = true;
    }
    recognized.then_some(tokens)
}

fn project_from_row(raw: &RawUsageRow) -> Option<ProjectRecord> {
    let cwd = raw
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())?;
    let path = Path::new(cwd);
    let name = raw
        .repository
        .as_deref()
        .map(str::trim)
        .filter(|repository| !repository.is_empty())
        .and_then(|repository| repository.rsplit('/').next())
        .or_else(|| path.file_name().and_then(|name| name.to_str()))
        .unwrap_or(cwd)
        .to_string();
    Some(ProjectRecord {
        id: project_id_for_worktree(path),
        name,
        worktree: cwd.to_string(),
    })
}

fn open_database(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(|| format!("opening Copilot database {}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

fn validate_schema(connection: &Connection) -> Result<()> {
    let version = connection
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get::<_, i64>(0)
        })
        .context("reading Copilot schema version")?;
    if version != SUPPORTED_SCHEMA_VERSION {
        bail!(
            "unsupported Copilot session-store schema {version}; expected {SUPPORTED_SCHEMA_VERSION}"
        );
    }
    require_columns(
        connection,
        "assistant_usage_events",
        &[
            "id",
            "session_id",
            "model",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "reasoning_tokens",
            "total_nano_aiu",
            "reasoning_effort",
            "token_details_json",
            "created_at",
        ],
    )?;
    require_columns(connection, "sessions", &["id", "cwd", "repository"])
}

fn require_columns(connection: &Connection, table: &str, required: &[&str]) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<HashSet<_>>>()?;
    let missing = required
        .iter()
        .filter(|column| !columns.contains(**column))
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        bail!(
            "Copilot table {table} is missing required columns: {}",
            missing.join(", ")
        );
    }
    Ok(())
}

fn prefix_boundary_hash(connection: &Connection, watermark: i64) -> Result<Vec<u8>> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"expensive-copilot-prefix-v1\0");
    hasher.update(&watermark.to_le_bytes());
    let count = connection.query_row(
        "SELECT COUNT(*) FROM assistant_usage_events WHERE id <= ?1",
        [watermark],
        |row| row.get::<_, i64>(0),
    )?;
    hasher.update(&count.to_le_bytes());
    hash_boundary_rows(connection, watermark, false, &mut hasher)?;
    hash_boundary_rows(connection, watermark, true, &mut hasher)?;
    Ok(hasher.finalize().as_bytes().to_vec())
}

fn hash_boundary_rows(
    connection: &Connection,
    watermark: i64,
    descending: bool,
    hasher: &mut blake3::Hasher,
) -> Result<()> {
    let order = if descending { "DESC" } else { "ASC" };
    let sql = format!(
        "SELECT usage.id, usage.session_id, usage.model,
                usage.input_tokens, usage.output_tokens,
                usage.cache_read_tokens, usage.cache_write_tokens,
                usage.reasoning_tokens, usage.total_nano_aiu,
                usage.reasoning_effort, usage.token_details_json,
                usage.created_at, sessions.cwd, sessions.repository
         FROM assistant_usage_events AS usage
         LEFT JOIN sessions ON sessions.id = usage.session_id
         WHERE usage.id <= ?1
         ORDER BY usage.id {order}
         LIMIT {BOUNDARY_ROWS}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([watermark], read_usage_row)?;
    for row in rows {
        hasher.update(format!("{:?}\0", row?).as_bytes());
    }
    Ok(())
}

fn database_artifact(
    path: &Path,
    metadata: &fs::Metadata,
    watermark: i64,
    boundary_hash: Vec<u8>,
    cursor: String,
    device: Option<i64>,
    inode: Option<i64>,
) -> ArtifactRecord {
    ArtifactRecord {
        key: "database".to_string(),
        path: Some(path.display().to_string()),
        device,
        inode,
        size: i64::try_from(metadata.len()).ok(),
        modified_ns: modified_ns(metadata),
        parsed_offset: watermark.max(0),
        boundary_hash: Some(boundary_hash),
        full_hash: None,
        cursor: Some(cursor),
        parser_version: PARSER_VERSION,
        scanned_at_ms: Utc::now().timestamp_millis(),
    }
}

fn file_identity_changed(
    checkpoint: &ArtifactCheckpoint,
    device: Option<i64>,
    inode: Option<i64>,
) -> bool {
    match (checkpoint.device, checkpoint.inode, device, inode) {
        (Some(old_device), Some(old_inode), Some(device), Some(inode)) => {
            old_device != device || old_inode != inode
        }
        _ => false,
    }
}

fn modified_ns(metadata: &fs::Metadata) -> Option<i64> {
    let duration = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(duration.as_nanos()).ok()
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> (Option<i64>, Option<i64>) {
    use std::os::unix::fs::MetadataExt;
    (
        i64::try_from(metadata.dev()).ok(),
        i64::try_from(metadata.ino()).ok(),
    )
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> (Option<i64>, Option<i64>) {
    (None, None)
}

fn nano_aiu_to_microusd(value: i64) -> Option<i64> {
    let value = i128::from(value);
    if value < 0 {
        return None;
    }
    i64::try_from((value + 50_000) / 100_000).ok()
}

fn nonnegative(value: Option<i64>) -> i64 {
    value.unwrap_or(0).max(0)
}

fn nonempty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
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
    use crate::{config::Scope, index::load_usage_range_scoped, time_window::Mode};

    fn create_fixture(home: &Path) -> PathBuf {
        fs::create_dir_all(home).unwrap();
        let path = home.join("session-store.db");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(include_str!("../../tests/fixtures/copilot.sql"))
            .unwrap();
        path
    }

    #[test]
    fn imports_current_schema_token_details_and_ai_credit_value() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("copilot");
        create_fixture(&home);
        let index_path = directory.path().join("usage.sqlite3");
        let mut index = UsageIndex::open(&index_path).unwrap();

        let report = CopilotSource::new(home)
            .sync(&mut index, SyncMode::Incremental)
            .unwrap();

        assert_eq!(report.scanned, 1);
        assert_eq!(report.imported, 1);
        assert_eq!(report.skipped, 0);
        let stats = load_usage_range_scoped(
            &index_path,
            Mode::AllTime,
            None,
            None,
            false,
            &Scope::All,
            Path::new("/elsewhere"),
        )
        .unwrap();
        assert_eq!(stats.totals.messages, 1);
        assert_eq!(stats.totals.input, 3);
        assert_eq!(stats.totals.output, 5);
        assert_eq!(stats.totals.cache_read, 0);
        assert_eq!(stats.totals.cache_write, 15_995);
        assert_eq!(stats.totals.total_tokens(), 16_003);
        assert!((stats.totals.cost - 0.050067).abs() < f64::EPSILON);
        assert_eq!(stats.models[0].provider, "github-copilot");
        assert_eq!(stats.models[0].model_id, "gpt-5.6-terra");
        assert_eq!(stats.models[0].variant, "high");
        assert_eq!(
            crate::index::list_projects(&index_path).unwrap()[0].name,
            "copilot-project"
        );
    }

    #[test]
    fn incrementally_reads_only_new_usage_rows() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("copilot");
        let source_path = create_fixture(&home);
        let index_path = directory.path().join("usage.sqlite3");
        let mut index = UsageIndex::open(&index_path).unwrap();
        let source = CopilotSource::new(home);

        source.sync(&mut index, SyncMode::Incremental).unwrap();
        let generation = index.diagnostics().unwrap().generation;
        let unchanged = source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(unchanged, SyncReport::default());
        assert_eq!(index.diagnostics().unwrap().generation, generation);

        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute(
                "INSERT INTO assistant_usage_events (
                    session_id, model, input_tokens, output_tokens,
                    cache_read_tokens, cache_write_tokens, reasoning_tokens,
                    total_nano_aiu, token_details_json, created_at
                 ) VALUES (
                    'session-a', 'claude-sonnet-5', 10, 20, 0, 0, 0,
                    3000000000,
                    '[{\"tokenType\":\"input\",\"tokenCount\":10},{\"tokenType\":\"output\",\"tokenCount\":20}]',
                    '2026-07-21T09:00:00Z'
                 )",
                [],
            )
            .unwrap();
        drop(connection);

        let update = source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(update.scanned, 1);
        assert_eq!(update.imported, 1);
        assert_eq!(index.diagnostics().unwrap().events, 2);
        assert_eq!(index.diagnostics().unwrap().generation, generation + 1);
    }

    #[test]
    fn reconciles_mutated_prefix_instead_of_treating_it_as_an_append() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("copilot");
        let source_path = create_fixture(&home);
        let index_path = directory.path().join("usage.sqlite3");
        let mut index = UsageIndex::open(&index_path).unwrap();
        let source = CopilotSource::new(home);
        source.sync(&mut index, SyncMode::Incremental).unwrap();

        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute(
                "UPDATE assistant_usage_events
                 SET model = 'replacement-model', total_nano_aiu = 1000000000
                 WHERE id = 1",
                [],
            )
            .unwrap();
        drop(connection);

        let update = source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(update.scanned, 1);
        assert_eq!(index.diagnostics().unwrap().events, 1);
        let stats = load_usage_range_scoped(
            &index_path,
            Mode::AllTime,
            None,
            None,
            false,
            &Scope::All,
            Path::new("/elsewhere"),
        )
        .unwrap();
        assert_eq!(stats.models[0].model_id, "replacement-model");
        assert!((stats.totals.cost - 0.01).abs() < f64::EPSILON);
    }

    #[test]
    fn replaces_indexed_events_when_session_store_is_rebuilt() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("copilot");
        let source_path = create_fixture(&home);
        let index_path = directory.path().join("usage.sqlite3");
        let mut index = UsageIndex::open(&index_path).unwrap();
        let source = CopilotSource::new(home.clone());
        source.sync(&mut index, SyncMode::Incremental).unwrap();

        fs::remove_file(&source_path).unwrap();
        create_fixture(&home);
        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute(
                "UPDATE assistant_usage_events
                 SET session_id = 'session-a', model = 'after-reindex',
                     total_nano_aiu = 2000000000
                 WHERE id = 1",
                [],
            )
            .unwrap();
        drop(connection);

        let update = source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(update.scanned, 1);
        assert_eq!(index.diagnostics().unwrap().events, 1);
        let stats = load_usage_range_scoped(
            &index_path,
            Mode::AllTime,
            None,
            None,
            false,
            &Scope::All,
            Path::new("/elsewhere"),
        )
        .unwrap();
        assert_eq!(stats.models[0].model_id, "after-reindex");
        assert!((stats.totals.cost - 0.02).abs() < f64::EPSILON);
    }

    #[test]
    fn rejects_unknown_session_store_schema() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().join("copilot");
        let source_path = create_fixture(&home);
        let connection = Connection::open(source_path).unwrap();
        connection
            .execute("UPDATE schema_version SET version = 7", [])
            .unwrap();
        drop(connection);
        let mut index = UsageIndex::open(&directory.path().join("usage.sqlite3")).unwrap();

        let error = CopilotSource::new(home)
            .sync(&mut index, SyncMode::Incremental)
            .unwrap_err();

        assert!(error.to_string().contains("schema 7"));
        assert!(error.to_string().contains("expected 6"));
    }
}
