//! Usage source adapters.

use std::{sync::mpsc, thread};

use anyhow::Result;

use crate::{
    config::Config,
    index::{IndexChange, SourceKind, SourceRegistration, UsageIndex},
};

pub mod claude;
pub mod codex;
pub mod copilot;
mod jsonl;
pub mod opencode;
pub mod pi;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncMode {
    Incremental,
    Full,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SyncReport {
    pub change: Option<IndexChange>,
    pub scanned: usize,
    pub imported: usize,
    pub removed: usize,
    pub skipped: usize,
}

impl SyncReport {
    pub(crate) fn record_change(&mut self, change: IndexChange) {
        self.change = Some(match self.change.take() {
            Some(previous) => IndexChange {
                generation: previous.generation.max(change.generation),
                source_id: change.source_id,
                start_ms: match (previous.start_ms, change.start_ms) {
                    (Some(left), Some(right)) => Some(left.min(right)),
                    (left, right) => left.or(right),
                },
                end_ms: match (previous.end_ms, change.end_ms) {
                    (Some(left), Some(right)) => Some(left.max(right)),
                    (left, right) => left.or(right),
                },
                event_count: previous.event_count + change.event_count,
            },
            None => change,
        });
    }
}

pub trait UsageSource: Send {
    fn registration(&self) -> SourceRegistration;
    fn sync(&self, index: &mut UsageIndex, mode: SyncMode) -> Result<SyncReport>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SyncProgress {
    Planned {
        sources: Vec<String>,
    },
    Started {
        source: String,
    },
    Finished {
        source: String,
        report: Option<SyncReport>,
        error: Option<String>,
        changed: bool,
    },
}

#[derive(Clone, Debug, Default)]
pub struct SyncSummary {
    pub reports: Vec<(String, SyncReport)>,
    pub errors: Vec<String>,
    pub generation_before: i64,
    pub generation_after: i64,
}

impl SyncSummary {
    pub fn changed(&self) -> bool {
        self.generation_after != self.generation_before
    }
}

pub fn sync_configured(config: &Config, mode: SyncMode) -> Result<SyncSummary> {
    sync_configured_with_progress(config, mode, |_| {})
}

pub fn sync_configured_with_progress(
    config: &Config,
    mode: SyncMode,
    mut progress: impl FnMut(SyncProgress),
) -> Result<SyncSummary> {
    let index = UsageIndex::open(&config.index_path)?;
    let generation_before = index.diagnostics()?.generation;
    let claude_available = config.claude_home.join("projects").is_dir()
        || index
            .has_artifacts_for_kind(SourceKind::Claude)
            .unwrap_or(false);
    drop(index);
    let mut sources: Vec<Box<dyn UsageSource>> = Vec::new();
    let opencode_available = config.db_path.is_file();
    if opencode_available {
        sources.push(Box::new(opencode::OpenCodeSource::new(
            config.db_path.clone(),
        )));
    }
    if config.copilot_home.join("session-store.db").is_file() {
        sources.push(Box::new(copilot::CopilotSource::new(
            config.copilot_home.clone(),
        )));
    }
    if config.pi_sessions_root.is_dir() {
        sources.push(Box::new(pi::PiSource::new(config.pi_sessions_root.clone())));
    }
    if config.codex_home.join("sessions").is_dir()
        || config.codex_home.join("archived_sessions").is_dir()
    {
        sources.push(Box::new(codex::CodexSource::new(config.codex_home.clone())));
    }
    if claude_available {
        sources.push(Box::new(claude::ClaudeSource::new(
            config.claude_home.clone(),
        )));
    }

    progress(SyncProgress::Planned {
        sources: sources
            .iter()
            .map(|source| source.registration().display_name)
            .collect(),
    });

    let mut summary = SyncSummary {
        generation_before,
        ..SyncSummary::default()
    };

    let mut sources = sources.into_iter();
    if opencode_available {
        if let Some(opencode) = sources.next() {
            let source_name = opencode.registration().display_name;
            progress(SyncProgress::Started {
                source: source_name.clone(),
            });
            let outcome = sync_one_source(&config.index_path, opencode, mode);
            record_outcome(&mut summary, &mut progress, source_name, outcome, mode);
        }
    }

    let secondary = sources.collect::<Vec<_>>();
    thread::scope(|scope| {
        let (tx, rx) = mpsc::channel();
        for source in secondary {
            let source_name = source.registration().display_name;
            progress(SyncProgress::Started {
                source: source_name.clone(),
            });
            let tx = tx.clone();
            let index_path = config.index_path.clone();
            scope.spawn(move || {
                let outcome = sync_one_source(&index_path, source, mode);
                let _ = tx.send((source_name, outcome));
            });
        }
        drop(tx);

        for (source_name, outcome) in rx {
            record_outcome(&mut summary, &mut progress, source_name, outcome, mode);
        }
    });

    summary.generation_after = UsageIndex::open(&config.index_path)?
        .diagnostics()?
        .generation;
    Ok(summary)
}

type SourceOutcome = std::result::Result<SyncReport, String>;

fn sync_one_source(
    index_path: &std::path::Path,
    source: Box<dyn UsageSource>,
    mode: SyncMode,
) -> SourceOutcome {
    let registration = source.registration();
    let mut index = UsageIndex::open(index_path)
        .map_err(|error| format!("{}: {error:#}", registration.display_name))?;
    source.sync(&mut index, mode).map_err(|error| {
        let message = format!("{}: {error:#}", registration.display_name);
        if let Ok(source_id) = index.register_source(&registration) {
            let _ = index.mark_source_error(source_id, &message);
        }
        message
    })
}

fn record_outcome(
    summary: &mut SyncSummary,
    progress: &mut impl FnMut(SyncProgress),
    source: String,
    outcome: SourceOutcome,
    mode: SyncMode,
) {
    match outcome {
        Ok(report) => {
            let changed = mode == SyncMode::Full
                || report
                    .change
                    .as_ref()
                    .is_some_and(|change| change.start_ms.is_some() || change.end_ms.is_some());
            progress(SyncProgress::Finished {
                source: source.clone(),
                report: Some(report.clone()),
                error: None,
                changed,
            });
            summary.reports.push((source, report));
        }
        Err(error) => {
            progress(SyncProgress::Finished {
                source,
                report: None,
                error: Some(error.clone()),
                changed: false,
            });
            summary.errors.push(error);
        }
    }
}

pub(crate) fn event_key(namespace: &str, native_id: &str) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace.as_bytes());
    hasher.update(&[0]);
    hasher.update(native_id.as_bytes());
    hasher.finalize().as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, fs, time::Duration};

    use rusqlite::Connection;

    use super::*;
    use crate::{
        config::{ColorTheme, ModelAliases, Scope, ThemeScope},
        time_window::{DailyStart, WeekStart},
    };

    #[test]
    fn publishes_opencode_before_starting_parallel_jsonl_sources() {
        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("opencode.db");
        let connection = Connection::open(&db_path).unwrap();
        connection
            .execute_batch(include_str!("../../tests/fixtures/opencode.sql"))
            .unwrap();
        drop(connection);
        let codex_home = directory.path().join("codex");
        let claude_home = directory.path().join("claude");
        let copilot_home = directory.path().join("copilot");
        let pi_sessions_root = directory.path().join("pi");
        fs::create_dir_all(codex_home.join("sessions")).unwrap();
        fs::create_dir_all(claude_home.join("projects")).unwrap();
        fs::create_dir_all(&copilot_home).unwrap();
        fs::create_dir_all(&pi_sessions_root).unwrap();
        let connection = Connection::open(copilot_home.join("session-store.db")).unwrap();
        connection
            .execute_batch(include_str!("../../tests/fixtures/copilot.sql"))
            .unwrap();
        drop(connection);
        let config = Config {
            db_path,
            index_path: directory.path().join("usage.sqlite3"),
            copilot_home,
            codex_home,
            claude_home,
            pi_sessions_root,
            current_directory: directory.path().to_path_buf(),
            config_path: None,
            daily_start: DailyStart::default(),
            week_start: WeekStart::default(),
            refresh_interval: Duration::from_secs(60),
            auto_refresh: true,
            show_comparison: false,
            estimate_api_cost: false,
            hidden_providers: BTreeSet::new(),
            scope: Scope::All,
            color_theme: ColorTheme::Aurora,
            theme_scope: ThemeScope::Calendar,
            aliases: ModelAliases::default(),
        };
        let mut events = Vec::new();

        let summary = sync_configured_with_progress(&config, SyncMode::Incremental, |event| {
            events.push(event)
        })
        .unwrap();

        assert_eq!(
            events.first(),
            Some(&SyncProgress::Planned {
                sources: vec![
                    "OpenCode".to_string(),
                    "Copilot".to_string(),
                    "Pi".to_string(),
                    "Codex".to_string(),
                    "Claude Code".to_string(),
                ]
            })
        );
        let opencode_finished = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    SyncProgress::Finished { source, .. } if source == "OpenCode"
                )
            })
            .unwrap();
        let pi_started = events
            .iter()
            .position(|event| matches!(event, SyncProgress::Started { source } if source == "Pi"))
            .unwrap();
        let copilot_started = events
            .iter()
            .position(
                |event| matches!(event, SyncProgress::Started { source } if source == "Copilot"),
            )
            .unwrap();
        let codex_started = events
            .iter()
            .position(
                |event| matches!(event, SyncProgress::Started { source } if source == "Codex"),
            )
            .unwrap();
        let claude_started = events
            .iter()
            .position(
                |event| matches!(event, SyncProgress::Started { source } if source == "Claude Code"),
            )
            .unwrap();
        let first_secondary_finished = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    SyncProgress::Finished { source, .. }
                        if source == "Copilot" || source == "Pi" || source == "Codex" || source == "Claude Code"
                )
            })
            .unwrap();

        assert!(opencode_finished < pi_started);
        assert!(opencode_finished < copilot_started);
        assert!(opencode_finished < codex_started);
        assert!(opencode_finished < claude_started);
        assert!(copilot_started < first_secondary_finished);
        assert!(pi_started < first_secondary_finished);
        assert!(codex_started < first_secondary_finished);
        assert!(claude_started < first_secondary_finished);
        assert_eq!(summary.reports.len(), 5);
        assert!(summary.errors.is_empty());
    }
}
