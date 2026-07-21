//! Durable, source-neutral usage index.
//!
//! Source adapters write normalized accounting facts here. The index stores no
//! prompts, responses, tool calls, or other conversation content.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

mod query;

pub use query::{
    list_projects, list_providers, load_period_costs_scoped, load_period_costs_scoped_with_options,
    load_usage_range_scoped, load_usage_range_scoped_with_options,
    load_usage_token_buckets_at_scoped, load_usage_token_buckets_at_scoped_with_options,
    UsageQueryOptions,
};

const SCHEMA_VERSION: i64 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    OpenCode,
    Codex,
    Pi,
    Claude,
}

impl SourceKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::OpenCode => "opencode",
            Self::Codex => "codex",
            Self::Pi => "pi",
            Self::Claude => "claude",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRegistration {
    pub kind: SourceKind,
    /// Stable identifier for this installation or account, such as a database
    /// path. It is local metadata and must never contain credentials.
    pub source_key: String,
    pub display_name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CostKind {
    Reported,
    Estimated,
    Unavailable,
}

impl CostKind {
    fn key(self) -> &'static str {
        match self {
            Self::Reported => "reported",
            Self::Estimated => "estimated",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectRecord {
    /// Source-neutral project identifier. Adapters should derive this from a
    /// canonical worktree when one is available.
    pub id: String,
    pub name: String,
    pub worktree: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactRecord {
    pub key: String,
    pub path: Option<String>,
    pub device: Option<i64>,
    pub inode: Option<i64>,
    pub size: Option<i64>,
    pub modified_ns: Option<i64>,
    pub parsed_offset: i64,
    pub boundary_hash: Option<Vec<u8>>,
    pub full_hash: Option<Vec<u8>>,
    /// Adapter-owned summary state required to continue parsing at
    /// `parsed_offset`. It must never contain conversation content.
    pub cursor: Option<String>,
    pub parser_version: i64,
    pub scanned_at_ms: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactCheckpoint {
    pub device: Option<i64>,
    pub inode: Option<i64>,
    pub size: Option<i64>,
    pub modified_ns: Option<i64>,
    pub parsed_offset: i64,
    pub boundary_hash: Option<Vec<u8>>,
    pub full_hash: Option<Vec<u8>>,
    pub cursor: Option<String>,
    pub parser_version: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UsageEvent {
    /// Stable identity supplied by the adapter. This is separate from hashes
    /// used to decide whether the source artifact changed.
    pub event_key: Vec<u8>,
    pub occurred_at_ms: i64,
    pub project_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub variant: String,
    pub messages: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub reasoning_tokens: i64,
    /// Source-reported total when available, otherwise the adapter's canonical
    /// non-overlapping total.
    pub total_tokens: i64,
    pub cost_microusd: Option<i64>,
    pub cost_kind: CostKind,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IndexChange {
    pub generation: i64,
    pub source_id: i64,
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    pub event_count: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexDiagnostics {
    pub path: PathBuf,
    pub sqlite_version: String,
    pub schema_version: i64,
    pub generation: i64,
    pub sources: i64,
    pub artifacts: i64,
    pub events: i64,
}

pub struct UsageIndex {
    path: PathBuf,
    connection: Connection,
}

impl UsageIndex {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating usage index directory {}", parent.display()))?;
        }

        let connection = Connection::open(path)
            .with_context(|| format!("opening usage index {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(30))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        let journal_mode: String =
            connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            connection.pragma_update(None, "journal_mode", "WAL")?;
        }
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        migrate(&connection)
            .with_context(|| format!("migrating usage index {}", path.display()))?;

        Ok(Self {
            path: path.to_path_buf(),
            connection,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn register_source(&self, source: &SourceRegistration) -> Result<i64> {
        self.connection.execute(
            "INSERT INTO sources (kind, source_key, display_name)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(kind, source_key) DO UPDATE SET display_name = excluded.display_name",
            params![source.kind.key(), source.source_key, source.display_name],
        )?;
        self.connection
            .query_row(
                "SELECT id FROM sources WHERE kind = ?1 AND source_key = ?2",
                params![source.kind.key(), source.source_key],
                |row| row.get(0),
            )
            .context("reading registered source")
    }

    pub fn upsert_project(
        &self,
        source_id: i64,
        native_id: &str,
        project: &ProjectRecord,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO projects (id, name, worktree)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                worktree = excluded.worktree",
            params![project.id, project.name, project.worktree],
        )?;
        self.connection.execute(
            "INSERT INTO source_projects (source_id, native_id, project_id)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(source_id, native_id) DO UPDATE SET project_id = excluded.project_id",
            params![source_id, native_id, project.id],
        )?;
        Ok(())
    }

    pub fn artifact_checkpoint(
        &self,
        source_id: i64,
        artifact_key: &str,
    ) -> Result<Option<ArtifactCheckpoint>> {
        self.connection
            .query_row(
                "SELECT device, inode, size, modified_ns, parsed_offset,
                        boundary_hash, full_hash, cursor, parser_version
                 FROM artifacts
                 WHERE source_id = ?1 AND artifact_key = ?2",
                params![source_id, artifact_key],
                |row| {
                    Ok(ArtifactCheckpoint {
                        device: row.get(0)?,
                        inode: row.get(1)?,
                        size: row.get(2)?,
                        modified_ns: row.get(3)?,
                        parsed_offset: row.get(4)?,
                        boundary_hash: row.get(5)?,
                        full_hash: row.get(6)?,
                        cursor: row.get(7)?,
                        parser_version: row.get(8)?,
                    })
                },
            )
            .optional()
            .context("reading artifact checkpoint")
    }

    /// Atomically replaces one artifact's event membership. Events referenced
    /// by another artifact remain in the index, which de-duplicates copied
    /// histories while still allowing a source file to be rescanned safely.
    pub fn replace_artifact_events(
        &mut self,
        source_id: i64,
        artifact: &ArtifactRecord,
        events: &[UsageEvent],
    ) -> Result<IndexChange> {
        let transaction = self.connection.transaction()?;
        let artifact_id = upsert_artifact(&transaction, source_id, artifact)?;
        let old_bounds = artifact_event_bounds(&transaction, artifact_id)?;

        transaction.execute(
            "DELETE FROM artifact_events WHERE artifact_id = ?1",
            [artifact_id],
        )?;
        for event in events {
            validate_event(event)?;
            let event_id = upsert_event(&transaction, source_id, event)?;
            transaction.execute(
                "INSERT OR IGNORE INTO artifact_events (artifact_id, event_id)
                 VALUES (?1, ?2)",
                params![artifact_id, event_id],
            )?;
        }

        transaction.execute(
            "DELETE FROM usage_events
             WHERE source_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM artifact_events
                   WHERE artifact_events.event_id = usage_events.id
               )",
            [source_id],
        )?;
        transaction.execute(
            "UPDATE sources
             SET last_sync_ms = ?2, last_error = NULL
             WHERE id = ?1",
            params![source_id, artifact.scanned_at_ms],
        )?;
        transaction.execute(
            "UPDATE index_state SET generation = generation + 1 WHERE id = 1",
            [],
        )?;
        let generation = transaction.query_row(
            "SELECT generation FROM index_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;

        let new_bounds = event_bounds(events);
        let (start_ms, end_ms) = merge_bounds(old_bounds, new_bounds);
        Ok(IndexChange {
            generation,
            source_id,
            start_ms,
            end_ms,
            event_count: events.len(),
        })
    }

    /// Applies an append/update scan without forgetting unchanged events from
    /// the same artifact. Deletions are explicit stable event keys discovered
    /// by the source adapter (for example, a changed row that is no longer an
    /// assistant response).
    pub fn apply_artifact_changes(
        &mut self,
        source_id: i64,
        artifact: &ArtifactRecord,
        upserts: &[UsageEvent],
        removals: &[Vec<u8>],
    ) -> Result<IndexChange> {
        let transaction = self.connection.transaction()?;
        let artifact_id = upsert_artifact(&transaction, source_id, artifact)?;
        let old_bounds = removed_event_bounds(&transaction, artifact_id, removals)?;

        for event_key in removals {
            transaction.execute(
                "DELETE FROM artifact_events
                 WHERE artifact_id = ?1
                   AND event_id IN (
                       SELECT id FROM usage_events
                       WHERE source_id = ?2 AND event_key = ?3
                   )",
                params![artifact_id, source_id, event_key],
            )?;
        }
        let mut upsert_changed = false;
        for event in upserts {
            validate_event(event)?;
            upsert_changed |= !event_is_unchanged(&transaction, source_id, event)?;
            let event_id = upsert_event(&transaction, source_id, event)?;
            transaction.execute(
                "INSERT OR IGNORE INTO artifact_events (artifact_id, event_id)
                 VALUES (?1, ?2)",
                params![artifact_id, event_id],
            )?;
        }
        let removed_events = transaction.execute(
            "DELETE FROM usage_events
             WHERE source_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM artifact_events
                   WHERE artifact_events.event_id = usage_events.id
               )",
            [source_id],
        )?;
        transaction.execute(
            "UPDATE sources
             SET last_sync_ms = ?2, last_error = NULL
             WHERE id = ?1",
            params![source_id, artifact.scanned_at_ms],
        )?;

        let changed = upsert_changed || removed_events > 0;
        if changed {
            transaction.execute(
                "UPDATE index_state SET generation = generation + 1 WHERE id = 1",
                [],
            )?;
        }
        let generation = transaction.query_row(
            "SELECT generation FROM index_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;

        let (start_ms, end_ms) = if changed {
            merge_bounds(old_bounds, event_bounds(upserts))
        } else {
            (None, None)
        };
        Ok(IndexChange {
            generation,
            source_id,
            start_ms,
            end_ms,
            event_count: upserts.len(),
        })
    }

    pub fn mark_source_error(&self, source_id: i64, message: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE sources SET last_error = ?2 WHERE id = ?1",
            params![source_id, message],
        )?;
        Ok(())
    }

    pub fn artifact_keys(&self, source_id: i64) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT artifact_key FROM artifacts WHERE source_id = ?1 ORDER BY artifact_key",
        )?;
        let rows = statement.query_map([source_id], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("listing source artifacts")
    }

    pub fn remove_artifact(
        &mut self,
        source_id: i64,
        artifact_key: &str,
    ) -> Result<Option<IndexChange>> {
        let transaction = self.connection.transaction()?;
        let artifact_id = transaction
            .query_row(
                "SELECT id FROM artifacts WHERE source_id = ?1 AND artifact_key = ?2",
                params![source_id, artifact_key],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let Some(artifact_id) = artifact_id else {
            return Ok(None);
        };
        let old_bounds = artifact_event_bounds(&transaction, artifact_id)?;
        transaction.execute("DELETE FROM artifacts WHERE id = ?1", [artifact_id])?;
        let removed_events = transaction.execute(
            "DELETE FROM usage_events
             WHERE source_id = ?1
               AND NOT EXISTS (
                   SELECT 1 FROM artifact_events
                   WHERE artifact_events.event_id = usage_events.id
               )",
            [source_id],
        )?;
        if removed_events > 0 {
            transaction.execute(
                "UPDATE index_state SET generation = generation + 1 WHERE id = 1",
                [],
            )?;
        }
        let generation = transaction.query_row(
            "SELECT generation FROM index_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        let (start_ms, end_ms) = if removed_events > 0 {
            merge_bounds(old_bounds, None)
        } else {
            (None, None)
        };
        Ok(Some(IndexChange {
            generation,
            source_id,
            start_ms,
            end_ms,
            event_count: 0,
        }))
    }

    pub fn diagnostics(&self) -> Result<IndexDiagnostics> {
        Ok(IndexDiagnostics {
            path: self.path.clone(),
            sqlite_version: self
                .connection
                .query_row("SELECT sqlite_version()", [], |row| row.get(0))?,
            schema_version: schema_version(&self.connection)?,
            generation: self.connection.query_row(
                "SELECT generation FROM index_state WHERE id = 1",
                [],
                |row| row.get(0),
            )?,
            sources: table_count(&self.connection, "sources")?,
            artifacts: table_count(&self.connection, "artifacts")?,
            events: table_count(&self.connection, "usage_events")?,
        })
    }
}

pub fn project_id_for_worktree(worktree: &Path) -> String {
    let normalized = worktree
        .canonicalize()
        .unwrap_or_else(|_| worktree.to_path_buf());
    format!(
        "worktree:{}",
        blake3::hash(normalized.to_string_lossy().as_bytes()).to_hex()
    )
}

fn migrate(connection: &Connection) -> Result<()> {
    let version = schema_version(connection)?;
    if version > SCHEMA_VERSION {
        return Err(anyhow!(
            "usage index schema {version} is newer than supported schema {SCHEMA_VERSION}"
        ));
    }
    if version == 0 {
        connection.execute_batch(
            r#"
            BEGIN IMMEDIATE;
            CREATE TABLE index_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                generation INTEGER NOT NULL DEFAULT 0 CHECK (generation >= 0)
            ) STRICT;
            INSERT INTO index_state (id, generation) VALUES (1, 0);

            CREATE TABLE sources (
                id INTEGER PRIMARY KEY,
                kind TEXT NOT NULL,
                source_key TEXT NOT NULL,
                display_name TEXT NOT NULL,
                last_sync_ms INTEGER,
                last_error TEXT,
                UNIQUE (kind, source_key)
            ) STRICT;

            CREATE TABLE projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                worktree TEXT NOT NULL
            ) STRICT;
            CREATE UNIQUE INDEX projects_worktree ON projects(worktree);

            CREATE TABLE source_projects (
                source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                native_id TEXT NOT NULL,
                project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
                PRIMARY KEY (source_id, native_id)
            ) STRICT, WITHOUT ROWID;

            CREATE TABLE artifacts (
                id INTEGER PRIMARY KEY,
                source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                artifact_key TEXT NOT NULL,
                path TEXT,
                device INTEGER,
                inode INTEGER,
                size INTEGER,
                modified_ns INTEGER,
                parsed_offset INTEGER NOT NULL DEFAULT 0 CHECK (parsed_offset >= 0),
                boundary_hash BLOB,
                full_hash BLOB,
                cursor TEXT,
                parser_version INTEGER NOT NULL CHECK (parser_version >= 0),
                last_scanned_ms INTEGER NOT NULL,
                UNIQUE (source_id, artifact_key)
            ) STRICT;

            CREATE TABLE usage_events (
                id INTEGER PRIMARY KEY,
                source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                event_key BLOB NOT NULL,
                occurred_at_ms INTEGER NOT NULL,
                project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
                provider TEXT NOT NULL,
                model TEXT NOT NULL,
                variant TEXT NOT NULL,
                messages INTEGER NOT NULL CHECK (messages >= 0),
                input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
                output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
                cache_read_tokens INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
                cache_write_tokens INTEGER NOT NULL CHECK (cache_write_tokens >= 0),
                reasoning_tokens INTEGER NOT NULL CHECK (reasoning_tokens >= 0),
                total_tokens INTEGER NOT NULL CHECK (total_tokens >= 0),
                cost_microusd INTEGER,
                cost_kind TEXT NOT NULL CHECK (
                    cost_kind IN ('reported', 'estimated', 'unavailable')
                ),
                UNIQUE (source_id, event_key)
            ) STRICT;
            CREATE INDEX usage_events_time ON usage_events(occurred_at_ms);
            CREATE INDEX usage_events_source_time ON usage_events(source_id, occurred_at_ms);
            CREATE INDEX usage_events_project_time ON usage_events(project_id, occurred_at_ms);
            CREATE INDEX usage_events_model_time
                ON usage_events(provider, model, variant, occurred_at_ms);

            CREATE TABLE artifact_events (
                artifact_id INTEGER NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
                event_id INTEGER NOT NULL REFERENCES usage_events(id) ON DELETE CASCADE,
                PRIMARY KEY (artifact_id, event_id)
            ) STRICT, WITHOUT ROWID;

            CREATE TABLE usage_buckets (
                id INTEGER PRIMARY KEY,
                source_id INTEGER NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                bucket_key BLOB NOT NULL,
                scope TEXT NOT NULL CHECK (scope IN ('local', 'account')),
                granularity TEXT NOT NULL,
                start_ms INTEGER NOT NULL,
                end_ms INTEGER NOT NULL CHECK (end_ms > start_ms),
                project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
                provider TEXT,
                model TEXT,
                messages INTEGER NOT NULL DEFAULT 0 CHECK (messages >= 0),
                input_tokens INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
                output_tokens INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
                cache_read_tokens INTEGER NOT NULL DEFAULT 0 CHECK (cache_read_tokens >= 0),
                cache_write_tokens INTEGER NOT NULL DEFAULT 0 CHECK (cache_write_tokens >= 0),
                reasoning_tokens INTEGER NOT NULL DEFAULT 0 CHECK (reasoning_tokens >= 0),
                total_tokens INTEGER NOT NULL DEFAULT 0 CHECK (total_tokens >= 0),
                cost_microusd INTEGER,
                cost_kind TEXT NOT NULL CHECK (
                    cost_kind IN ('reported', 'estimated', 'unavailable')
                ),
                UNIQUE (source_id, bucket_key)
            ) STRICT;
            CREATE INDEX usage_buckets_range ON usage_buckets(start_ms, end_ms);

            PRAGMA user_version = 2;
            COMMIT;
            "#,
        )?;
    } else if version == 1 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE artifacts ADD COLUMN cursor TEXT;
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    Ok(())
}

fn schema_version(connection: &Connection) -> Result<i64> {
    connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("reading usage index schema version")
}

fn upsert_artifact(
    transaction: &Transaction<'_>,
    source_id: i64,
    artifact: &ArtifactRecord,
) -> Result<i64> {
    transaction
        .query_row(
            "INSERT INTO artifacts (
                source_id, artifact_key, path, device, inode, size, modified_ns,
                parsed_offset, boundary_hash, full_hash, cursor, parser_version, last_scanned_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(source_id, artifact_key) DO UPDATE SET
                path = excluded.path,
                device = excluded.device,
                inode = excluded.inode,
                size = excluded.size,
                modified_ns = excluded.modified_ns,
                parsed_offset = excluded.parsed_offset,
                boundary_hash = excluded.boundary_hash,
                full_hash = excluded.full_hash,
                cursor = excluded.cursor,
                parser_version = excluded.parser_version,
                last_scanned_ms = excluded.last_scanned_ms
             RETURNING id",
            params![
                source_id,
                artifact.key,
                artifact.path,
                artifact.device,
                artifact.inode,
                artifact.size,
                artifact.modified_ns,
                artifact.parsed_offset,
                artifact.boundary_hash,
                artifact.full_hash,
                artifact.cursor,
                artifact.parser_version,
                artifact.scanned_at_ms,
            ],
            |row| row.get(0),
        )
        .context("upserting source artifact")
}

fn upsert_event(transaction: &Transaction<'_>, source_id: i64, event: &UsageEvent) -> Result<i64> {
    transaction
        .query_row(
            "INSERT INTO usage_events (
                source_id, event_key, occurred_at_ms, project_id, provider, model,
                variant, messages, input_tokens, output_tokens, cache_read_tokens,
                cache_write_tokens, reasoning_tokens, total_tokens, cost_microusd,
                cost_kind
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16
             )
             ON CONFLICT(source_id, event_key) DO UPDATE SET
                occurred_at_ms = excluded.occurred_at_ms,
                project_id = excluded.project_id,
                provider = excluded.provider,
                model = excluded.model,
                variant = excluded.variant,
                messages = excluded.messages,
                input_tokens = excluded.input_tokens,
                output_tokens = excluded.output_tokens,
                cache_read_tokens = excluded.cache_read_tokens,
                cache_write_tokens = excluded.cache_write_tokens,
                reasoning_tokens = excluded.reasoning_tokens,
                total_tokens = excluded.total_tokens,
                cost_microusd = excluded.cost_microusd,
                cost_kind = excluded.cost_kind
             RETURNING id",
            params![
                source_id,
                event.event_key,
                event.occurred_at_ms,
                event.project_id,
                event.provider,
                event.model,
                event.variant,
                event.messages,
                event.input_tokens,
                event.output_tokens,
                event.cache_read_tokens,
                event.cache_write_tokens,
                event.reasoning_tokens,
                event.total_tokens,
                event.cost_microusd,
                event.cost_kind.key(),
            ],
            |row| row.get(0),
        )
        .context("upserting usage event")
}

fn event_is_unchanged(
    transaction: &Transaction<'_>,
    source_id: i64,
    event: &UsageEvent,
) -> Result<bool> {
    transaction
        .query_row(
            "SELECT EXISTS (
                SELECT 1 FROM usage_events
                WHERE source_id = ?1
                  AND event_key = ?2
                  AND occurred_at_ms = ?3
                  AND project_id IS ?4
                  AND provider = ?5
                  AND model = ?6
                  AND variant = ?7
                  AND messages = ?8
                  AND input_tokens = ?9
                  AND output_tokens = ?10
                  AND cache_read_tokens = ?11
                  AND cache_write_tokens = ?12
                  AND reasoning_tokens = ?13
                  AND total_tokens = ?14
                  AND cost_microusd IS ?15
                  AND cost_kind = ?16
             )",
            params![
                source_id,
                event.event_key,
                event.occurred_at_ms,
                event.project_id,
                event.provider,
                event.model,
                event.variant,
                event.messages,
                event.input_tokens,
                event.output_tokens,
                event.cache_read_tokens,
                event.cache_write_tokens,
                event.reasoning_tokens,
                event.total_tokens,
                event.cost_microusd,
                event.cost_kind.key(),
            ],
            |row| row.get(0),
        )
        .context("comparing indexed usage event")
}

fn validate_event(event: &UsageEvent) -> Result<()> {
    if event.event_key.is_empty() {
        return Err(anyhow!("usage event key must not be empty"));
    }
    for (name, value) in [
        ("messages", event.messages),
        ("input tokens", event.input_tokens),
        ("output tokens", event.output_tokens),
        ("cache read tokens", event.cache_read_tokens),
        ("cache write tokens", event.cache_write_tokens),
        ("reasoning tokens", event.reasoning_tokens),
        ("total tokens", event.total_tokens),
    ] {
        if value < 0 {
            return Err(anyhow!("usage event {name} must not be negative"));
        }
    }
    Ok(())
}

fn artifact_event_bounds(
    transaction: &Transaction<'_>,
    artifact_id: i64,
) -> Result<Option<(i64, i64)>> {
    transaction
        .query_row(
            "SELECT MIN(usage_events.occurred_at_ms), MAX(usage_events.occurred_at_ms)
             FROM usage_events
             JOIN artifact_events ON artifact_events.event_id = usage_events.id
             WHERE artifact_events.artifact_id = ?1",
            [artifact_id],
            |row| {
                let start: Option<i64> = row.get(0)?;
                let end: Option<i64> = row.get(1)?;
                Ok(start.zip(end))
            },
        )
        .context("reading previous artifact event range")
}

fn removed_event_bounds(
    transaction: &Transaction<'_>,
    artifact_id: i64,
    event_keys: &[Vec<u8>],
) -> Result<Option<(i64, i64)>> {
    let mut bounds: Option<(i64, i64)> = None;
    for event_key in event_keys {
        let event_time = transaction
            .query_row(
                "SELECT usage_events.occurred_at_ms
                 FROM usage_events
                 JOIN artifact_events ON artifact_events.event_id = usage_events.id
                 WHERE artifact_events.artifact_id = ?1
                   AND usage_events.event_key = ?2",
                params![artifact_id, event_key],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(event_time) = event_time {
            bounds = Some(match bounds {
                Some((start, end)) => (start.min(event_time), end.max(event_time)),
                None => (event_time, event_time),
            });
        }
    }
    Ok(bounds)
}

fn event_bounds(events: &[UsageEvent]) -> Option<(i64, i64)> {
    let start = events.iter().map(|event| event.occurred_at_ms).min()?;
    let end = events.iter().map(|event| event.occurred_at_ms).max()?;
    Some((start, end))
}

fn merge_bounds(left: Option<(i64, i64)>, right: Option<(i64, i64)>) -> (Option<i64>, Option<i64>) {
    match (left, right) {
        (Some((left_start, left_end)), Some((right_start, right_end))) => (
            Some(left_start.min(right_start)),
            Some(left_end.max(right_end).saturating_add(1)),
        ),
        (Some((start, end)), None) | (None, Some((start, end))) => {
            (Some(start), Some(end.saturating_add(1)))
        }
        (None, None) => (None, None),
    }
}

fn table_count(connection: &Connection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    connection
        .query_row(&sql, [], |row| row.get(0))
        .with_context(|| format!("counting {table}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> (tempfile::TempDir, UsageIndex) {
        let directory = tempfile::tempdir().unwrap();
        let index = UsageIndex::open(&directory.path().join("usage.sqlite3")).unwrap();
        (directory, index)
    }

    fn source(index: &UsageIndex) -> i64 {
        index
            .register_source(&SourceRegistration {
                kind: SourceKind::Pi,
                source_key: "default".to_string(),
                display_name: "Pi".to_string(),
            })
            .unwrap()
    }

    fn artifact(key: &str) -> ArtifactRecord {
        ArtifactRecord {
            key: key.to_string(),
            path: Some(format!("/sessions/{key}.jsonl")),
            device: Some(1),
            inode: Some(2),
            size: Some(100),
            modified_ns: Some(200),
            parsed_offset: 100,
            boundary_hash: Some(vec![1, 2, 3]),
            full_hash: None,
            cursor: None,
            parser_version: 1,
            scanned_at_ms: 1_000,
        }
    }

    fn event(key: &[u8], occurred_at_ms: i64) -> UsageEvent {
        UsageEvent {
            event_key: key.to_vec(),
            occurred_at_ms,
            project_id: None,
            provider: "openai".to_string(),
            model: "gpt-test".to_string(),
            variant: "default".to_string(),
            messages: 1,
            input_tokens: 10,
            output_tokens: 20,
            cache_read_tokens: 30,
            cache_write_tokens: 0,
            reasoning_tokens: 5,
            total_tokens: 60,
            cost_microusd: None,
            cost_kind: CostKind::Unavailable,
        }
    }

    #[test]
    fn initializes_versioned_wal_index() {
        let (_directory, index) = index();
        let diagnostics = index.diagnostics().unwrap();

        assert_eq!(diagnostics.schema_version, 2);
        assert_eq!(diagnostics.generation, 0);
        assert_eq!(diagnostics.events, 0);
        let journal_mode: String = index
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn records_artifact_checkpoint_and_normalized_events() {
        let (_directory, mut index) = index();
        let source_id = source(&index);

        let change = index
            .replace_artifact_events(source_id, &artifact("one"), &[event(b"event-1", 50)])
            .unwrap();
        let checkpoint = index
            .artifact_checkpoint(source_id, "one")
            .unwrap()
            .unwrap();

        assert_eq!(change.generation, 1);
        assert_eq!(change.start_ms, Some(50));
        assert_eq!(change.end_ms, Some(51));
        assert_eq!(checkpoint.parsed_offset, 100);
        assert_eq!(checkpoint.boundary_hash, Some(vec![1, 2, 3]));
        assert_eq!(index.diagnostics().unwrap().events, 1);
    }

    #[test]
    fn deduplicates_events_copied_between_artifacts() {
        let (_directory, mut index) = index();
        let source_id = source(&index);
        let copied = event(b"copied-entry", 50);

        index
            .replace_artifact_events(
                source_id,
                &artifact("parent"),
                std::slice::from_ref(&copied),
            )
            .unwrap();
        index
            .replace_artifact_events(source_id, &artifact("branch"), &[copied])
            .unwrap();

        let diagnostics = index.diagnostics().unwrap();
        assert_eq!(diagnostics.artifacts, 2);
        assert_eq!(diagnostics.events, 1);
    }

    #[test]
    fn retracts_only_events_no_longer_referenced_by_any_artifact() {
        let (_directory, mut index) = index();
        let source_id = source(&index);
        let copied = event(b"copied-entry", 50);

        index
            .replace_artifact_events(
                source_id,
                &artifact("parent"),
                std::slice::from_ref(&copied),
            )
            .unwrap();
        index
            .replace_artifact_events(source_id, &artifact("branch"), &[copied])
            .unwrap();
        index
            .replace_artifact_events(source_id, &artifact("parent"), &[])
            .unwrap();
        assert_eq!(index.diagnostics().unwrap().events, 1);

        index
            .replace_artifact_events(source_id, &artifact("branch"), &[])
            .unwrap();
        assert_eq!(index.diagnostics().unwrap().events, 0);
    }

    #[test]
    fn maps_native_projects_to_source_neutral_worktrees() {
        let (_directory, index) = index();
        let source_id = source(&index);
        let worktree = Path::new("/work/project");
        let project = ProjectRecord {
            id: project_id_for_worktree(worktree),
            name: "Project".to_string(),
            worktree: worktree.display().to_string(),
        };

        index
            .upsert_project(source_id, "pi-project-id", &project)
            .unwrap();

        let stored: String = index
            .connection
            .query_row(
                "SELECT project_id FROM source_projects
                 WHERE source_id = ?1 AND native_id = 'pi-project-id'",
                [source_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored, project.id);
    }
}
