use std::{
    collections::HashMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use serde_json::Value;

use crate::{
    db,
    index::{
        project_id_for_worktree, ArtifactRecord, CostKind, ProjectRecord, SourceKind,
        SourceRegistration, UsageEvent, UsageIndex,
    },
    sources::{event_key, SyncMode, SyncReport, UsageSource},
};

const PARSER_VERSION: i64 = 1;
const MUTABLE_WINDOW_MS: i64 = 24 * 60 * 60 * 1_000;
const FINGERPRINT_SAMPLE_BYTES: usize = 64 * 1_024;

pub struct OpenCodeSource {
    path: PathBuf,
}

impl OpenCodeSource {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn source_key(&self) -> String {
        self.path
            .canonicalize()
            .unwrap_or_else(|_| self.path.clone())
            .display()
            .to_string()
    }
}

impl UsageSource for OpenCodeSource {
    fn registration(&self) -> SourceRegistration {
        SourceRegistration {
            kind: SourceKind::OpenCode,
            source_key: self.source_key(),
            display_name: "OpenCode".to_string(),
        }
    }

    fn sync(&self, index: &mut UsageIndex, requested_mode: SyncMode) -> Result<SyncReport> {
        let source_id = index.register_source(&self.registration())?;
        let connection = db::open_database(&self.path)?;
        let columns = db::table_columns(&connection, "message")?;
        let supports_incremental = columns.contains("time_updated");
        let mode = if supports_incremental {
            requested_mode
        } else {
            SyncMode::Full
        };
        let checkpoint = index.artifact_checkpoint(source_id, "database")?;
        let parser_changed = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.parser_version != PARSER_VERSION)
            .unwrap_or(true);
        let fingerprint_before = database_fingerprint(&self.path)?;
        let fingerprint_matches = checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.boundary_hash.as_ref())
            .zip(fingerprint_before.as_ref())
            .is_some_and(|(previous, current)| previous == current);
        if requested_mode == SyncMode::Incremental && !parser_changed && fingerprint_matches {
            return Ok(SyncReport::default());
        }
        let mode = if parser_changed { SyncMode::Full } else { mode };
        let watermark = checkpoint
            .as_ref()
            .map(|checkpoint| checkpoint.parsed_offset)
            .unwrap_or(0);
        let scan_from = match mode {
            SyncMode::Full => 0,
            SyncMode::Incremental => watermark.saturating_sub(MUTABLE_WINDOW_MS),
        };

        let projects = load_projects(&self.path);
        for (native_id, project) in &projects {
            index.upsert_project(source_id, native_id, project)?;
        }

        let sql = if supports_incremental {
            "SELECT message.id, message.time_created, message.time_updated, message.data,
                    session.project_id
             FROM message
             JOIN session ON session.id = message.session_id
             WHERE message.time_updated >= ?1
             ORDER BY message.time_updated, message.id"
        } else {
            "SELECT message.id, message.time_created, message.time_created, message.data,
                    session.project_id
             FROM message
             JOIN session ON session.id = message.session_id
             WHERE message.time_created >= ?1
             ORDER BY message.time_created, message.id"
        };
        let mut statement = connection.prepare(sql)?;
        let mut rows = statement.query(params![scan_from])?;
        let mut upserts = Vec::new();
        let mut removals = Vec::new();
        let mut scanned = 0usize;
        let mut skipped = 0usize;
        let mut max_updated = watermark;

        while let Some(row) = rows.next()? {
            scanned += 1;
            let id: String = row.get(0)?;
            let occurred_at_ms: i64 = row.get(1)?;
            let updated_at_ms: i64 = row.get(2)?;
            let data: String = row.get(3)?;
            let native_project_id: String = row.get(4)?;
            max_updated = max_updated.max(updated_at_ms);
            let key = event_key("opencode", &id);

            match parse_event(
                key.clone(),
                occurred_at_ms,
                projects.get(&native_project_id),
                &data,
            ) {
                Ok(Some(event)) => upserts.push(event),
                Ok(None) => removals.push(key),
                Err(_) => {
                    skipped += 1;
                    removals.push(key);
                }
            }
        }

        let fingerprint_after = database_fingerprint(&self.path)?;
        let stable_fingerprint = (fingerprint_before.is_some()
            && fingerprint_before == fingerprint_after)
            .then_some(fingerprint_after)
            .flatten();
        let artifact = database_artifact(&self.path, max_updated, stable_fingerprint)?;
        let removed = removals.len();
        let change = match mode {
            SyncMode::Full => index.replace_artifact_events(source_id, &artifact, &upserts)?,
            SyncMode::Incremental => {
                index.apply_artifact_changes(source_id, &artifact, &upserts, &removals)?
            }
        };
        Ok(SyncReport {
            change: Some(change),
            scanned,
            imported: upserts.len(),
            removed,
            skipped,
        })
    }
}

