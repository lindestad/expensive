//! Terminal dashboard for OpenCode token usage and spend.
//!
//! `expensive` is primarily a binary crate. It provides a Ratatui-based TUI
//! that reads OpenCode's local SQLite database directly and aggregates assistant
//! message usage into daily, weekly, monthly, all-time, and calendar views.
//!
//! The public modules are exposed to keep the binary small and testable. They
//! cover configuration, OpenCode database aggregation, time-window calculation,
//! formatting, application state, and terminal rendering.
//!
//! The usage data comes from OpenCode's stored assistant message fields such as
//! `cost`, `tokens.input`, `tokens.output`, `tokens.cache.read`,
//! `tokens.cache.write`, `providerID`, `modelID`, and `variant`.
//!
//! For normal use, install and run the `expensive` binary:
//!
//! ```text
//! cargo install --locked expensive
//! expensive
//! ```

#[doc(hidden)]
pub mod app;
pub mod config;
pub mod db;
pub mod format;
pub mod time_window;
#[doc(hidden)]
pub mod tui;
