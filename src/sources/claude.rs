use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{event_key, jsonl, SyncMode, SyncReport, UsageSource};
use crate::index::{
    project_id_for_worktree, CostKind, ProjectRecord, SourceKind, SourceRegistration, UsageEvent,
    UsageIndex,
};

const PARSER_VERSION: i64 = 1;

#[derive(Clone, Debug)]
pub struct ClaudeSource {
    pub root: PathBuf,
}

impl ClaudeSource {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn is_available(&self) -> bool {
        self.root.exists()
    }
}

impl UsageSource for ClaudeSource {
    fn registration(&self) -> SourceRegistration {
        SourceRegistration {
            kind: SourceKind::Claude,
            source_key: "default".to_string(),
            display_name: "Claude Code".to_string(),
        }
    }

    fn sync(&self, index: &mut UsageIndex, mode: SyncMode) -> Result<SyncReport> {
        let source_id = index.register_source(&self.registration())?;
        let projects_root = self.root.join("projects");
        let files = if projects_root.is_dir() {
            jsonl::discover(std::slice::from_ref(&projects_root))?
        } else {
            Vec::new()
        };

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
            if plan == jsonl::ScanPlan::Unchanged {
                continue;
            }

            let mut state = checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.cursor.as_deref())
                .and_then(|cursor| serde_json::from_str::<Cursor>(cursor).ok())
                .unwrap_or_default();
            if plan == jsonl::ScanPlan::Full {
                state = Cursor::default();
            }

            let mut projects = HashMap::<String, ProjectRecord>::new();
            if let Some(cwd) = &state.current_cwd {
                if let Some(project) = project_from_cwd(cwd) {
                    projects.insert(cwd.clone(), project);
                }
            }

            let start_offset = match plan {
                jsonl::ScanPlan::Append(offset) => offset,
                jsonl::ScanPlan::Full | jsonl::ScanPlan::Unchanged => 0,
            };

            let mut candidates: HashMap<Vec<u8>, Candidate> = HashMap::new();
            let mut scanned = 0usize;
            let mut skipped = 0usize;

            let scan =
                jsonl::scan_lines(&path, start_offset, plan == jsonl::ScanPlan::Full, |line| {
                    scanned += 1;
                    match parse_line(line, &mut state, &mut projects) {
                        Ok(Some(candidate)) => {
                            candidates
                                .entry(candidate.event.event_key.clone())
                                .and_modify(|existing| merge_candidate(existing, candidate.clone()))
                                .or_insert(candidate);
                        }
                        Ok(None) => {}
                        Err(_) => skipped += 1,
                    }
                })?;

            for (cwd, project) in &projects {
                index.upsert_project(source_id, cwd, project)?;
            }

