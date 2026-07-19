//! Usage source adapters.

use anyhow::Result;

use crate::index::{IndexChange, SourceRegistration, UsageIndex};

pub mod opencode;

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