fn load_projects(path: &Path) -> HashMap<String, ProjectRecord> {
    db::list_projects(path)
        .unwrap_or_default()
        .into_iter()
        .map(|project| {
            let worktree = Path::new(&project.worktree);
            (
                project.id,
                ProjectRecord {
                    id: project_id_for_worktree(worktree),
                    name: project.name,
                    worktree: project.worktree,
                },
            )
        })
        .collect()
}

fn parse_event(
    event_key: Vec<u8>,
    occurred_at_ms: i64,
    project: Option<&ProjectRecord>,
    raw: &str,
) -> Result<Option<UsageEvent>> {
    let value: Value = serde_json::from_str(raw).context("parsing OpenCode message JSON")?;
    if value.get("role").and_then(Value::as_str) != Some("assistant") {
        return Ok(None);
    }

    let input_tokens = nonnegative_i64(value.pointer("/tokens/input"));
    let output_tokens = nonnegative_i64(value.pointer("/tokens/output"));
    let cache_read_tokens = nonnegative_i64(value.pointer("/tokens/cache/read"));
    let cache_write_tokens = nonnegative_i64(value.pointer("/tokens/cache/write"));
    let total_tokens = input_tokens
        .saturating_add(output_tokens)
        .saturating_add(cache_read_tokens)
        .saturating_add(cache_write_tokens);
    let cost = value.get("cost").and_then(Value::as_f64);
    let cost_microusd = cost.and_then(cost_to_microusd);

    Ok(Some(UsageEvent {
        event_key,
        occurred_at_ms,
        project_id: project.map(|project| project.id.clone()),
        provider: text_or(value.get("providerID"), "unknown"),
        model: text_or(value.get("modelID"), "unknown"),
        variant: text_or(value.get("variant"), "default"),
        messages: 1,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cache_write_1h_tokens: 0,
        reasoning_tokens: 0,
        total_tokens,
        cost_microusd,
        cost_kind: if cost_microusd.is_some() {
            CostKind::Reported
        } else {
            CostKind::Unavailable
        },
        is_sidechain: false,
        has_detailed_cache: false,
    }))
}

fn nonnegative_i64(value: Option<&Value>) -> i64 {
    value
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
        })
        .unwrap_or(0)
        .max(0)
}

fn text_or(value: Option<&Value>, fallback: &str) -> String {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn cost_to_microusd(cost: f64) -> Option<i64> {
    let micros = cost * 1_000_000.0;
    (micros.is_finite() && micros >= 0.0 && micros <= i64::MAX as f64)
        .then(|| micros.round() as i64)
}

fn database_artifact(
    path: &Path,
    watermark: i64,
    fingerprint: Option<Vec<u8>>,
) -> Result<ArtifactRecord> {
    let metadata = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    let (device, inode) = file_identity(&metadata);
    Ok(ArtifactRecord {
        key: "database".to_string(),
        path: Some(path.display().to_string()),
        device,
        inode,
        size: i64::try_from(metadata.len()).ok(),
        modified_ns: modified_ns(&metadata),
        parsed_offset: watermark.max(0),
        boundary_hash: fingerprint,
        full_hash: None,
        cursor: None,
        parser_version: PARSER_VERSION,
        scanned_at_ms: Utc::now().timestamp_millis(),
    })
}

fn database_fingerprint(path: &Path) -> Result<Option<Vec<u8>>> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"expensive-opencode-database-v1\0");
    if !hash_file_snapshot(&mut hasher, b"database", path)? {
        return Ok(None);
    }

    let mut wal_path = path.as_os_str().to_os_string();
    wal_path.push("-wal");
    if !hash_file_snapshot(&mut hasher, b"wal", Path::new(&wal_path))? {
        return Ok(None);
    }
    Ok(Some(hasher.finalize().as_bytes().to_vec()))
}