            let events: Vec<UsageEvent> = candidates.into_values().map(|c| c.event).collect();
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
                jsonl::ScanPlan::Full => {
                    index.replace_artifact_events(source_id, &artifact, &events)?
                }
                jsonl::ScanPlan::Append(_) => {
                    index.apply_artifact_changes(source_id, &artifact, &events, &[])?
                }
                jsonl::ScanPlan::Unchanged => unreachable!(),
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

#[derive(Clone, Debug)]
struct Candidate {
    event: UsageEvent,
    is_sidechain: bool,
    has_detailed_cache: bool,
}

fn merge_candidate(existing: &mut Candidate, incoming: Candidate) {
    let prefer_incoming = if incoming.event.total_tokens > existing.event.total_tokens {
        true
    } else if incoming.event.total_tokens == existing.event.total_tokens {
        if incoming.has_detailed_cache && !existing.has_detailed_cache {
            true
        } else if incoming.has_detailed_cache == existing.has_detailed_cache {
            !incoming.is_sidechain && existing.is_sidechain
        } else {
            false
        }
    } else {
        false
    };

    let reported_cost = incoming
        .event
        .cost_microusd
        .or(existing.event.cost_microusd);
    let is_bedrock =
        incoming.event.provider == "amazon-bedrock" || existing.event.provider == "amazon-bedrock";
    let project_id = if prefer_incoming {
        incoming
            .event
            .project_id
            .clone()
            .or_else(|| existing.event.project_id.clone())
    } else {
        existing
            .event
            .project_id
            .clone()
            .or_else(|| incoming.event.project_id.clone())
    };

    if prefer_incoming {
        *existing = incoming;
    }

    if let Some(cost) = reported_cost {
        existing.event.cost_microusd = Some(cost);
        existing.event.cost_kind = CostKind::Reported;
    }
    if is_bedrock {
        existing.event.provider = "amazon-bedrock".to_string();
    }
    existing.event.project_id = project_id;
}

#[derive(Debug, Deserialize)]
struct RawClaudeLine {
    #[serde(rename = "type")]
    kind: Option<String>,
    #[serde(rename = "sessionId")]
    session_id_camel: Option<String>,
    session_id: Option<String>,
    uuid: Option<String>,
    id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "createdAt")]
    created_at_camel: Option<String>,
    created_at: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "isSidechain")]
    is_sidechain_camel: Option<bool>,
    is_sidechain: Option<bool>,
    #[serde(rename = "costUSD")]
    cost_usd_upper: Option<f64>,
    cost_usd: Option<f64>,
    cost: Option<f64>,
    effort: Option<String>,
    model: Option<String>,
    role: Option<String>,
    message: Option<RawClaudeMessage>,
    usage: Option<RawClaudeUsage>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    ephemeral_5m_input_tokens: Option<i64>,
    ephemeral_1h_input_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RawClaudeMessage {
    id: Option<String>,
    role: Option<String>,
    model: Option<String>,
    usage: Option<RawClaudeUsage>,
    #[serde(rename = "createdAt")]
    created_at_camel: Option<String>,
    created_at: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "costUSD")]
    cost_usd_upper: Option<f64>,
    cost_usd: Option<f64>,
    cost: Option<f64>,
    effort: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawClaudeUsage {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_read_input_tokens: Option<i64>,
    cache_creation_input_tokens: Option<i64>,
    cache_creation: Option<RawCacheCreation>,
    ephemeral_5m_input_tokens: Option<i64>,
    ephemeral_1h_input_tokens: Option<i64>,
    thinking_tokens: Option<i64>,
    output_tokens_details: Option<RawOutputTokensDetails>,
}

