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

const SCHEMA_VERSION: i64 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceKind {
    OpenCode,
    Copilot,
    Codex,
    Pi,
    Claude,
}

impl SourceKind {
    pub fn key(self) -> &'static str {
        match self {
            Self::OpenCode => "opencode",
            Self::Copilot => "copilot",
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
    pub cache_write_1h_tokens: i64,
    pub reasoning_tokens: i64,
    /// Source-reported total when available, otherwise the adapter's canonical
    /// non-overlapping total.
    pub total_tokens: i64,
    pub cost_microusd: Option<i64>,
    pub cost_kind: CostKind,
    pub is_sidechain: bool,
    pub has_detailed_cache: bool,
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

    pub fn has_artifacts_for_kind(&self, kind: SourceKind) -> Result<bool> {
        let count: i64 = self.connection.query_row(
            "SELECT COUNT(*) FROM artifacts
             JOIN sources ON sources.id = artifacts.source_id
             WHERE sources.kind = ?1",
            [kind.key()],
            |row| row.get(0),
        )?;
        Ok(count > 0)
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
        let old_keys = artifact_event_keys(&transaction, artifact_id)?;

        transaction.execute(
            "DELETE FROM artifact_events WHERE artifact_id = ?1",
            [artifact_id],
        )?;

        let mut affected_keys: std::collections::HashSet<Vec<u8>> = old_keys.into_iter().collect();
        for event in events {
            validate_event(event)?;
            insert_artifact_event(&transaction, artifact_id, event, false)?;
            affected_keys.insert(event.event_key.clone());
        }

        for event_key in &affected_keys {
            recalculate_canonical_event(&transaction, source_id, event_key)?;
        }

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

        let is_claude: bool = transaction.query_row(
            "SELECT kind = 'claude' FROM sources WHERE id = ?1",
            [source_id],
            |row| row.get(0),
        )?;

        let mut affected_keys: std::collections::HashSet<Vec<u8>> =
            std::collections::HashSet::new();

        for event_key in removals {
            let deleted = transaction.execute(
                "DELETE FROM artifact_events
                 WHERE artifact_id = ?1 AND event_key = ?2",
                params![artifact_id, event_key],
            )?;
            if deleted > 0 {
                affected_keys.insert(event_key.clone());
            }
        }

        for event in upserts {
            validate_event(event)?;
            let updated = insert_artifact_event(&transaction, artifact_id, event, is_claude)?;
            if updated {
                affected_keys.insert(event.event_key.clone());
            }
        }

        for event_key in &affected_keys {
            recalculate_canonical_event(&transaction, source_id, event_key)?;
        }

        transaction.execute(
            "UPDATE sources
             SET last_sync_ms = ?2, last_error = NULL
             WHERE id = ?1",
            params![source_id, artifact.scanned_at_ms],
        )?;

        let changed = !affected_keys.is_empty();
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
        let old_keys = artifact_event_keys(&transaction, artifact_id)?;

        transaction.execute("DELETE FROM artifacts WHERE id = ?1", [artifact_id])?;

        for event_key in &old_keys {
            recalculate_canonical_event(&transaction, source_id, event_key)?;
        }

        let changed = !old_keys.is_empty();
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
        let sources = table_count(&self.connection, "sources")?;
        let artifacts = table_count(&self.connection, "artifacts")?;
        let events = table_count(&self.connection, "usage_events")?;
        let schema_version = schema_version(&self.connection)?;
        let generation = self.connection.query_row(
            "SELECT generation FROM index_state WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        let sqlite_version = self
            .connection
            .query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
        Ok(IndexDiagnostics {
            path: self.path.clone(),
            sqlite_version,
            schema_version,
            generation,
            sources,
            artifacts,
            events,
        })
    }
}

fn table_count(connection: &Connection, table: &str) -> Result<i64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    connection
        .query_row(&sql, [], |row| row.get(0))
        .with_context(|| format!("counting {table}"))
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
            "usage index schema version {} is newer than supported version {}",
            version,
            SCHEMA_VERSION
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
                cache_write_1h_tokens INTEGER NOT NULL DEFAULT 0 CHECK (
                    cache_write_1h_tokens >= 0 AND cache_write_1h_tokens <= cache_write_tokens
                ),
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
                cache_write_1h_tokens INTEGER NOT NULL DEFAULT 0 CHECK (
                    cache_write_1h_tokens >= 0 AND cache_write_1h_tokens <= cache_write_tokens
                ),
                reasoning_tokens INTEGER NOT NULL CHECK (reasoning_tokens >= 0),
                total_tokens INTEGER NOT NULL CHECK (total_tokens >= 0),
                cost_microusd INTEGER,
                cost_kind TEXT NOT NULL CHECK (
                    cost_kind IN ('reported', 'estimated', 'unavailable')
                ),
                is_sidechain INTEGER NOT NULL DEFAULT 0 CHECK (is_sidechain IN (0, 1)),
                has_detailed_cache INTEGER NOT NULL DEFAULT 0 CHECK (has_detailed_cache IN (0, 1)),
                PRIMARY KEY (artifact_id, event_key)
            ) STRICT, WITHOUT ROWID;
            CREATE INDEX artifact_events_key ON artifact_events(event_key);

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
                cache_write_1h_tokens INTEGER NOT NULL DEFAULT 0 CHECK (
                    cache_write_1h_tokens >= 0 AND cache_write_1h_tokens <= cache_write_tokens
                ),
                reasoning_tokens INTEGER NOT NULL DEFAULT 0 CHECK (reasoning_tokens >= 0),
                total_tokens INTEGER NOT NULL DEFAULT 0 CHECK (total_tokens >= 0),
                cost_microusd INTEGER,
                cost_kind TEXT NOT NULL CHECK (
                    cost_kind IN ('reported', 'estimated', 'unavailable')
                ),
                UNIQUE (source_id, bucket_key)
            ) STRICT;
            CREATE INDEX usage_buckets_range ON usage_buckets(start_ms, end_ms);

            PRAGMA user_version = 4;
            COMMIT;
            "#,
        )?;
    } else {
        if version == 1 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 ALTER TABLE artifacts ADD COLUMN cursor TEXT;
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
        }
        if schema_version(connection)? == 2 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE INDEX IF NOT EXISTS artifact_events_event_id
                     ON artifact_events(event_id);
                 PRAGMA user_version = 3;
                 COMMIT;",
            )?;
        }
        if schema_version(connection)? == 3 {
            connection.execute_batch(
                "BEGIN IMMEDIATE;
                 CREATE TABLE artifact_events_new (
                     artifact_id INTEGER NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
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
                     cache_write_1h_tokens INTEGER NOT NULL DEFAULT 0 CHECK (
                         cache_write_1h_tokens >= 0 AND cache_write_1h_tokens <= cache_write_tokens
                     ),
                     reasoning_tokens INTEGER NOT NULL CHECK (reasoning_tokens >= 0),
                     total_tokens INTEGER NOT NULL CHECK (total_tokens >= 0),
                     cost_microusd INTEGER,
                     cost_kind TEXT NOT NULL CHECK (
                         cost_kind IN ('reported', 'estimated', 'unavailable')
                     ),
                     is_sidechain INTEGER NOT NULL DEFAULT 0 CHECK (is_sidechain IN (0, 1)),
                     has_detailed_cache INTEGER NOT NULL DEFAULT 0 CHECK (has_detailed_cache IN (0, 1)),
                     PRIMARY KEY (artifact_id, event_key)
                 ) STRICT, WITHOUT ROWID;

                 INSERT INTO artifact_events_new (
                     artifact_id, event_key, occurred_at_ms, project_id, provider, model,
                     variant, messages, input_tokens, output_tokens, cache_read_tokens,
                     cache_write_tokens, cache_write_1h_tokens, reasoning_tokens, total_tokens,
                     cost_microusd, cost_kind, is_sidechain, has_detailed_cache
                 )
                 SELECT
                     ae.artifact_id, ue.event_key, ue.occurred_at_ms, ue.project_id, ue.provider, ue.model,
                     ue.variant, ue.messages, ue.input_tokens, ue.output_tokens, ue.cache_read_tokens,
                     ue.cache_write_tokens, 0, ue.reasoning_tokens, ue.total_tokens,
                     ue.cost_microusd, ue.cost_kind, 0, 0
                 FROM artifact_events ae
                 JOIN usage_events ue ON ue.id = ae.event_id;

                 DROP TABLE artifact_events;
                 ALTER TABLE artifact_events_new RENAME TO artifact_events;
                 CREATE INDEX artifact_events_key ON artifact_events(event_key);

                 ALTER TABLE usage_events ADD COLUMN cache_write_1h_tokens INTEGER NOT NULL DEFAULT 0 CHECK (
                     cache_write_1h_tokens >= 0 AND cache_write_1h_tokens <= cache_write_tokens
                 );
                 ALTER TABLE usage_buckets ADD COLUMN cache_write_1h_tokens INTEGER NOT NULL DEFAULT 0 CHECK (
                     cache_write_1h_tokens >= 0 AND cache_write_1h_tokens <= cache_write_tokens
                 );
                 PRAGMA user_version = 4;
                 COMMIT;",
            )?;
        }
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

