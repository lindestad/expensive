# Changelog

## Unreleased

- import Claude Code project session usage with incremental JSONL scanning, streaming turn deduplication, thinking token accounting, and 5-minute vs 1-hour cache write distinction
- automatically estimate unpriced Amazon Bedrock Claude usage while gating Anthropic API estimates behind the existing API cost flag
- migrate local usage index to schema version 4 to record 1-hour cache write tokens and support duration-aware prompt caching pricing
- add Claude Code source availability and diagnostics to `expensive doctor`
- import current-schema GitHub Copilot CLI and app usage from the shared local session-store database, including exact token classes and AI-credit usage value
- incrementally scan only new Copilot usage rows while detecting source rewrites and rejecting unknown database schemas

## 0.5.3

- reflow summaries and model usage into width-aware layouts so API estimates, large counts, and model details remain readable in narrow terminals
- shorten tabs, controls, sync status, comparisons, help, and picker entries only as space requires, with explicit ellipses for omitted text
- show compact calendar costs when they fit and omit them at very narrow widths instead of rendering ambiguous fragments

## 0.5.2

- publish OpenCode data first during initial sync, then scan Codex and Pi concurrently while updating the dashboard as each source completes
- add animated first-sync and per-source progress states while keeping partial model data visible and fitting sync status to the available terminal width
- make OpenCode incremental refreshes fast with a 24-hour mutable window, database and WAL fingerprints, unchanged-event write avoidance, and indexed artifact cleanup

## 0.5.1

- add a persisted provider-visibility checklist that filters dashboards, graphs, calendars, comparisons, projections, and JSON reports without deleting indexed data
- optionally estimate otherwise-unpriced OpenAI subscription usage at documented standard API token rates while leaving unknown model variants explicitly unpriced
- mark API-estimated cost in summaries and model rows, expose estimate provenance in report schema version 3, and show active cost/filter settings in `doctor`

## 0.5.0

- add a source-neutral local SQLite index that stores accounting facts and content-free scan checkpoints instead of copied session histories
- import OpenCode, Codex, and Pi usage with safe incremental scans, rewrite reconciliation, and clone/fork deduplication
- query cached usage immediately while refreshing sources in the background; add `R` for a full rescan and handle day rollover independently
- distinguish known spend from unpriced messages when sources such as Codex provide tokens without dollar cost
- expand `doctor` with index and source diagnostics and update JSON reports to schema version 2
- update dependencies within the Rust 1.88 compatibility boundary, including bundled SQLite 3.51.3

## 0.4.0

- add all-project, current-directory, and persisted project scopes with a TUI picker
- add provider and model aliases in TOML and the interactive config editor
- add optional previous-period comparisons, model cost attribution, projections, and cost graphs
- add versioned JSON reports with date bounds and project scopes
- add database schema diagnostics and the `doctor` command
- keep deferred graphs on the same snapshot as their summary and ignore stale scope results
- add CI, Rust 1.88 MSRV coverage, trusted crates.io publishing, and native release archives

## 0.3.1

- avoid redrawing an unchanged graph during automatic refresh
- separate refresh redraw behavior from view transitions
