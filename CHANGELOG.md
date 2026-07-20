# Changelog

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