fn insert_artifact_event(
    transaction: &Transaction<'_>,
    artifact_id: i64,
    event: &UsageEvent,
    is_claude: bool,
) -> Result<bool> {
    if is_claude {
        let existing: Option<UsageEvent> = transaction
            .query_row(
                "SELECT occurred_at_ms, project_id, provider, model, variant,
                        messages, input_tokens, output_tokens, cache_read_tokens,
                        cache_write_tokens, cache_write_1h_tokens, reasoning_tokens,
                        total_tokens, cost_microusd, cost_kind, is_sidechain, has_detailed_cache
                 FROM artifact_events
                 WHERE artifact_id = ?1 AND event_key = ?2",
                params![artifact_id, event.event_key],
                |row| {
                    let cost_kind_str: String = row.get(14)?;
                    let cost_kind = match cost_kind_str.as_str() {
                        "reported" => CostKind::Reported,
                        "estimated" => CostKind::Estimated,
                        _ => CostKind::Unavailable,
                    };
                    Ok(UsageEvent {
                        event_key: event.event_key.clone(),
                        occurred_at_ms: row.get(0)?,
                        project_id: row.get(1)?,
                        provider: row.get(2)?,
                        model: row.get(3)?,
                        variant: row.get(4)?,
                        messages: row.get(5)?,
                        input_tokens: row.get(6)?,
                        output_tokens: row.get(7)?,
                        cache_read_tokens: row.get(8)?,
                        cache_write_tokens: row.get(9)?,
                        cache_write_1h_tokens: row.get(10)?,
                        reasoning_tokens: row.get(11)?,
                        total_tokens: row.get(12)?,
                        cost_microusd: row.get(13)?,
                        cost_kind,
                        is_sidechain: row.get::<_, i64>(15)? != 0,
                        has_detailed_cache: row.get::<_, i64>(16)? != 0,
                    })
                },
            )
            .optional()?;

        if let Some(existing_event) = existing {
            let candidates = vec![existing_event.clone(), event.clone()];
            let merged = canonicalize_events(&candidates).unwrap_or_else(|| event.clone());
            if merged == existing_event {
                return Ok(false);
            }
            let rows_affected = transaction.execute(
                "UPDATE artifact_events SET
                    occurred_at_ms = ?3,
                    project_id = ?4,
                    provider = ?5,
                    model = ?6,
                    variant = ?7,
                    messages = ?8,
                    input_tokens = ?9,
                    output_tokens = ?10,
                    cache_read_tokens = ?11,
                    cache_write_tokens = ?12,
                    cache_write_1h_tokens = ?13,
                    reasoning_tokens = ?14,
                    total_tokens = ?15,
                    cost_microusd = ?16,
                    cost_kind = ?17,
                    is_sidechain = ?18,
                    has_detailed_cache = ?19
                 WHERE artifact_id = ?1 AND event_key = ?2",
                params![
                    artifact_id,
                    merged.event_key,
                    merged.occurred_at_ms,
                    merged.project_id,
                    merged.provider,
                    merged.model,
                    merged.variant,
                    merged.messages,
                    merged.input_tokens,
                    merged.output_tokens,
                    merged.cache_read_tokens,
                    merged.cache_write_tokens,
                    merged.cache_write_1h_tokens,
                    merged.reasoning_tokens,
                    merged.total_tokens,
                    merged.cost_microusd,
                    merged.cost_kind.key(),
                    if merged.is_sidechain { 1 } else { 0 },
                    if merged.has_detailed_cache { 1 } else { 0 },
                ],
            )?;
            return Ok(rows_affected > 0);
        }
    }

    let rows_affected = transaction.execute(
        "INSERT INTO artifact_events (
            artifact_id, event_key, occurred_at_ms, project_id, provider, model,
            variant, messages, input_tokens, output_tokens, cache_read_tokens,
            cache_write_tokens, cache_write_1h_tokens, reasoning_tokens, total_tokens,
            cost_microusd, cost_kind, is_sidechain, has_detailed_cache
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
            ?14, ?15, ?16, ?17, ?18, ?19
         )
         ON CONFLICT(artifact_id, event_key) DO UPDATE SET
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
            cache_write_1h_tokens = excluded.cache_write_1h_tokens,
            reasoning_tokens = excluded.reasoning_tokens,
            total_tokens = excluded.total_tokens,
            cost_microusd = excluded.cost_microusd,
            cost_kind = excluded.cost_kind,
            is_sidechain = excluded.is_sidechain,
            has_detailed_cache = excluded.has_detailed_cache
         WHERE
            artifact_events.occurred_at_ms IS NOT excluded.occurred_at_ms
            OR artifact_events.project_id IS NOT excluded.project_id
            OR artifact_events.provider IS NOT excluded.provider
            OR artifact_events.model IS NOT excluded.model
            OR artifact_events.variant IS NOT excluded.variant
            OR artifact_events.messages IS NOT excluded.messages
            OR artifact_events.input_tokens IS NOT excluded.input_tokens
            OR artifact_events.output_tokens IS NOT excluded.output_tokens
            OR artifact_events.cache_read_tokens IS NOT excluded.cache_read_tokens
            OR artifact_events.cache_write_tokens IS NOT excluded.cache_write_tokens
            OR artifact_events.cache_write_1h_tokens IS NOT excluded.cache_write_1h_tokens
            OR artifact_events.reasoning_tokens IS NOT excluded.reasoning_tokens
            OR artifact_events.total_tokens IS NOT excluded.total_tokens
            OR artifact_events.cost_microusd IS NOT excluded.cost_microusd
            OR artifact_events.cost_kind IS NOT excluded.cost_kind
            OR artifact_events.is_sidechain IS NOT excluded.is_sidechain
            OR artifact_events.has_detailed_cache IS NOT excluded.has_detailed_cache",
        params![
            artifact_id,
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
            event.cache_write_1h_tokens,
            event.reasoning_tokens,
            event.total_tokens,
            event.cost_microusd,
            event.cost_kind.key(),
            if event.is_sidechain { 1 } else { 0 },
            if event.has_detailed_cache { 1 } else { 0 },
        ],
    )?;
    Ok(rows_affected > 0)
}

