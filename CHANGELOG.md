# Changelog

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
