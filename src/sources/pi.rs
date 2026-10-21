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

const PARSER_VERSION: i64 = 2;

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
    let value: PiLine = serde_json::from_slice(line)?;
    let relevant = match value.kind.as_deref() {
        Some("message") => value.message.as_ref().is_some_and(|message| {
            message.role.as_deref() == Some("assistant")
                || (message.role.as_deref() == Some("toolResult") && message.usage.is_some())
        }),
        Some("compaction" | "branch_summary") => value.usage.is_some(),
        _ => false,
    };
    if !relevant {
        return Ok(None);
    }
    let id = value.id.as_deref().unwrap_or_default();
    let entry_timestamp = value.timestamp.as_deref().unwrap_or_default();
    if id.is_empty() || entry_timestamp.is_empty() {
        anyhow::bail!("Pi usage entry has no stable ID or timestamp");
    }
    let (usage, provider, model, messages, occurred_at_ms) = match value.kind.as_deref() {
        Some("message") => {
            let Some(message) = value.message else {
                return Ok(None);
            };
            let occurred_at_ms = match message.timestamp {
                Some(timestamp) => timestamp,
                None => outer_timestamp(entry_timestamp)
                    .ok_or_else(|| anyhow::anyhow!("Pi usage entry has an invalid timestamp"))?,
            };
            match message.role.as_deref() {
                Some("assistant") => (
                    message.usage.unwrap_or_default(),
                    text_or(message.provider.as_deref(), "unknown"),
                    first_text(
                        message.response_model.as_deref(),
                        message.model.as_deref(),
                        "unknown",
                    ),
                    1,
                    occurred_at_ms,
                ),
                Some("toolResult") if message.usage.is_some() => (
                    message.usage.unwrap_or_default(),
                    "Tools".to_string(),
                    "summaries".to_string(),
                    0,
                    occurred_at_ms,
                ),
                _ => return Ok(None),
            }
        }
        Some("compaction" | "branch_summary") if value.usage.is_some() => (
            value.usage.unwrap_or_default(),
            "Tools".to_string(),
            "summaries".to_string(),
            0,
            outer_timestamp(entry_timestamp)
                .ok_or_else(|| anyhow::anyhow!("Pi usage entry has an invalid timestamp"))?,
        ),
        _ => return Ok(None),
    };

    let input_tokens = nonnegative(usage.input);
    let output_tokens = nonnegative(usage.output);
    let cache_read_tokens = nonnegative(usage.cache_read);
    let cache_write_tokens = nonnegative(usage.cache_write);
    let reasoning_tokens = nonnegative(usage.reasoning);
    let fallback_total = input_tokens
        .saturating_add(output_tokens)
        .saturating_add(cache_read_tokens)
        .saturating_add(cache_write_tokens);
    let total_tokens = usage.total_tokens.unwrap_or(fallback_total).max(0);
    let cost_microusd = usage
        .cost
        .and_then(|cost| cost.total)
        .and_then(cost_to_microusd);
    if messages == 0 && total_tokens == 0 && cost_microusd.unwrap_or_default() == 0 {
        return Ok(None);
    }
    let native_key = format!("{id}\0{entry_timestamp}");

    Ok(Some(UsageEvent {
        event_key: event_key("pi", &native_key),
        occurred_at_ms,
        project_id: project.map(|project| project.id.clone()),
        provider,
        model,
        variant: "default".to_string(),
        messages,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        reasoning_tokens,
        total_tokens,
        cost_microusd,
        cost_kind: if cost_microusd.is_some() {
            CostKind::Estimated
        } else {
            CostKind::Unavailable
        },
    }))
}