fn canonicalize_events(candidates: &[UsageEvent]) -> Option<UsageEvent> {
    if candidates.is_empty() {
        return None;
    }
    let mut winner = candidates[0].clone();
    for candidate in &candidates[1..] {
        let prefer_candidate = if candidate.total_tokens != winner.total_tokens {
            candidate.total_tokens > winner.total_tokens
        } else if candidate.has_detailed_cache != winner.has_detailed_cache {
            candidate.has_detailed_cache
        } else if candidate.is_sidechain != winner.is_sidechain {
            !candidate.is_sidechain
        } else {
            // SQLite does not promise row order. Keep equal-priority snapshots
            // stable when their artifact insertion order changes.
            event_stable_key(candidate) < event_stable_key(&winner)
        };

        if prefer_candidate {
            winner = candidate.clone();
        }
    }

    let reported_candidates: Vec<&UsageEvent> = candidates
        .iter()
        .filter(|c| c.cost_kind == CostKind::Reported && c.cost_microusd.is_some())
        .collect();

    if !reported_candidates.is_empty() {
        if winner.cost_kind == CostKind::Reported && winner.cost_microusd.is_some() {
            // winner already has a reported cost, retain it
        } else {
            let best_reported = reported_candidates
                .iter()
                .max_by(|left, right| {
                    left.cost_microusd
                        .cmp(&right.cost_microusd)
                        .then_with(|| left.total_tokens.cmp(&right.total_tokens))
                        .then_with(|| right.occurred_at_ms.cmp(&left.occurred_at_ms))
                        .then_with(|| event_stable_key(left).cmp(&event_stable_key(right)))
                })
                .unwrap();
            winner.cost_microusd = best_reported.cost_microusd;
            winner.cost_kind = CostKind::Reported;
        }
    }

    let is_bedrock = candidates.iter().any(|c| c.provider == "amazon-bedrock");
    let project_id = winner.project_id.or_else(|| {
        candidates
            .iter()
            .filter_map(|candidate| candidate.project_id.as_ref())
            .min()
            .cloned()
    });

    if is_bedrock {
        winner.provider = "amazon-bedrock".to_string();
    }
    winner.project_id = project_id;

    Some(winner)
}

