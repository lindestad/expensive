use std::{collections::HashMap, path::PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

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

pub struct CodexSource {
    codex_home: PathBuf,
}

impl CodexSource {
    pub fn new(codex_home: PathBuf) -> Self {
        Self { codex_home }
    }

    fn source_key(&self) -> String {
        self.codex_home
            .canonicalize()
            .unwrap_or_else(|_| self.codex_home.clone())
            .display()
            .to_string()
    }

    fn roots(&self) -> [PathBuf; 2] {
        [
            self.codex_home.join("sessions"),
            self.codex_home.join("archived_sessions"),
        ]
    }
}

impl UsageSource for CodexSource {
    fn registration(&self) -> SourceRegistration {
        SourceRegistration {
            kind: SourceKind::Codex,
            source_key: self.source_key(),
            display_name: "Codex".to_string(),
        }
    }

    fn sync(&self, index: &mut UsageIndex, mode: SyncMode) -> Result<SyncReport> {
        let source_id = index.register_source(&self.registration())?;
        let files = jsonl::discover(&self.roots())?;
        let current_keys = files
            .iter()
            .map(|path| jsonl::artifact_key(path))
            .collect::<std::collections::HashSet<_>>();
        let mut report = SyncReport::default();

        for path in files {
            let key = jsonl::artifact_key(&path);
            let metadata = jsonl::metadata(&path)?;
            let checkpoint = index.artifact_checkpoint(source_id, &key)?;
            let mut plan = jsonl::plan(
                &path,
                &metadata,
                checkpoint.as_ref(),
                PARSER_VERSION,
                mode == SyncMode::Full,
            )?;
            let mut state = checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.cursor.as_deref())
                .and_then(|cursor| serde_json::from_str::<Cursor>(cursor).ok())
                .unwrap_or_default();
            if matches!(plan, ScanPlan::Append(_)) && state.session_id.is_empty() {
                plan = ScanPlan::Full;
            }
            if plan == ScanPlan::Unchanged {
                continue;
            }
            if plan == ScanPlan::Full {
                state = Cursor::default();
            }

            let start_offset = match plan {
                ScanPlan::Append(offset) => offset,
                ScanPlan::Full | ScanPlan::Unchanged => 0,
            };
            let mut events = Vec::new();
            let mut projects = HashMap::<String, ProjectRecord>::new();
            let mut scanned = 0usize;
            let mut skipped = 0usize;
            let scan = jsonl::scan_lines(&path, start_offset, plan == ScanPlan::Full, |line| {
                scanned += 1;
                match consume_line(line, &mut state, &mut projects) {
                    Ok(Some(event)) => events.push(event),
                    Ok(None) => {}
                    Err(_) => skipped += 1,
                }
            })?;
            for (cwd, project) in &projects {
                index.upsert_project(source_id, cwd, project)?;
            }
            let cursor = serde_json::to_string(&state)?;
            let artifact = jsonl::artifact(
                key,
                &path,
                &metadata,
                &scan,
                Some(cursor),
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Cursor {
    session_id: String,
    provider: String,
    model: String,
    variant: String,
    cwd: String,
    token_sequence: u64,
}

fn consume_line(
    line: &[u8],
    state: &mut Cursor,
    projects: &mut HashMap<String, ProjectRecord>,
) -> Result<Option<UsageEvent>> {
    let prefix = &line[..line.len().min(1_024)];
    if !contains(prefix, b"\"type\":\"session_meta\"")
        && !contains(prefix, b"\"type\":\"turn_context\"")
        && !(contains(prefix, b"\"type\":\"event_msg\"")
            && contains(prefix, b"\"type\":\"token_count\""))
    {
        return Ok(None);
    }
    let line: CodexLine = serde_json::from_slice(line)?;
    let payload = line.payload.as_ref();
    match line.kind.as_deref() {
        Some("session_meta") => {
            state.session_id = text_or(
                payload
                    .and_then(|payload| payload.id.as_deref())
                    .or_else(|| payload.and_then(|payload| payload.session_id.as_deref())),
                &state.session_id,
            );
            state.provider = text_or(
                payload.and_then(|payload| payload.model_provider.as_deref()),
                "openai",
            );
            update_cwd(
                state,
                projects,
                payload.and_then(|payload| payload.cwd.as_deref()),
            );
            Ok(None)
        }
        Some("turn_context") => {
            state.model = text_or(
                payload.and_then(|payload| payload.model.as_deref()),
                "unknown",
            );
            state.variant = text_or(
                payload.and_then(|payload| payload.effort.as_deref()),
                "default",
            );
            update_cwd(
                state,
                projects,
                payload.and_then(|payload| payload.cwd.as_deref()),
            );
            Ok(None)
        }
        Some("event_msg")
            if payload.and_then(|payload| payload.kind.as_deref()) == Some("token_count") =>
        {
            state.token_sequence = state.token_sequence.saturating_add(1);
            let Some(usage) = payload
                .and_then(|payload| payload.info.as_ref())
                .and_then(|info| info.last_token_usage.as_ref())
            else {
                return Ok(None);
            };
            parse_token_event(line.timestamp.as_deref(), usage, state, projects)
        }
        _ => Ok(None),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn parse_token_event(
    timestamp: Option<&str>,
    usage: &TokenUsage,
    state: &Cursor,
    projects: &mut HashMap<String, ProjectRecord>,
) -> Result<Option<UsageEvent>> {
    let timestamp = timestamp
        .and_then(|timestamp| DateTime::parse_from_rfc3339(timestamp).ok())
        .map(|timestamp| timestamp.timestamp_millis())
        .ok_or_else(|| anyhow::anyhow!("Codex token event has an invalid timestamp"))?;
    let input_total = nonnegative(usage.input_tokens);
    let cache_read_tokens = nonnegative(usage.cached_input_tokens);
    let input_tokens = input_total.saturating_sub(cache_read_tokens);
    let output_tokens = nonnegative(usage.output_tokens);
    let reasoning_tokens = nonnegative(usage.reasoning_output_tokens);
    let total_tokens = usage
        .total_tokens
        .unwrap_or_else(|| input_total.saturating_add(output_tokens))
        .max(0);
    let project = project_from_cwd(&state.cwd);
    if let Some(project) = &project {
        projects.insert(state.cwd.clone(), project.clone());
    }
    let native_key = format!("{}\0{}", state.session_id, state.token_sequence);

    Ok(Some(UsageEvent {
        event_key: event_key("codex", &native_key),
        occurred_at_ms: timestamp,
        project_id: project.map(|project| project.id),
        provider: nonempty_or(&state.provider, "openai").to_string(),
        model: nonempty_or(&state.model, "unknown").to_string(),
        variant: nonempty_or(&state.variant, "default").to_string(),
        messages: 1,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens: 0,
        reasoning_tokens,
        total_tokens,
        cost_microusd: None,
        cost_kind: CostKind::Unavailable,
    }))
}

fn update_cwd(
    state: &mut Cursor,
    projects: &mut HashMap<String, ProjectRecord>,
    value: Option<&str>,
) {
    let cwd = text_or(value, &state.cwd);
    if !cwd.is_empty() {
        state.cwd = cwd;
        if let Some(project) = project_from_cwd(&state.cwd) {
            projects.insert(state.cwd.clone(), project);
        }
    }
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

fn text_or(value: Option<&str>, fallback: &str) -> String {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

fn nonempty_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    let value = value.trim();
    if value.is_empty() {
        fallback
    } else {
        value
    }
}

fn nonnegative(value: Option<i64>) -> i64 {
    value.unwrap_or(0).max(0)
}

#[derive(Deserialize)]
struct CodexLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    payload: Option<CodexPayload>,
}

#[derive(Deserialize)]
struct CodexPayload {
    #[serde(rename = "type")]
    kind: Option<String>,
    id: Option<String>,
    session_id: Option<String>,
    model_provider: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    info: Option<TokenInfo>,
}

#[derive(Deserialize)]
struct TokenInfo {
    last_token_usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
struct TokenUsage {
    input_tokens: Option<i64>,
    cached_input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    reasoning_output_tokens: Option<i64>,
    total_tokens: Option<i64>,
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn write_header(file: &mut std::fs::File) {
        writeln!(
            file,
            r#"{{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"session-id","cwd":"/work/project","model_provider":"openai"}}}}"#
        )
        .unwrap();
        writeln!(
            file,
            r#"{{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{{"cwd":"/work/project","model":"gpt-test","effort":"high"}}}}"#
        )
        .unwrap();
    }

    fn write_usage(file: &mut std::fs::File, timestamp: &str, total: i64) {
        let value = serde_json::json!({
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {
                "type": "token_count",
                "info": {
                    "last_token_usage": {
                        "input_tokens": 10,
                        "cached_input_tokens": 4,
                        "output_tokens": 2,
                        "reasoning_output_tokens": 1,
                        "total_tokens": total
                    }
                }
            }
        });
        writeln!(file, "{value}").unwrap();
    }

    #[test]
    fn indexes_codex_requests_and_resumes_with_model_context() {
        let directory = tempfile::tempdir().unwrap();
        let codex_home = directory.path().join("codex");
        let sessions = codex_home.join("sessions/2026/01/01");
        std::fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("rollout.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        write_header(&mut file);
        write_usage(&mut file, "2026-01-01T00:00:02Z", 12);
        drop(file);
        let mut index = UsageIndex::open(&directory.path().join("usage.sqlite3")).unwrap();
        let source = CodexSource::new(codex_home);

        source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(index.diagnostics().unwrap().events, 1);

        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        write_usage(&mut file, "2026-01-01T00:00:03Z", 13);
        drop(file);
        source.sync(&mut index, SyncMode::Incremental).unwrap();

        assert_eq!(index.diagnostics().unwrap().events, 2);
    }
}