fn outer_timestamp(timestamp: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
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

fn first_text(primary: Option<&str>, fallback: Option<&str>, default: &str) -> String {
    primary
        .filter(|value| !value.trim().is_empty())
        .or_else(|| fallback.filter(|value| !value.trim().is_empty()))
        .unwrap_or(default)
        .trim()
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
    usage: Option<PiUsage>,
}

#[derive(Deserialize)]
struct PiMessage {
    role: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    #[serde(rename = "responseModel")]
    response_model: Option<String>,
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
    reasoning: Option<i64>,
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

    #[test]
    fn indexes_current_pi_usage_shapes() {
        let project = ProjectRecord {
            id: "project".to_string(),
            name: "project".to_string(),
            worktree: "/work/project".to_string(),
        };
        let assistant = parse_event(
            br#"{"type":"message","id":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"role":"assistant","provider":"gateway","model":"requested","responseModel":"routed","timestamp":2000,"usage":{"input":10,"output":20,"reasoning":7,"cacheRead":30,"cacheWrite":40,"cost":{"total":0}}}}"#,
            Some(&project),
        )
        .unwrap()
        .unwrap();
        assert_eq!(assistant.provider, "gateway");
        assert_eq!(assistant.model, "routed");
        assert_eq!(assistant.messages, 1);
        assert_eq!(assistant.occurred_at_ms, 2000);
        assert_eq!(assistant.input_tokens, 10);
        assert_eq!(assistant.output_tokens, 20);
        assert_eq!(assistant.reasoning_tokens, 7);
        assert_eq!(assistant.cache_read_tokens, 30);
        assert_eq!(assistant.cache_write_tokens, 40);
        assert_eq!(assistant.total_tokens, 100);
        assert_eq!(assistant.cost_microusd, Some(0));
        assert_eq!(assistant.cost_kind, CostKind::Estimated);

        let malformed_outer_timestamp = parse_event(
            br#"{"type":"message","id":"assistant-message-time","timestamp":"not-rfc3339","message":{"role":"assistant","provider":"gateway","model":"requested","timestamp":3000,"usage":{"input":1,"output":2}}}"#,
            Some(&project),
        )
        .unwrap()
        .unwrap();
        assert_eq!(malformed_outer_timestamp.occurred_at_ms, 3000);

        let fallback_model = parse_event(
            br#"{"type":"message","id":"assistant-fallback","timestamp":"2026-01-01T00:00:01Z","message":{"role":"assistant","provider":"gateway","model":"requested","responseModel":"  ","usage":{"input":1,"output":2}}}"#,
            Some(&project),
        )
        .unwrap()
        .unwrap();
        assert_eq!(fallback_model.model, "requested");

        let tool = parse_event(
            br#"{"type":"message","id":"tool","timestamp":"2026-01-01T00:00:02Z","message":{"role":"toolResult","timestamp":3000,"usage":{"input":1,"output":2,"cacheRead":3,"cacheWrite":4,"cost":{"total":1.5}}}}"#,
            Some(&project),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (tool.provider, tool.model, tool.messages),
            ("Tools".to_string(), "summaries".to_string(), 0)
        );
        assert_eq!(tool.total_tokens, 10);
        assert_eq!(tool.cost_microusd, Some(1_500_000));

        let summary = parse_event(
            br#"{"type":"compaction","id":"summary","timestamp":"2026-01-01T00:00:03Z","usage":{"input":4,"output":5,"reasoning":4,"cost":{"total":2}}}"#,
            Some(&project),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (summary.provider, summary.model, summary.messages),
            ("Tools".to_string(), "summaries".to_string(), 0)
        );
        assert_eq!(summary.total_tokens, 9);
        assert_eq!(summary.reasoning_tokens, 4);
        assert_eq!(summary.cost_microusd, Some(2_000_000));
        assert_eq!(summary.occurred_at_ms, 1_767_225_603_000);

        let branch_summary = parse_event(
            br#"{"type":"branch_summary","id":"branch-summary","timestamp":"2026-01-01T00:00:04Z","usage":{"input":2,"output":3,"cost":{"total":0.25}}}"#,
            Some(&project),
        )
        .unwrap()
        .unwrap();
        assert_eq!(branch_summary.provider, "Tools");
        assert_eq!(branch_summary.model, "summaries");
        assert_eq!(branch_summary.messages, 0);
        assert_eq!(branch_summary.total_tokens, 5);
        assert_eq!(branch_summary.cost_microusd, Some(250_000));
    }

    #[test]
    fn ignores_empty_unattributed_usage() {
        for line in [
            br#"{"type":"message","id":"empty-tool","timestamp":"2026-01-01T00:00:01Z","message":{"role":"toolResult","usage":{"cost":{"total":0}}}}"#.as_slice(),
            br#"{"type":"compaction","id":"empty-compaction","timestamp":"2026-01-01T00:00:02Z","usage":{"input":0,"output":0}}"#.as_slice(),
        ] {
            assert!(parse_event(line, None).unwrap().is_none());
        }

        let total_only = parse_event(
            br#"{"type":"branch_summary","id":"total-only","timestamp":"2026-01-01T00:00:03Z","usage":{"totalTokens":7,"cost":{"total":0}}}"#,
            None,
        )
        .unwrap()
        .unwrap();
        assert_eq!(total_only.total_tokens, 7);
    }
}
