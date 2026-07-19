use std::{collections::HashSet, path::PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::{
    index::{
        project_id_for_worktree, CostKind, ProjectRecord, SourceKind, SourceRegistration,
        UsageEvent, UsageIndex,
    },
    sources::{
        event_key,
        jsonl::{self, ScanPlan},
        SyncMode, SyncReport, UsageSource,
    },
};

const PARSER_VERSION: i64 = 1;

pub struct PiSource {
    sessions_root: PathBuf,
}

impl PiSource {
    pub fn new(sessions_root: PathBuf) -> Self {
        Self { sessions_root }
    }

    fn source_key(&self) -> String {
        self.sessions_root
            .canonicalize()
            .unwrap_or_else(|_| self.sessions_root.clone())
            .display()
            .to_string()
    }
}

impl UsageSource for PiSource {
    fn registration(&self) -> SourceRegistration {
        SourceRegistration {
            kind: SourceKind::Pi,
            source_key: self.source_key(),
            display_name: "Pi".to_string(),
        }
    }

    fn sync(&self, index: &mut UsageIndex, mode: SyncMode) -> Result<SyncReport> {
        let source_id = index.register_source(&self.registration())?;
        let files = jsonl::discover(std::slice::from_ref(&self.sessions_root))?;
        let current_keys = files
            .iter()
            .map(|path| jsonl::artifact_key(path))
            .collect::<HashSet<_>>();
        let mut report = SyncReport::default();

        for path in files {
            let key = jsonl::artifact_key(&path);
            let metadata = jsonl::metadata(&path)?;
            let checkpoint = index.artifact_checkpoint(source_id, &key)?;
            let plan = jsonl::plan(
                &path,
                &metadata,
                checkpoint.as_ref(),
                PARSER_VERSION,
                mode == SyncMode::Full,
            )?;
            if plan == ScanPlan::Unchanged {
                continue;
            }

            let header = parse_header(&jsonl::first_line(&path)?);
            let project = header
                .as_ref()
                .and_then(|header| project_from_cwd(&header.cwd));
            if let (Some(header), Some(project)) = (&header, &project) {
                index.upsert_project(source_id, &header.cwd, project)?;
            }

            let start_offset = match plan {
                ScanPlan::Append(offset) => offset,
                ScanPlan::Full | ScanPlan::Unchanged => 0,
            };
            let mut events = Vec::new();
            let mut scanned = 0usize;
            let mut skipped = 0usize;
            let scan = jsonl::scan_lines(&path, start_offset, plan == ScanPlan::Full, |line| {
                scanned += 1;
                match parse_event(line, project.as_ref()) {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => {}
                    Err(_) => skipped += 1,
                }
            })?;
            let artifact = jsonl::artifact(
                key,
                &path,
                &metadata,
                &scan,
                None,
                PARSER_VERSION,
                Utc::now().timestamp_millis(),
            )?;
            let change = match plan {
                ScanPlan::Full => index.replace_artifact_events(source_id, &artifact, &events)?,
                ScanPlan::Append(_) => {
                    index.apply_artifact_changes(source_id, &artifact, &events, &[])?
                }
                ScanPlan::Unchanged => unreachable!(),
            };
            report.scanned += scanned;
            report.imported += events.len();
            report.skipped += skipped;
            report.record_change(change);
        }

        for missing in index
            .artifact_keys(source_id)?
            .into_iter()
            .filter(|key| !current_keys.contains(key))
        {
            if let Some(change) = index.remove_artifact(source_id, &missing)? {
                report.removed += 1;
                report.record_change(change);
            }
        }

        Ok(report)
    }
}

struct SessionHeader {
    cwd: String,
}

fn parse_header(line: &[u8]) -> Option<SessionHeader> {
    let value: PiLine = serde_json::from_slice(line).ok()?;
    (value.kind.as_deref() == Some("session")).then(|| SessionHeader {
        cwd: value.cwd.unwrap_or_default(),
    })
}

fn project_from_cwd(cwd: &str) -> Option<ProjectRecord> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return None;
    }
    let path = std::path::Path::new(cwd);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(cwd)
        .to_string();
    Some(ProjectRecord {
        id: project_id_for_worktree(path),
        name,
        worktree: cwd.to_string(),
    })
}