type EventStableKey<'a> = (
    (&'a str, &'a str, &'a str, Option<&'a str>),
    (i64, i64, i64, i64, i64, i64, i64, i64),
    (Option<i64>, &'a str, bool, bool),
);

fn event_stable_key(event: &UsageEvent) -> EventStableKey<'_> {
    (
        (
            &event.provider,
            &event.model,
            &event.variant,
            event.project_id.as_deref(),
        ),
        (
            event.occurred_at_ms,
            event.messages,
            event.input_tokens,
            event.output_tokens,
            event.cache_read_tokens,
            event.cache_write_tokens,
            event.cache_write_1h_tokens,
            event.reasoning_tokens,
        ),
        (
            event.cost_microusd,
            event.cost_kind.key(),
            event.is_sidechain,
            event.has_detailed_cache,
        ),
    )
}

fn recalculate_canonical_event(
    transaction: &Transaction<'_>,
    source_id: i64,
    event_key: &[u8],
) -> Result<()> {
    let mut stmt = transaction.prepare(
        "SELECT ae.occurred_at_ms, ae.project_id, ae.provider, ae.model, ae.variant,
                ae.messages, ae.input_tokens, ae.output_tokens, ae.cache_read_tokens,
                ae.cache_write_tokens, ae.cache_write_1h_tokens, ae.reasoning_tokens,
                ae.total_tokens, ae.cost_microusd, ae.cost_kind, ae.is_sidechain, ae.has_detailed_cache
         FROM artifact_events ae
         JOIN artifacts a ON a.id = ae.artifact_id
         WHERE a.source_id = ?1 AND ae.event_key = ?2",
    )?;
    let candidates: Vec<UsageEvent> = stmt
        .query_map(params![source_id, event_key], |row| {
            let cost_kind_str: String = row.get(14)?;
            let cost_kind = match cost_kind_str.as_str() {
                "reported" => CostKind::Reported,
                "estimated" => CostKind::Estimated,
                _ => CostKind::Unavailable,
            };
            Ok(UsageEvent {
                event_key: event_key.to_vec(),
                occurred_at_ms: row.get(0)?,
                project_id: row.get(1)?,
                provider: row.get(2)?,
                model: row.get(3)?,
                variant: row.get(4)?,
                messages: row.get(5)?,
                input_tokens: row.get(6)?,
                output_tokens: row.get(7)?,
                cache_read_tokens: row.get(8)?,
                cache_write_tokens: row.get(9)?,
                cache_write_1h_tokens: row.get(10)?,
                reasoning_tokens: row.get(11)?,
                total_tokens: row.get(12)?,
                cost_microusd: row.get(13)?,
                cost_kind,
                is_sidechain: row.get::<_, i64>(15)? != 0,
                has_detailed_cache: row.get::<_, i64>(16)? != 0,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    if candidates.is_empty() {
        transaction.execute(
            "DELETE FROM usage_events WHERE source_id = ?1 AND event_key = ?2",
            params![source_id, event_key],
        )?;
    } else if let Some(winner) = canonicalize_events(&candidates) {
        upsert_canonical_usage_event(transaction, source_id, &winner)?;
    }
    Ok(())
}

fn upsert_canonical_usage_event(
    transaction: &Transaction<'_>,
    source_id: i64,
    event: &UsageEvent,
) -> Result<i64> {
    transaction
        .query_row(
            "INSERT INTO usage_events (
                source_id, event_key, occurred_at_ms, project_id, provider, model,
                variant, messages, input_tokens, output_tokens, cache_read_tokens,
                cache_write_tokens, cache_write_1h_tokens, reasoning_tokens, total_tokens, cost_microusd,
                cost_kind
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17
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
                cache_write_1h_tokens = excluded.cache_write_1h_tokens,
                reasoning_tokens = excluded.reasoning_tokens,
                total_tokens = excluded.total_tokens,
                cost_microusd = excluded.cost_microusd,
                cost_kind = excluded.cost_kind
             WHERE
                usage_events.occurred_at_ms IS NOT excluded.occurred_at_ms
                OR usage_events.project_id IS NOT excluded.project_id
                OR usage_events.provider IS NOT excluded.provider
                OR usage_events.model IS NOT excluded.model
                OR usage_events.variant IS NOT excluded.variant
                OR usage_events.messages IS NOT excluded.messages
                OR usage_events.input_tokens IS NOT excluded.input_tokens
                OR usage_events.output_tokens IS NOT excluded.output_tokens
                OR usage_events.cache_read_tokens IS NOT excluded.cache_read_tokens
                OR usage_events.cache_write_tokens IS NOT excluded.cache_write_tokens
                OR usage_events.cache_write_1h_tokens IS NOT excluded.cache_write_1h_tokens
                OR usage_events.reasoning_tokens IS NOT excluded.reasoning_tokens
                OR usage_events.total_tokens IS NOT excluded.total_tokens
                OR usage_events.cost_microusd IS NOT excluded.cost_microusd
                OR usage_events.cost_kind IS NOT excluded.cost_kind
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
                event.cache_write_1h_tokens,
                event.reasoning_tokens,
                event.total_tokens,
                event.cost_microusd,
                event.cost_kind.key(),
            ],
            |row| row.get(0),
        )
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => transaction.query_row(
                "SELECT id FROM usage_events WHERE source_id = ?1 AND event_key = ?2",
                params![source_id, event.event_key],
                |row| row.get(0),
            ),
            other => Err(other),
        })
        .context("upserting canonical usage event")
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
        ("cache write 1h tokens", event.cache_write_1h_tokens),
        ("reasoning tokens", event.reasoning_tokens),
        ("total tokens", event.total_tokens),
    ] {
        if value < 0 {
            return Err(anyhow!("usage event {name} must not be negative"));
        }
    }
    if event.cache_write_1h_tokens > event.cache_write_tokens {
        return Err(anyhow!(
            "cache write 1h tokens ({}) must not exceed cache write tokens ({})",
            event.cache_write_1h_tokens,
            event.cache_write_tokens
        ));
    }
    Ok(())
}

