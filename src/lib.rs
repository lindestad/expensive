//! Terminal dashboard for AI coding harness token usage and spend.
//!
//! `expensive` is primarily a binary crate. It provides a Ratatui-based TUI
//! that normalizes OpenCode, Codex, and Pi accounting into a privacy-preserving
//! local index and aggregates it into daily, weekly, monthly, all-time, and
//! calendar views.
//!
//! The public modules are exposed to keep the binary small and testable. They
//! cover configuration, source adapters, local indexing, time-window
//! calculation, formatting, application state, and terminal rendering.
//!
//! For normal use, install and run the `expensive` binary:
//!
//! ```text
//! cargo install --locked expensive
//! expensive
//! ```

pub mod analytics;
#[doc(hidden)]
pub mod app;
pub mod config;
pub mod db;
pub mod format;
pub mod index;
pub mod report;
#[doc(hidden)]
pub mod sources;
pub mod time_window;
#[doc(hidden)]
pub mod tui;