#[derive(Debug, Deserialize)]
struct RawCacheCreation {
    ephemeral_5m_input_tokens: Option<i64>,
    ephemeral_1h_input_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RawOutputTokensDetails {
    thinking_tokens: Option<i64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Cursor {
    current_cwd: Option<String>,
}

fn is_bedrock_model(model: &str) -> bool {
    let m = model.trim();
    if m.starts_with("anthropic.")
        || m.starts_with("us.anthropic.")
        || m.starts_with("eu.anthropic.")
        || m.starts_with("apac.anthropic.")
        || m.starts_with("global.anthropic.")
    {
        return true;
    }
    if (m.starts_with("arn:aws:bedrock:")
        || m.starts_with("arn:aws-us-gov:bedrock:")
        || m.starts_with("arn:aws-cn:bedrock:"))
        && (m.contains(":foundation-model/") || m.contains(":inference-profile/"))
    {
        return true;
    }
    false
}

fn parse_line(
    line: &[u8],
    state: &mut Cursor,
    projects: &mut HashMap<String, ProjectRecord>,
) -> Result<Option<Candidate>> {
    let raw: RawClaudeLine = serde_json::from_slice(line)?;

    if let Some(cwd) = raw.cwd.as_deref().filter(|c| !c.trim().is_empty()) {
        state.current_cwd = Some(cwd.to_string());
        if !projects.contains_key(cwd) {
            if let Some(project) = project_from_cwd(cwd) {
                projects.insert(cwd.to_string(), project);
            }
        }
    }

    let is_assistant = if let Some(kind) = raw.kind.as_deref() {
        kind == "assistant"
    } else {
        raw.role.as_deref() == Some("assistant")
            || raw.message.as_ref().and_then(|m| m.role.as_deref()) == Some("assistant")
    };

    if !is_assistant {
        return Ok(None);
    }

    let timestamp_str = raw
        .created_at_camel
        .as_deref()
        .or(raw.created_at.as_deref())
        .or(raw.timestamp.as_deref())
        .or_else(|| {
            raw.message.as_ref().and_then(|m| {
                m.created_at_camel
                    .as_deref()
                    .or(m.created_at.as_deref())
                    .or(m.timestamp.as_deref())
            })
        })
        .ok_or_else(|| anyhow!("missing timestamp on Claude turn"))?;

    let occurred_at_ms = DateTime::parse_from_rfc3339(timestamp_str)
        .context("invalid RFC 3339 timestamp on Claude turn")?
        .timestamp_millis();

    let raw_message_id = raw
        .message
        .as_ref()
        .and_then(|m| m.id.as_deref())
        .map(str::trim)
        .filter(|id| !id.is_empty());

    let session_id = raw
        .session_id_camel
        .as_deref()
        .or(raw.session_id.as_deref())
        .map(str::trim)
        .unwrap_or_default();

    let native_id = if let Some(msg_id) = raw_message_id {
        msg_id.to_string()
    } else {
        let uuid = raw.uuid.as_deref().map(str::trim).unwrap_or("");
        if session_id.is_empty() || uuid.is_empty() {
            return Ok(None);
        }
        format!("{session_id}\0{uuid}")
    };

    let model = raw
        .message
        .as_ref()
        .and_then(|m| m.model.as_deref())
        .or(raw.model.as_deref())
        .unwrap_or("unknown")
        .trim();

    let raw_top_level_id = raw.id.as_deref().map(str::trim).filter(|id| !id.is_empty());
    let is_bedrock_id = raw_message_id.is_some_and(|id| id.starts_with("msg_bdrk_"))
        || raw_top_level_id.is_some_and(|id| id.starts_with("msg_bdrk_"));
    let provider = if is_bedrock_id || is_bedrock_model(model) {
        "amazon-bedrock".to_string()
    } else {
        "anthropic".to_string()
    };

    let variant = raw
        .effort
        .as_deref()
        .or_else(|| raw.message.as_ref().and_then(|m| m.effort.as_deref()))
        .filter(|e| !e.trim().is_empty())
        .unwrap_or("default")
        .to_string();

    let usage = raw
        .message
        .as_ref()
        .and_then(|m| m.usage.as_ref())
        .or(raw.usage.as_ref());

    let input_tokens = usage
        .and_then(|u| u.input_tokens)
        .or(raw.input_tokens)
        .unwrap_or(0)
        .max(0);

    let output_tokens = usage
        .and_then(|u| u.output_tokens)
        .or(raw.output_tokens)
        .unwrap_or(0)
        .max(0);

    let cache_read_tokens = usage
        .and_then(|u| u.cache_read_input_tokens)
        .or(raw.cache_read_input_tokens)
        .unwrap_or(0)
        .max(0);

    let eph_5m = usage
        .and_then(|u| {
            u.cache_creation
                .as_ref()
                .and_then(|cc| cc.ephemeral_5m_input_tokens)
                .or(u.ephemeral_5m_input_tokens)
        })
        .or(raw.ephemeral_5m_input_tokens);
    let eph_1h = usage
        .and_then(|u| {
            u.cache_creation
                .as_ref()
                .and_then(|cc| cc.ephemeral_1h_input_tokens)
                .or(u.ephemeral_1h_input_tokens)
        })
        .or(raw.ephemeral_1h_input_tokens);
    let creation = usage
        .and_then(|u| u.cache_creation_input_tokens)
        .or(raw.cache_creation_input_tokens);

    let has_detailed_cache = eph_5m.is_some() || eph_1h.is_some();
    let (cache_write_tokens, cache_write_1h_tokens) = match (eph_5m, eph_1h) {
        (Some(m5), Some(h1)) => (m5.max(0).saturating_add(h1.max(0)), h1.max(0)),
        (Some(m5), None) => (m5.max(0), 0),
        (None, Some(h1)) => (h1.max(0), h1.max(0)),
        (None, None) => (creation.unwrap_or(0).max(0), 0),
    };

    let reasoning_tokens = usage
        .and_then(|u| {
            u.output_tokens_details
                .as_ref()
                .and_then(|d| d.thinking_tokens)
                .or(u.thinking_tokens)
        })
        .unwrap_or(0)
        .max(0);

    let total_tokens = input_tokens
        .saturating_add(output_tokens)
        .saturating_add(cache_read_tokens)
        .saturating_add(cache_write_tokens);

    let cost_usd = [
        raw.message.as_ref().and_then(|m| m.cost_usd_upper),
        raw.message.as_ref().and_then(|m| m.cost_usd),
        raw.message.as_ref().and_then(|m| m.cost),
        raw.cost_usd_upper,
        raw.cost_usd,
        raw.cost,
    ]
    .into_iter()
    .flatten()
    .find(|cost| cost.is_finite() && *cost > 0.0);
    let cost_microusd = cost_usd.map(|cost| (cost * 1_000_000.0).round() as i64);

    if total_tokens == 0 && cost_microusd.is_none() {
        return Ok(None);
    }

    let is_sidechain = raw.is_sidechain_camel.or(raw.is_sidechain).unwrap_or(false);

    let project_id = state
        .current_cwd
        .as_deref()
        .and_then(|cwd| projects.get(cwd))
        .map(|p| p.id.clone());

    let event = UsageEvent {
        event_key: event_key("claude", &native_id),
        occurred_at_ms,
        project_id,
        provider,
        model: model.to_string(),
        variant,
        messages: 1,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        cache_write_1h_tokens,
        reasoning_tokens,
        total_tokens,
        cost_microusd,
        cost_kind: if cost_microusd.is_some() {
            CostKind::Reported
        } else {
            CostKind::Unavailable
        },
        is_sidechain,
        has_detailed_cache,
    };

    Ok(Some(Candidate {
        event,
        is_sidechain,
        has_detailed_cache,
    }))
}

fn project_from_cwd(cwd: &str) -> Option<ProjectRecord> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        return None;
    }
    let path = Path::new(cwd);
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn parses_claude_assistant_record_with_cache_breakdown() {
        let json = r#"{
            "type": "assistant",
            "sessionId": "sess-1",
            "uuid": "u-1",
            "cwd": "/work/my-app",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "isSidechain": false,
            "costUSD": 0.025,
            "message": {
                "id": "msg_01abc",
                "role": "assistant",
                "model": "claude-3-7-sonnet-20250219",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_read_input_tokens": 300,
                    "ephemeral_5m_input_tokens": 200,
                    "ephemeral_1h_input_tokens": 100,
                    "output_tokens_details": {
                        "thinking_tokens": 25
                    }
                }
            }
        }"#;

        let mut state = Cursor::default();
        let mut projects = HashMap::new();
        let candidate = parse_line(json.as_bytes(), &mut state, &mut projects)
            .unwrap()
            .expect("should parse candidate");

        assert_eq!(candidate.event.provider, "anthropic");
        assert_eq!(candidate.event.model, "claude-3-7-sonnet-20250219");
        assert_eq!(candidate.event.input_tokens, 100);
        assert_eq!(candidate.event.output_tokens, 50);
        assert_eq!(candidate.event.cache_read_tokens, 300);
        assert_eq!(candidate.event.cache_write_tokens, 300);
        assert_eq!(candidate.event.cache_write_1h_tokens, 100);
        assert_eq!(candidate.event.reasoning_tokens, 25);
        assert_eq!(candidate.event.total_tokens, 750);
        assert_eq!(candidate.event.cost_microusd, Some(25_000));
        assert_eq!(candidate.event.cost_kind, CostKind::Reported);
        assert!(candidate.has_detailed_cache);
        assert!(!candidate.is_sidechain);
        assert_eq!(state.current_cwd.as_deref(), Some("/work/my-app"));
        assert!(!projects.is_empty());
    }

    #[test]
    fn detects_bedrock_provider_from_msg_id_or_model_prefix() {
        let json_bedrock_id = r#"{
            "type": "assistant",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "id": "msg_bdrk_01xyz",
                "role": "assistant",
                "model": "claude-3-7-sonnet-20250219",
                "usage": { "input_tokens": 10, "output_tokens": 10 }
            }
        }"#;

        let candidate = parse_line(
            json_bedrock_id.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(candidate.event.provider, "amazon-bedrock");

        let json_bedrock_model = r#"{
            "type": "assistant",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "id": "msg_norm_01xyz",
                "role": "assistant",
                "model": "anthropic.claude-sonnet-5:0",
                "usage": { "input_tokens": 10, "output_tokens": 10 }
            }
        }"#;

        let candidate2 = parse_line(
            json_bedrock_model.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(candidate2.event.provider, "amazon-bedrock");
    }

    #[test]
    fn ignores_user_and_zero_token_records() {
        let json_user = r#"{"type": "user", "timestamp": "2025-05-01T12:00:00.000Z", "message": {"role": "user"}}"#;
        assert!(parse_line(
            json_user.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new()
        )
        .unwrap()
        .is_none());

        let json_zero = r#"{
            "type": "assistant",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "id": "msg_zero",
                "role": "assistant",
                "model": "claude-3-7-sonnet",
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        }"#;
        assert!(parse_line(
            json_zero.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new()
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn candidate_deduplication_prefers_larger_total_or_detailed_cache() {
        let base_event = UsageEvent {
            event_key: vec![1, 2, 3],
            occurred_at_ms: 1000,
            project_id: None,
            provider: "anthropic".to_string(),
            model: "claude-3-7-sonnet".to_string(),
            variant: "default".to_string(),
            messages: 1,
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 0,
            cache_write_tokens: 100,
            cache_write_1h_tokens: 0,
            reasoning_tokens: 0,
            total_tokens: 250,
            cost_microusd: None,
            cost_kind: CostKind::Unavailable,
            is_sidechain: false,
            has_detailed_cache: false,
        };

        let mut existing = Candidate {
            event: base_event.clone(),
            is_sidechain: true,
            has_detailed_cache: false,
        };

        // Same tokens, but detailed cache duration present
        let mut detailed = base_event.clone();
        detailed.cache_write_1h_tokens = 50;
        let incoming = Candidate {
            event: detailed,
            is_sidechain: false,
            has_detailed_cache: true,
        };

        merge_candidate(&mut existing, incoming);
        assert_eq!(existing.event.cache_write_1h_tokens, 50);
        assert!(!existing.is_sidechain);
    }

    #[test]
    fn parses_nested_cache_creation_tokens() {
        let json = r#"{
            "type": "assistant",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "id": "msg_nested_cache",
                "role": "assistant",
                "model": "claude-3-7-sonnet-20250219",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_creation": {
                        "ephemeral_5m_input_tokens": 200,
                        "ephemeral_1h_input_tokens": 80
                    }
                }
            }
        }"#;
        let candidate = parse_line(json.as_bytes(), &mut Cursor::default(), &mut HashMap::new())
            .unwrap()
            .unwrap();

        assert_eq!(candidate.event.cache_write_tokens, 280);
        assert_eq!(candidate.event.cache_write_1h_tokens, 80);
        assert!(candidate.has_detailed_cache);
    }

    #[test]
    fn parses_json_with_duplicate_camel_and_snake_keys() {
        let json = r#"{
            "type": "assistant",
            "sessionId": "sess-camel",
            "session_id": "sess-snake",
            "createdAt": "2025-05-01T12:00:00.000Z",
            "created_at": "2025-05-01T12:00:00.000Z",
            "isSidechain": true,
            "is_sidechain": true,
            "costUSD": 0.05,
            "cost_usd": 0.05,
            "message": {
                "id": "msg_dup_keys",
                "role": "assistant",
                "model": "claude-3-7-sonnet-20250219",
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50
                }
            }
        }"#;
        let candidate = parse_line(json.as_bytes(), &mut Cursor::default(), &mut HashMap::new())
            .unwrap()
            .unwrap();

        assert_eq!(candidate.event.cost_microusd, Some(50_000));
        assert_eq!(candidate.event.cost_kind, CostKind::Reported);
        assert!(candidate.is_sidechain);
    }

    #[test]
    fn accepts_cost_only_record_when_cost_is_positive() {
        let json = r#"{
            "type": "assistant",
            "costUSD": 0.0125,
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "id": "msg_cost_only",
                "role": "assistant",
                "model": "claude-3-7-sonnet",
                "usage": { "input_tokens": 0, "output_tokens": 0 }
            }
        }"#;
        let candidate = parse_line(json.as_bytes(), &mut Cursor::default(), &mut HashMap::new())
            .unwrap()
            .unwrap();

        assert_eq!(candidate.event.total_tokens, 0);
        assert_eq!(candidate.event.cost_microusd, Some(12_500));
        assert_eq!(candidate.event.cost_kind, CostKind::Reported);
    }

    #[test]
    fn rejects_non_positive_or_nan_costs() {
        let json_zero_cost = r#"{
            "type": "assistant",
            "costUSD": 0.0,
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "id": "msg_cost_zero",
                "role": "assistant",
                "model": "claude-3-7-sonnet",
                "usage": { "input_tokens": 10, "output_tokens": 10 }
            }
        }"#;
        let candidate = parse_line(
            json_zero_cost.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(candidate.event.cost_microusd, None);
        assert_eq!(candidate.event.cost_kind, CostKind::Unavailable);
    }

    #[test]
    fn sync_discovers_and_imports_claude_project_sessions() {
        let dir = tempdir().unwrap();
        let claude_home = dir.path().join(".claude");
        let project_dir = claude_home.join("projects").join("project-a");
        fs::create_dir_all(&project_dir).unwrap();

        let session_file = project_dir.join("session.jsonl");
        let session_content = format!(
            "{}\n{}\n",
            r#"{"type":"user","cwd":"/work/test-proj","timestamp":"2025-05-01T12:00:00.000Z"}"#,
            r#"{"type":"assistant","timestamp":"2025-05-01T12:00:01.000Z","message":{"id":"msg_test_1","role":"assistant","model":"claude-3-7-sonnet-20250219","usage":{"input_tokens":100,"output_tokens":200}}}"#
        );
        fs::write(&session_file, session_content).unwrap();

        let index_path = dir.path().join("usage.sqlite3");
        let mut index = UsageIndex::open(&index_path).unwrap();
        let source = ClaudeSource::new(&claude_home);

        assert!(source.is_available());
        let report = source.sync(&mut index, SyncMode::Incremental).unwrap();
        assert_eq!(report.scanned, 2);
        assert_eq!(report.imported, 1);

        assert_eq!(index.diagnostics().unwrap().events, 1);
    }

    #[test]
    fn fallback_identity_requires_session_id_and_uuid_and_ignores_raw_id() {
        // Valid fallback: has session_id and uuid
        let json_valid = r#"{
            "type": "assistant",
            "sessionId": "sess-fallback",
            "uuid": "uuid-fallback-1",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "role": "assistant",
                "model": "claude-3-7-sonnet",
                "usage": { "input_tokens": 10, "output_tokens": 10 }
            }
        }"#;
        let c = parse_line(
            json_valid.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new(),
        )
        .unwrap()
        .expect("should parse fallback identity");
        assert_eq!(
            c.event.event_key,
            event_key("claude", "sess-fallback\0uuid-fallback-1")
        );

        // Missing uuid (only top-level raw id present) -> skipped
        let json_no_uuid = r#"{
            "type": "assistant",
            "sessionId": "sess-fallback",
            "id": "raw-id-to-ignore",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "role": "assistant",
                "model": "claude-3-7-sonnet",
                "usage": { "input_tokens": 10, "output_tokens": 10 }
            }
        }"#;
        assert!(parse_line(
            json_no_uuid.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new()
        )
        .unwrap()
        .is_none());

        // Missing sessionId -> skipped
        let json_no_session = r#"{
            "type": "assistant",
            "uuid": "uuid-fallback-1",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "role": "assistant",
                "model": "claude-3-7-sonnet",
                "usage": { "input_tokens": 10, "output_tokens": 10 }
            }
        }"#;
        assert!(parse_line(
            json_no_session.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new()
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn positive_cost_is_not_shadowed_by_earlier_zero_cost() {
        let json = r#"{
            "type": "assistant",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "cost": 0.045,
            "message": {
                "id": "msg_shadow_test",
                "role": "assistant",
                "model": "claude-3-7-sonnet",
                "costUSD": 0.0,
                "usage": { "input_tokens": 10, "output_tokens": 10 }
            }
        }"#;
        let c = parse_line(json.as_bytes(), &mut Cursor::default(), &mut HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(c.event.cost_microusd, Some(45_000));
        assert_eq!(c.event.cost_kind, CostKind::Reported);
    }

    #[test]
    fn bedrock_model_classification_strictness() {
        let test_cases = [
            ("anthropic.claude-v1", "amazon-bedrock"),
            ("us.anthropic.claude-3-7-sonnet-20250219:0", "amazon-bedrock"),
            ("eu.anthropic.claude-3-5-sonnet-20240620-v1:0", "amazon-bedrock"),
            ("apac.anthropic.claude-3-sonnet", "amazon-bedrock"),
            ("global.anthropic.claude-3-haiku", "amazon-bedrock"),
            ("arn:aws:bedrock:us-east-1:123456789012:foundation-model/anthropic.claude-v1", "amazon-bedrock"),
            ("arn:aws:bedrock:us-west-2:123456789012:inference-profile/us.anthropic.claude-3-5-sonnet-20241022-v2:0", "amazon-bedrock"),
            // Non-bedrock or invalid forms
            ("claude-3-7-sonnet-20250219", "anthropic"),
            ("random.anthropic.fake", "anthropic"),
            ("arn:aws:bedrock:invalid", "anthropic"),
            ("other.provider.model", "anthropic"),
        ];

        for (model_id, expected_provider) in test_cases {
            let json = format!(
                r#"{{
                    "type": "assistant",
                    "timestamp": "2025-05-01T12:00:00.000Z",
                    "message": {{
                        "id": "msg_model_test",
                        "role": "assistant",
                        "model": "{model_id}",
                        "usage": {{ "input_tokens": 10, "output_tokens": 10 }}
                    }}
                }}"#
            );
            let c = parse_line(json.as_bytes(), &mut Cursor::default(), &mut HashMap::new())
                .unwrap()
                .unwrap();
            assert_eq!(
                c.event.provider, expected_provider,
                "model {model_id} should have provider {expected_provider}"
            );
        }
    }

    #[test]
    fn parses_assistant_turn_with_error_and_valid_usage() {
        let json = r#"{
            "type": "assistant",
            "is_error": true,
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "id": "msg_error_turn",
                "role": "assistant",
                "model": "claude-3-7-sonnet-20250219",
                "usage": {
                    "input_tokens": 150,
                    "output_tokens": 50
                }
            }
        }"#;
        let c = parse_line(json.as_bytes(), &mut Cursor::default(), &mut HashMap::new())
            .unwrap()
            .unwrap();
        assert_eq!(c.event.input_tokens, 150);
        assert_eq!(c.event.output_tokens, 50);
        assert_eq!(c.event.total_tokens, 200);
    }

    #[test]
    fn honors_explicit_top_level_type_over_nested_assistant_role() {
        let json_user = r#"{
            "type": "user",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "id": "msg_user_with_assistant_role",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "usage": { "input_tokens": 100, "output_tokens": 50 }
            }
        }"#;
        let c = parse_line(
            json_user.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new(),
        )
        .unwrap();
        assert!(c.is_none());

        // When top-level type is absent, nested role is used as fallback
        let json_no_type = r#"{
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "id": "msg_no_type_assistant",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "usage": { "input_tokens": 100, "output_tokens": 50 }
            }
        }"#;
        let c = parse_line(
            json_no_type.as_bytes(),
            &mut Cursor::default(),
            &mut HashMap::new(),
        )
        .unwrap();
        assert!(c.is_some());
    }

    #[test]
    fn detects_bedrock_provider_from_top_level_id_with_fallback_identity() {
        let json = r#"{
            "type": "assistant",
            "id": "msg_bdrk_top_level_synthetic",
            "sessionId": "sess-synthetic",
            "uuid": "uuid-synthetic-1",
            "timestamp": "2025-05-01T12:00:00.000Z",
            "message": {
                "role": "assistant",
                "model": "claude-3-7-sonnet-20250219",
                "usage": { "input_tokens": 50, "output_tokens": 25 }
            }
        }"#;

        let candidate = parse_line(json.as_bytes(), &mut Cursor::default(), &mut HashMap::new())
            .unwrap()
            .expect("should parse synthetic bedrock turn");

        assert_eq!(candidate.event.provider, "amazon-bedrock");
        assert_eq!(
            candidate.event.event_key,
            event_key("claude", "sess-synthetic\0uuid-synthetic-1")
        );
        assert_eq!(candidate.event.model, "claude-3-7-sonnet-20250219");
    }
}