fn parse_event(line: &[u8], project: Option<&ProjectRecord>) -> Result<Option<UsageEvent>> {
    let prefix = &line[..line.len().min(256)];
    if !prefix
        .windows(b"\"type\":\"message\"".len())
        .any(|window| window == b"\"type\":\"message\"")
    {
        return Ok(None);
    }
    let value: PiLine = serde_json::from_slice(line)?;
    if value.kind.as_deref() != Some("message") {
        return Ok(None);
    }
    let Some(message) = value.message else {
        return Ok(None);
    };
    if message.role.as_deref() != Some("assistant") {
        return Ok(None);
    }
    let id = value.id.as_deref().unwrap_or_default();
    let entry_timestamp = value.timestamp.as_deref().unwrap_or_default();
    if id.is_empty() || entry_timestamp.is_empty() {
        anyhow::bail!("Pi assistant entry has no stable ID or timestamp");
    }
    let occurred_at_ms = message
        .timestamp
        .or_else(|| {
            DateTime::parse_from_rfc3339(entry_timestamp)
                .ok()
                .map(|timestamp| timestamp.timestamp_millis())
        })
        .ok_or_else(|| anyhow::anyhow!("Pi assistant entry has an invalid timestamp"))?;
    let usage = message.usage.unwrap_or_default();
    let input_tokens = nonnegative(usage.input);
    let output_tokens = nonnegative(usage.output);
    let cache_read_tokens = nonnegative(usage.cache_read);
    let cache_write_tokens = nonnegative(usage.cache_write);
    let fallback_total = input_tokens
        .saturating_add(output_tokens)
        .saturating_add(cache_read_tokens)
        .saturating_add(cache_write_tokens);
    let total_tokens = usage.total_tokens.unwrap_or(fallback_total).max(0);
    let cost_microusd = usage
        .cost
        .and_then(|cost| cost.total)
        .and_then(cost_to_microusd);
    let native_key = format!("{id}\0{entry_timestamp}");

    Ok(Some(UsageEvent {
        event_key: event_key("pi", &native_key),
        occurred_at_ms,
        project_id: project.map(|project| project.id.clone()),
        provider: text_or(message.provider.as_deref(), "unknown"),
        model: text_or(message.model.as_deref(), "unknown"),
        variant: "default".to_string(),
        messages: 1,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens: 0,
        total_tokens,
        cost_microusd,
        cost_kind: if cost_microusd.is_some() {
            CostKind::Estimated
        } else {
            CostKind::Unavailable
        },
    }))
}

fn nonnegative(value: Option<i64>) -> i64 {
    value.unwrap_or(0).max(0)
}

fn text_or(value: Option<&str>, fallback: &str) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

#[derive(Deserialize)]
struct PiLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    timestamp: Option<String>,
    cwd: Option<String>,
    message: Option<PiMessage>,
}

#[derive(Deserialize)]
struct PiMessage {
    role: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    timestamp: Option<i64>,
    usage: Option<PiUsage>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiUsage {
    input: Option<i64>,
    output: Option<i64>,
    cache_read: Option<i64>,
    cache_write: Option<i64>,
    total_tokens: Option<i64>,
    cost: Option<PiCost>,
}

#[derive(Deserialize)]
struct PiCost {
    total: Option<f64>,
}

fn cost_to_microusd(cost: f64) -> Option<i64> {
    let micros = cost * 1_000_000.0;
    (micros.is_finite() && micros >= 0.0 && micros <= i64::MAX as f64)
        .then(|| micros.round() as i64)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn assistant(id: &str, timestamp: &str) -> String {
        format!(
            r#"{{"type":"message","id":"{id}","parentId":null,"timestamp":"{timestamp}","message":{{"role":"assistant","provider":"anthropic","model":"claude-test","timestamp":1000,"usage":{{"input":10,"output":20,"cacheRead":30,"cacheWrite":40,"totalTokens":100,"cost":{{"total":1.25}}}}}}}}"#
        )
    }

    fn write_session(path: &std::path::Path, entries: &[String]) {
        let mut file = std::fs::File::create(path).unwrap();
        writeln!(
            file,
            r#"{{"type":"session","version":3,"id":"session","timestamp":"2026-01-01T00:00:00Z","cwd":"/work/project"}}"#
        )
        .unwrap();
        for entry in entries {
            writeln!(file, "{entry}").unwrap();
        }
    }

    #[test]
    fn indexes_pi_usage_and_deduplicates_cloned_entries() {
        let directory = tempfile::tempdir().unwrap();
        let sessions = directory.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        let copied = assistant("a1b2c3d4", "2026-01-01T00:00:01Z");
        write_session(
            &sessions.join("parent.jsonl"),
            std::slice::from_ref(&copied),
        );
        write_session(&sessions.join("clone.jsonl"), &[copied]);
        let mut index = UsageIndex::open(&directory.path().join("usage.sqlite3")).unwrap();

        let report = PiSource::new(sessions)
            .sync(&mut index, SyncMode::Incremental)
            .unwrap();

        assert_eq!(report.imported, 2);
        assert_eq!(index.diagnostics().unwrap().artifacts, 2);
        assert_eq!(index.diagnostics().unwrap().events, 1);
    }

    #[test]
    fn resumes_appended_complete_lines_and_removes_deleted_files() {
        let directory = tempfile::tempdir().unwrap();
        let sessions = directory.path().join("sessions");
        std::fs::create_dir(&sessions).unwrap();
        let path = sessions.join("session.jsonl");
        write_session(&path, &[assistant("a1b2c3d4", "2026-01-01T00:00:01Z")]);
        let mut index = UsageIndex::open(&directory.path().join("usage.sqlite3")).unwrap();
        let source = PiSource::new(sessions);
        source.sync(&mut index, SyncMode::Incremental).unwrap();

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{}", assistant("b2c3d4e5", "2026-01-01T00:00:02Z")).unwrap();
        source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(index.diagnostics().unwrap().events, 2);

        std::fs::remove_file(path).unwrap();
        source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(index.diagnostics().unwrap().events, 0);
    }
}
