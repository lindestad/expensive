//! Usage source adapters.

use anyhow::Result;

use crate::index::{IndexChange, SourceRegistration, UsageIndex};

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

pub(crate) fn event_key(namespace: &str, native_id: &str) -> Vec<u8> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace.as_bytes());
    hasher.update(&[0]);
    hasher.update(native_id.as_bytes());
    hasher.finalize().as_bytes().to_vec()
}