fn artifact_event_bounds(
    transaction: &Transaction<'_>,
    artifact_id: i64,
) -> Result<Option<(i64, i64)>> {
    transaction
        .query_row(
            "SELECT MIN(occurred_at_ms), MAX(occurred_at_ms)
             FROM artifact_events
             WHERE artifact_id = ?1",
            [artifact_id],
            |row| {
                let start: Option<i64> = row.get(0)?;
                let end: Option<i64> = row.get(1)?;
                Ok(start.zip(end))
            },
        )
        .context("reading previous artifact event range")
}

fn artifact_event_keys(transaction: &Transaction<'_>, artifact_id: i64) -> Result<Vec<Vec<u8>>> {
    let mut stmt =
        transaction.prepare("SELECT event_key FROM artifact_events WHERE artifact_id = ?1")?;
    let rows = stmt.query_map([artifact_id], |row| row.get(0))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("listing artifact event keys")
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
                "SELECT occurred_at_ms
                 FROM artifact_events
                 WHERE artifact_id = ?1
                   AND event_key = ?2",
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
            cache_write_1h_tokens: 0,
            reasoning_tokens: 5,
            total_tokens: 60,
            cost_microusd: None,
            cost_kind: CostKind::Unavailable,
            is_sidechain: false,
            has_detailed_cache: false,
        }
    }

    #[test]
    fn initializes_versioned_wal_index() {
        let (_directory, index) = index();
        let diagnostics = index.diagnostics().unwrap();

        assert_eq!(diagnostics.schema_version, 4);
        assert_eq!(diagnostics.generation, 0);
        assert_eq!(diagnostics.events, 0);
        let journal_mode: String = index
            .connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "wal");
    }

    #[test]
    fn migrates_version_two_indexes_for_reverse_artifact_lookups() {
        let (_directory, index) = index();
        index
            .connection
            .execute_batch(
                "DROP TABLE artifact_events;
                 DROP TABLE usage_events;
                 DROP TABLE usage_buckets;
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
                 PRAGMA user_version = 2;",
            )
            .unwrap();

        migrate(&index.connection).unwrap();

        assert_eq!(schema_version(&index.connection).unwrap(), 4);
        let index_exists: bool = index
            .connection
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'index' AND name = 'artifact_events_key'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(index_exists);
    }

    #[test]
    fn migrates_version_three_indexes_for_cache_durations() {
        let (_directory, index) = index();
        index
            .connection
            .execute_batch(
                "DROP TABLE artifact_events;
                 DROP TABLE usage_events;
                 DROP TABLE usage_buckets;
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
                 CREATE TABLE artifact_events (
                     artifact_id INTEGER NOT NULL REFERENCES artifacts(id) ON DELETE CASCADE,
                     event_id INTEGER NOT NULL REFERENCES usage_events(id) ON DELETE CASCADE,
                     PRIMARY KEY (artifact_id, event_id)
                 ) STRICT, WITHOUT ROWID;
                 CREATE INDEX artifact_events_event_id ON artifact_events(event_id);
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
                 PRAGMA user_version = 3;",
            )
            .unwrap();

        let source_id = source(&index);
        index
            .connection
            .execute(
                "INSERT INTO artifacts (
                    source_id, artifact_key, path, device, inode, size, modified_ns,
                    parsed_offset, boundary_hash, full_hash, cursor, parser_version, last_scanned_ms
                 ) VALUES (
                    ?1, 'v3-art', '/sessions/v3-art.jsonl', 1, 2, 120, 300,
                    100, X'0102', X'0304', 'checkpoint-cursor', 7, 1000
                 )",
                params![source_id],
            )
            .unwrap();

        migrate(&index.connection).unwrap();

        assert_eq!(schema_version(&index.connection).unwrap(), 4);
        let column_exists: bool = index
            .connection
            .query_row(
                "SELECT COUNT(*) > 0 FROM pragma_table_info('usage_events') WHERE name = 'cache_write_1h_tokens'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(column_exists);
        let checkpoint = index
            .artifact_checkpoint(source_id, "v3-art")
            .unwrap()
            .unwrap();
        assert_eq!(checkpoint.device, Some(1));
        assert_eq!(checkpoint.inode, Some(2));
        assert_eq!(checkpoint.size, Some(120));
        assert_eq!(checkpoint.modified_ns, Some(300));
        assert_eq!(checkpoint.parsed_offset, 100);
        assert_eq!(checkpoint.boundary_hash, Some(vec![1, 2]));
        assert_eq!(checkpoint.full_hash, Some(vec![3, 4]));
        assert_eq!(checkpoint.cursor.as_deref(), Some("checkpoint-cursor"));
        assert_eq!(checkpoint.parser_version, 7);
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
    fn incremental_changes_do_not_rewrite_identical_events() {
        let (_directory, mut index) = index();
        let source_id = source(&index);
        let existing = event(b"event-1", 50);
        index
            .replace_artifact_events(source_id, &artifact("one"), std::slice::from_ref(&existing))
            .unwrap();
        index
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_usage_event_update
                 BEFORE UPDATE ON usage_events
                 BEGIN
                     SELECT RAISE(FAIL, 'unexpected event rewrite');
                 END;",
            )
            .unwrap();

        let change = index
            .apply_artifact_changes(
                source_id,
                &artifact("one"),
                std::slice::from_ref(&existing),
                &[],
            )
            .unwrap();

        assert_eq!(change.generation, 1);
        assert_eq!(change.start_ms, None);
        assert_eq!(change.end_ms, None);
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
    fn canonicalization_prefers_larger_tokens_regardless_of_insertion_order() {
        let (_directory, mut index1) = index();
        let source1 = source(&index1);
        let mut smaller = event(b"msg_1", 100);
        smaller.total_tokens = 50;
        let mut larger = event(b"msg_1", 100);
        larger.total_tokens = 100;

        // Insert smaller first, then larger
        index1
            .replace_artifact_events(source1, &artifact("art_a"), &[smaller.clone()])
            .unwrap();
        index1
            .replace_artifact_events(source1, &artifact("art_b"), &[larger.clone()])
            .unwrap();

        let total: i64 = index1
            .connection
            .query_row(
                "SELECT total_tokens FROM usage_events WHERE source_id = ?1",
                [source1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, 100);

        // Insert larger first, then smaller
        let (_directory2, mut index2) = index();
        let source2 = source(&index2);
        index2
            .replace_artifact_events(source2, &artifact("art_b"), &[larger])
            .unwrap();
        index2
            .replace_artifact_events(source2, &artifact("art_a"), &[smaller])
            .unwrap();

        let total2: i64 = index2
            .connection
            .query_row(
                "SELECT total_tokens FROM usage_events WHERE source_id = ?1",
                [source2],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total2, 100);
    }

    #[test]
    fn canonicalization_ties_are_independent_of_candidate_order() {
        let mut first = event(b"msg_1", 100);
        first.provider = "provider-z".to_string();
        first.model = "model-z".to_string();
        first.total_tokens = 100;
        first.has_detailed_cache = true;

        let mut second = first.clone();
        second.provider = "provider-a".to_string();
        second.model = "model-a".to_string();

        let forward = canonicalize_events(&[first.clone(), second.clone()]).unwrap();
        let reverse = canonicalize_events(&[second, first]).unwrap();

        assert_eq!(forward, reverse);
        assert_eq!(forward.provider, "provider-a");
        assert_eq!(forward.model, "model-a");
    }

    #[test]
    fn canonicalization_tie_breakers_and_retention() {
        let (_directory, mut index) = index();
        let source_id = source(&index);

        let mut sidechain = event(b"msg_1", 100);
        sidechain.total_tokens = 100;
        sidechain.is_sidechain = true;
        sidechain.has_detailed_cache = false;
        sidechain.cost_microusd = Some(5000);
        sidechain.cost_kind = CostKind::Reported;
        sidechain.provider = "amazon-bedrock".to_string();

        let mut main_detail = event(b"msg_1", 100);
        main_detail.total_tokens = 100;
        main_detail.is_sidechain = false;
        main_detail.has_detailed_cache = true;
        main_detail.cache_write_tokens = 20;
        main_detail.cache_write_1h_tokens = 10;
        main_detail.cost_microusd = None;
        main_detail.cost_kind = CostKind::Unavailable;
        main_detail.provider = "anthropic".to_string();

        // Detailed cache wins over no detailed cache, non-sidechain wins over sidechain,
        // but reported cost and amazon-bedrock are retained from sidechain
        index
            .replace_artifact_events(source_id, &artifact("sidechain_file"), &[sidechain])
            .unwrap();
        index
            .replace_artifact_events(source_id, &artifact("main_file"), &[main_detail])
            .unwrap();

        let (cost_microusd, cost_kind, provider, cache_1h, total): (
            Option<i64>,
            String,
            String,
            i64,
            i64,
        ) = index
            .connection
            .query_row(
                "SELECT cost_microusd, cost_kind, provider, cache_write_1h_tokens, total_tokens
                 FROM usage_events WHERE source_id = ?1",
                [source_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(total, 100);
        assert_eq!(cache_1h, 10);
        assert_eq!(cost_microusd, Some(5000));
        assert_eq!(cost_kind, "reported");
        assert_eq!(provider, "amazon-bedrock");

        // When main_file is removed, the remaining sidechain artifact snapshot takes over
        index.remove_artifact(source_id, "main_file").unwrap();

        let (cost_microusd, provider, cache_1h): (Option<i64>, String, i64) = index
            .connection
            .query_row(
                "SELECT cost_microusd, provider, cache_write_1h_tokens
                 FROM usage_events WHERE source_id = ?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(cache_1h, 0); // sidechain didn't have detailed cache
        assert_eq!(cost_microusd, Some(5000));
        assert_eq!(provider, "amazon-bedrock");

        // When sidechain_file is removed, event is deleted
        index.remove_artifact(source_id, "sidechain_file").unwrap();
        assert_eq!(index.diagnostics().unwrap().events, 0);
    }

    #[test]
    fn validates_cache_write_1h_does_not_exceed_total_cache_write() {
        let (_directory, mut index) = index();
        let source_id = source(&index);

        let mut invalid = event(b"invalid_event", 100);
        invalid.cache_write_tokens = 10;
        invalid.cache_write_1h_tokens = 15;

        let result = index.replace_artifact_events(source_id, &artifact("invalid"), &[invalid]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must not exceed"));
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

    fn claude_source(index: &UsageIndex) -> i64 {
        index
            .register_source(&SourceRegistration {
                kind: SourceKind::Claude,
                source_key: "claude-default".to_string(),
                display_name: "Claude Code".to_string(),
            })
            .unwrap()
    }

    #[test]
    fn same_artifact_claude_incremental_append_preserves_cost_and_provider_and_upgrades() {
        let (_directory, mut index) = index();
        let source_id = claude_source(&index);

        let mut initial = event(b"claude_event_1", 100);
        initial.total_tokens = 100;
        initial.cost_microusd = Some(5000);
        initial.cost_kind = CostKind::Reported;
        initial.provider = "amazon-bedrock".to_string();

        let art = artifact("claude_session");
        index
            .apply_artifact_changes(source_id, &art, &[initial], &[])
            .unwrap();

        let (tokens, cost, kind, provider): (i64, Option<i64>, String, String) = index
            .connection
            .query_row(
                "SELECT total_tokens, cost_microusd, cost_kind, provider
                 FROM usage_events WHERE source_id = ?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(tokens, 100);
        assert_eq!(cost, Some(5000));
        assert_eq!(kind, "reported");
        assert_eq!(provider, "amazon-bedrock");

        // Appending a smaller/partial event to the same Claude artifact does NOT downgrade
        let mut partial = event(b"claude_event_1", 100);
        partial.total_tokens = 50;
        partial.cost_microusd = None;
        partial.cost_kind = CostKind::Unavailable;
        partial.provider = "anthropic".to_string();

        index
            .apply_artifact_changes(source_id, &art, &[partial], &[])
            .unwrap();

        let (tokens, cost, kind, provider): (i64, Option<i64>, String, String) = index
            .connection
            .query_row(
                "SELECT total_tokens, cost_microusd, cost_kind, provider
                 FROM usage_events WHERE source_id = ?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(tokens, 100);
        assert_eq!(cost, Some(5000));
        assert_eq!(kind, "reported");
        assert_eq!(provider, "amazon-bedrock");

        // Appending a fuller event upgrades the tokens and cache stats while preserving cost and provider
        let mut fuller = event(b"claude_event_1", 100);
        fuller.total_tokens = 200;
        fuller.cache_write_tokens = 40;
        fuller.cache_write_1h_tokens = 20;
        fuller.has_detailed_cache = true;
        fuller.cost_microusd = None;
        fuller.cost_kind = CostKind::Unavailable;
        fuller.provider = "anthropic".to_string();

        index
            .apply_artifact_changes(source_id, &art, &[fuller], &[])
            .unwrap();

        let (tokens, cache_1h, cost, kind, provider): (i64, i64, Option<i64>, String, String) =
            index
                .connection
                .query_row(
                    "SELECT total_tokens, cache_write_1h_tokens, cost_microusd, cost_kind, provider
                 FROM usage_events WHERE source_id = ?1",
                    [source_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .unwrap();
        assert_eq!(tokens, 200);
        assert_eq!(cache_1h, 20);
        assert_eq!(cost, Some(5000));
        assert_eq!(kind, "reported");
        assert_eq!(provider, "amazon-bedrock");
    }

    #[test]
    fn same_artifact_non_claude_incremental_append_replaces() {
        let (_directory, mut index) = index();
        let source_id = source(&index);

        let mut initial = event(b"pi_event_1", 100);
        initial.total_tokens = 100;
        initial.cost_microusd = Some(2000);
        initial.cost_kind = CostKind::Estimated;

        let art = artifact("pi_session");
        index
            .apply_artifact_changes(source_id, &art, &[initial], &[])
            .unwrap();

        // Appending a smaller event to non-Claude source performs true replacement
        let mut smaller = event(b"pi_event_1", 100);
        smaller.total_tokens = 50;
        smaller.cost_microusd = None;
        smaller.cost_kind = CostKind::Unavailable;

        index
            .apply_artifact_changes(source_id, &art, &[smaller], &[])
            .unwrap();

        let (tokens, cost, kind): (i64, Option<i64>, String) = index
            .connection
            .query_row(
                "SELECT total_tokens, cost_microusd, cost_kind
                 FROM usage_events WHERE source_id = ?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(tokens, 50);
        assert_eq!(cost, None);
        assert_eq!(kind, "unavailable");
    }

    #[test]
    fn preserves_cost_provenance_and_prioritizes_reported_over_estimated() {
        let (_directory, mut index) = index();
        let source_id = source(&index);

        let mut pi_est = event(b"shared_key", 100);
        pi_est.total_tokens = 100;
        pi_est.cost_microusd = Some(2500);
        pi_est.cost_kind = CostKind::Estimated;

        // Pi event alone preserves Estimated cost_kind
        index
            .replace_artifact_events(source_id, &artifact("pi_art_1"), &[pi_est.clone()])
            .unwrap();

        let (cost, kind): (Option<i64>, String) = index
            .connection
            .query_row(
                "SELECT cost_microusd, cost_kind FROM usage_events WHERE source_id = ?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(cost, Some(2500));
        assert_eq!(kind, "estimated");

        // Duplicate Pi event across artifacts preserves Estimated cost_kind
        index
            .replace_artifact_events(source_id, &artifact("pi_art_2"), &[pi_est])
            .unwrap();

        let (cost, kind): (Option<i64>, String) = index
            .connection
            .query_row(
                "SELECT cost_microusd, cost_kind FROM usage_events WHERE source_id = ?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(cost, Some(2500));
        assert_eq!(kind, "estimated");

        // When a Reported cost candidate exists for the same event key, Reported cost wins
        let mut reported_copy = event(b"shared_key", 100);
        reported_copy.total_tokens = 100;
        reported_copy.cost_microusd = Some(3500);
        reported_copy.cost_kind = CostKind::Reported;

        index
            .replace_artifact_events(source_id, &artifact("reported_art"), &[reported_copy])
            .unwrap();

        let (cost, kind): (Option<i64>, String) = index
            .connection
            .query_row(
                "SELECT cost_microusd, cost_kind FROM usage_events WHERE source_id = ?1",
                [source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(cost, Some(3500));
        assert_eq!(kind, "reported");
    }

    #[test]
    fn migrated_database_enforces_cache_write_1h_check_constraint() {
        let (_directory, index) = index();
        index
            .connection
            .execute_batch(
                "DROP TABLE artifact_events;
                 DROP TABLE usage_events;
                 DROP TABLE usage_buckets;
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
                 PRAGMA user_version = 3;",
            )
            .unwrap();

        migrate(&index.connection).unwrap();
        let source_id = source(&index);

        // Attempting to insert cache_write_1h_tokens > cache_write_tokens should violate the CHECK constraint
        let res = index.connection.execute(
            "INSERT INTO usage_events (
                source_id, event_key, occurred_at_ms, project_id, provider, model,
                variant, messages, input_tokens, output_tokens, cache_read_tokens,
                cache_write_tokens, cache_write_1h_tokens, reasoning_tokens, total_tokens,
                cost_microusd, cost_kind
             ) VALUES (
                ?1, X'01', 100, NULL, 'anthropic', 'claude-test', 'default',
                1, 10, 10, 0, 10, 20, 0, 40, NULL, 'unavailable'
             )",
            params![source_id],
        );
        assert!(res.is_err());
        assert!(res
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("check constraint"));
    }
}
