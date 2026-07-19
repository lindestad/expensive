//! Usage source adapters.

use anyhow::Result;

use crate::{
    config::Config,
    index::{IndexChange, SourceRegistration, UsageIndex},
};

pub mod codex;
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

pub trait UsageSource {
    fn registration(&self) -> SourceRegistration;
    fn sync(&self, index: &mut UsageIndex, mode: SyncMode) -> Result<SyncReport>;
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
    let mut index = UsageIndex::open(&config.index_path)?;
    let generation_before = index.diagnostics()?.generation;
    let mut sources: Vec<Box<dyn UsageSource>> = Vec::new();
    if config.db_path.is_file() {
        sources.push(Box::new(opencode::OpenCodeSource::new(
            config.db_path.clone(),
        )));
    }
    if config.codex_home.join("sessions").is_dir()
        || config.codex_home.join("archived_sessions").is_dir()
    {
        sources.push(Box::new(codex::CodexSource::new(config.codex_home.clone())));
    }
    if config.pi_sessions_root.is_dir() {
        sources.push(Box::new(pi::PiSource::new(config.pi_sessions_root.clone())));
    }

    let mut summary = SyncSummary {
        generation_before,
        ..SyncSummary::default()
    };
    for source in sources {
        let registration = source.registration();
        match source.sync(&mut index, mode) {
            Ok(report) => summary.reports.push((registration.display_name, report)),
            Err(error) => {
                let message = format!("{}: {error:#}", registration.display_name);
                if let Ok(source_id) = index.register_source(&registration) {
                    let _ = index.mark_source_error(source_id, &message);
                }
                summary.errors.push(message);
            }
        }
    }
    summary.generation_after = index.diagnostics()?.generation;
    Ok(summary)
}

pub(crate) fn event_key(namespace: &str, native_id: &str) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace.as_bytes());
    hasher.update(&[0]);
    hasher.update(native_id.as_bytes());
    hasher.finalize().as_bytes().to_vec()
}