fn hash_file_snapshot(hasher: &mut blake3::Hasher, label: &[u8], path: &Path) -> Result<bool> {
    hasher.update(label);
    hasher.update(&[0]);
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            hasher.update(b"missing");
            return Ok(!path.exists());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("opening {}", path.display()));
        }
    };
    let before = file
        .metadata()
        .with_context(|| format!("reading metadata for {}", path.display()))?;
    let signature = metadata_signature(&before);
    hasher.update(format!("{signature:?}").as_bytes());

    let sample_len = usize::try_from(before.len())
        .unwrap_or(usize::MAX)
        .min(FINGERPRINT_SAMPLE_BYTES);
    let mut sample = vec![0; sample_len];
    file.read_exact(&mut sample)
        .with_context(|| format!("reading fingerprint head from {}", path.display()))?;
    hasher.update(&sample);

    if before.len() > FINGERPRINT_SAMPLE_BYTES as u64 {
        file.seek(SeekFrom::End(-(FINGERPRINT_SAMPLE_BYTES as i64)))
            .with_context(|| format!("seeking fingerprint tail in {}", path.display()))?;
        sample.resize(FINGERPRINT_SAMPLE_BYTES, 0);
        file.read_exact(&mut sample)
            .with_context(|| format!("reading fingerprint tail from {}", path.display()))?;
        hasher.update(&sample);
    }

    let after = file
        .metadata()
        .with_context(|| format!("re-reading metadata for {}", path.display()))?;
    let current = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("re-reading {}", path.display()));
        }
    };
    Ok(signature == metadata_signature(&after) && signature == metadata_signature(&current))
}

fn metadata_signature(metadata: &fs::Metadata) -> (Option<i64>, Option<i64>, u64, Option<i64>) {
    let (device, inode) = file_identity(metadata);
    (device, inode, metadata.len(), modified_ns(metadata))
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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn imports_and_incrementally_reconciles_opencode_messages() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("opencode.db");
        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute_batch(include_str!("../../tests/fixtures/opencode.sql"))
            .unwrap();
        drop(connection);
        let mut index = UsageIndex::open(&directory.path().join("usage.sqlite3")).unwrap();
        let source = OpenCodeSource::new(source_path.clone());

        let initial = source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(initial.imported, 1);
        assert_eq!(index.diagnostics().unwrap().events, 1);

        let unchanged = source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(unchanged.scanned, 0);
        assert_eq!(unchanged.imported, 0);
        assert_eq!(unchanged.change, None);
        assert_eq!(index.diagnostics().unwrap().generation, 1);

        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES ('assistant-b', 'session-a', 3000, 3000,
                    '{\"role\":\"assistant\",\"cost\":2.5,\"tokens\":{\"input\":1,\"output\":2},\"modelID\":\"new\",\"providerID\":\"provider\"}')",
                [],
            )
            .unwrap();
        drop(connection);

        let update = source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(update.imported, 2);
        assert_eq!(index.diagnostics().unwrap().events, 2);

        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute(
                "UPDATE message
                 SET data = '{\"role\":\"user\"}', time_updated = 4000
                 WHERE id = 'assistant-a'",
                [],
            )
            .unwrap();
        drop(connection);

        source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(index.diagnostics().unwrap().events, 1);
    }

    #[test]
    fn database_fingerprint_tracks_main_database_and_wal_changes() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("opencode.db");
        fs::write(&database, b"database-a").unwrap();

        let without_wal = database_fingerprint(&database).unwrap().unwrap();
        let mut wal = database.as_os_str().to_os_string();
        wal.push("-wal");
        fs::write(Path::new(&wal), b"wal-a").unwrap();
        let with_wal = database_fingerprint(&database).unwrap().unwrap();
        fs::write(Path::new(&wal), b"wal-b").unwrap();
        let changed_wal = database_fingerprint(&database).unwrap().unwrap();
        fs::write(&database, b"database-b").unwrap();
        let changed_database = database_fingerprint(&database).unwrap().unwrap();

        assert_ne!(without_wal, with_wal);
        assert_ne!(with_wal, changed_wal);
        assert_ne!(changed_wal, changed_database);
    }

    #[test]
    fn incremental_sync_reconciles_one_mutable_day() {
        assert_eq!(MUTABLE_WINDOW_MS, 24 * 60 * 60 * 1_000);
    }

    #[test]
    fn full_sync_retracts_rows_deleted_from_source_database() {
        let directory = tempfile::tempdir().unwrap();
        let source_path = directory.path().join("opencode.db");
        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute_batch(include_str!("../../tests/fixtures/opencode.sql"))
            .unwrap();
        drop(connection);
        let mut index = UsageIndex::open(&directory.path().join("usage.sqlite3")).unwrap();
        let source = OpenCodeSource::new(source_path.clone());

        source.sync(&mut index, SyncMode::Full).unwrap();
        let connection = Connection::open(&source_path).unwrap();
        connection
            .execute("DELETE FROM message WHERE id = 'assistant-a'", [])
            .unwrap();
        drop(connection);

        source.sync(&mut index, SyncMode::Full).unwrap();
        assert_eq!(index.diagnostics().unwrap().events, 0);
    }
}
