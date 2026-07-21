# expensive

A local terminal dashboard for AI coding harness usage.

<img width="1324" height="958" alt="image" src="https://github.com/user-attachments/assets/027ae18e-7e22-437b-a58c-5a4cc3acf60d" />

`expensive` combines usage from OpenCode, GitHub Copilot, Codex, and Pi into a
small local SQLite index, then serves a live dashboard without repeatedly
parsing session history. Source scans run in the background, so an existing
dashboard remains usable while new accounting data is imported.

The index contains normalized accounting facts and scan checkpoints, not cloned
session histories. Prompts, responses, reasoning text, and tool calls are never
stored by `expensive`.

## What It Shows

- known cost, with unpriced messages called out explicitly and optional
  API-equivalent estimates for supported subscription usage
- total, input, output, cache-read, and cache-write tokens
- model-level message, cost, token, and spend-share breakdowns
- daily, weekly, monthly, all-time, and calendar views
- optional previous-period deltas, biggest model increase, and cost projection
- token or known-cost history graphs
- all-project, current-directory, and individual-project scopes
- configurable compact names for providers and models
- provider visibility filters shared by dashboards, calendars, and reports

Daily usage defaults to "since 04:00" in your local timezone, which fits
late-night coding accounting better than a strict midnight cutoff.

## Install

Install the published crate:

```bash
cargo install --locked expensive
```

For development, install from a local checkout or run it directly:

```bash
cargo install --locked --path .
cargo run
```

## Usage

```bash
expensive
```

Controls:

| Key / Mouse | Action |
| --- | --- |
| `Tab` | Next time window, or next calendar scale |
| `Shift+Tab` | Previous time window, or previous calendar scale |
| click a top tab | Jump to that time window or Calendar |
| `c` | Open Calendar |
| arrows / `hjkl` | Move Calendar selection |
| `Enter` | Open selected Calendar period |
| `h` / `k` | Previous period from Calendar detail |
| `j` / `l` | Next period from Calendar detail |
| `?` | Open or close Help |
| `r` | Refresh the view and incrementally scan sources |
| `R` | Fully rescan sources and rebuild index facts |
| `g` | Toggle the graph between tokens and cost |
| `p` | Choose all, current-directory, or individual-project scope |
| `Esc` | Back one level; quit from the dashboard |
| `q` | Quit |

By default, `expensive` refreshes every 60 seconds. It queries the local index
immediately, then checks sources in a background thread. Unchanged graphs stay
on screen and are only redrawn when their data changes. Day rollover is handled
separately even when automatic refresh is disabled.

The first run publishes OpenCode data as soon as it is ready, then scans
Copilot, Codex, and Pi concurrently while updating the dashboard as each source
completes. Later scans use file identity, size, timestamps, boundary hashes,
and parser checkpoints to read only safe appends. OpenCode fingerprints its
database and WAL, and rechecks a rolling 24-hour mutable window when either
changes. Copilot reads only usage rows beyond its stored high-water mark after
verifying that the already-indexed prefix is unchanged. Press `R` if source
history was rewritten or you want a complete reconciliation.

## Options and Reports

```bash
expensive --daily-start 05:00
expensive --week-start sunday
expensive --color-theme ocean
expensive --theme-scope all
expensive --refresh 10
expensive --no-refresh
expensive --scope current
expensive --db ~/.local/share/opencode/opencode.db
expensive --index ~/.local/share/expensive/usage.sqlite3
```

Check the index, discovered sources, and OpenCode schema compatibility without
opening the TUI:

```bash
expensive doctor
```

Write versioned JSON for scripts and exports:

```bash
expensive report --period weekly --pretty
expensive report --scope current --period all | jq '.totals'
expensive report --from 2026-07-01 --to 2026-08-01 --pretty
```

`--from` is inclusive and `--to` is exclusive. Bounds accept `YYYY-MM-DD`, a
local `YYYY-MM-DDTHH:MM[:SS]`, or RFC 3339. Reports use schema version 3 and
include totals, models, comparison data, projections, token/cost buckets,
provider filters, the API-estimate setting, and the index path. The
`unpriced_messages`, `api_estimated_messages`, and `api_estimated_cost` fields
make the displayed cost basis explicit.

## Sources and Index

Available sources are discovered automatically:

- OpenCode: local SQLite assistant-message accounting
- GitHub Copilot CLI and app: local SQLite usage accounting
- Codex: `sessions` and `archived_sessions` rollout JSONL files
- Pi: version 3 session JSONL files

`expensive` finds the OpenCode database in this order:

1. `--db <path>`
2. `OPENCODE_DB_PATH`
3. `opencode db path`
4. `~/.local/share/opencode/opencode.db`

Copilot uses `COPILOT_HOME/session-store.db`, falling back to
`~/.copilot/session-store.db`. The GitHub Copilot app is built on Copilot CLI
and uses its session storage, so locally persisted app sessions are covered by
the same source. See GitHub's [Copilot configuration-directory
reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-config-dir-reference)
and [app session documentation](https://docs.github.com/en/copilot/how-tos/github-copilot-app/agent-sessions).
Codex uses `CODEX_HOME`, falling back to `~/.codex`. Pi uses
`PI_CODING_AGENT_SESSION_DIR`, then `PI_CODING_AGENT_DIR/sessions`, then
`~/.pi/agent/sessions`. These environment variables can also select custom
source locations.

The normalized index is selected in this order:

1. `--index <path>`
2. `EXPENSIVE_INDEX_PATH`
3. the platform local-data directory, normally
   `~/.local/share/expensive/usage.sqlite3` on Linux

The index uses SQLite WAL mode. It stores timestamps, project/worktree metadata,
provider and model IDs, token categories, known costs, stable event hashes, and
content-free parser cursors. Pi clone/fork copies are deduplicated by stable
entry identity. Removing or rewriting an original artifact is reconciled
without dropping an event still referenced by another artifact.

## Config

If present, config is read from:

```text
~/.config/expensive/config.toml
```

Example:

```toml
daily_start = "04:00"
week_start = "monday"
refresh_seconds = 60
auto_refresh = true
show_comparison = false
estimate_api_cost = false
hidden_providers = []
color_theme = "aurora"
theme_scope = "calendar"
scope = "all"

[provider_aliases]
github-copilot = "gc"

[model_aliases]
"github-copilot/claude-sonnet-4" = "sonnet"
```

`scope` can be `all`, `current`, or `project:<id>`. Press `p` in the TUI to
choose a project and persist its ID without editing TOML. `current` selects the
deepest indexed worktree containing the directory where `expensive` started.

Provider aliases replace only the provider prefix, so `github-copilot/model`
becomes `gc/model`. Model aliases replace the complete displayed model name.
Model keys may be a full `provider/model` (preferred) or a bare model ID. All
matches are exact and aliases affect labels only; raw indexed IDs remain in JSON
reports.

Press `?` to edit the regular settings, provider visibility, and both alias maps
from the TUI. The provider checklist controls every dashboard, graph, calendar,
comparison, projection, and JSON report without deleting indexed data. Alias
editors support add/edit with `Enter`, delete with `d`, and field switching with
`Tab`.

`week_start` can be `monday` or `sunday`. `auto_refresh` and
`show_comparison` can be `true` or `false`; the previous-period panel is hidden
by default. `estimate_api_cost` is also off by default. When enabled, supported
otherwise-unpriced OpenAI usage is added at standard API text-token rates and
marked as estimated; unknown model IDs remain unpriced. Themes are `aurora`,
`ember`, `ocean`, `forest`, and `graphite`.
`theme_scope = "calendar"` applies the theme to the Calendar heatmap only;
`theme_scope = "all"` applies it to the entire TUI.

CLI flags override file values. Settings changed in the TUI are written back to
this file, including the last selected project scope.

## Accounting Notes

OpenCode usage comes from its stored assistant message fields: `cost`, token
categories, `providerID`, `modelID`, and `variant`. Totals should therefore track
OpenCode's own stored accounting without rerunning its slower stats command.

Copilot usage comes from the current version 6 `session-store.db` schema.
Per-request token details preserve input, output, cache-read, cache-write, and
reasoning counts without relying on the database's coarser normalized input
column. Copilot's reported nano-AI-unit total is converted to AI credits and
then to its USD usage value at GitHub's documented `1 AI credit = $0.01` rate.
This represents consumed value, including usage covered by a plan allowance;
it is not necessarily an out-of-pocket charge. Unknown Copilot database schema
versions are rejected instead of being interpreted optimistically. See
GitHub's [Copilot model and pricing
reference](https://docs.github.com/en/copilot/reference/copilot-billing/models-and-pricing).

Codex usage comes from per-request `last_token_usage` facts in rollout logs.
Cached input is treated as a subset of input to avoid double-counting, and
source-reported total tokens remain authoritative. Codex rollouts do not provide
a dollar cost, so those messages are marked unpriced and the summary changes
from `Cost` to `Known Cost`. With `estimate_api_cost = true`, `expensive`
calculates a best-effort API equivalent for recognized OpenAI model IDs using
[OpenAI's documented model rates](https://developers.openai.com/api/docs/models)
as verified on 2026-07-21. Estimated model costs are prefixed with `~`.

API equivalents use standard input, cached-input, output, and cache-write token
rates. They do not reconstruct long-context multipliers, tool-call fees,
regional processing uplifts, priority pricing, or other request-specific
charges, so they should be read as comparisons rather than invoices. The
built-in rate table is intentionally conservative: undocumented variants such
as subscription-only model IDs stay unpriced instead of inheriting a guessed
family rate.

Pi usage comes from assistant entries in version 3 session files. Token
categories and Pi's supplied cost total are indexed; supplied Pi costs are
classified as estimates.

`expensive` validates the required OpenCode and Copilot tables and columns, as
well as OpenCode's SQLite JSON support, before importing them. `expensive
doctor` also reports index schema and generation, indexed
source/artifact/event counts, source locations, optional OpenCode project-scope
support, detected OpenCode versions, and malformed message JSON.

## Compatibility and Releases

The minimum supported Rust version is 1.88. CI checks formatting, Clippy, tests,
crate packaging, and the MSRV. Version tags publish the crate and attach native
archives for Linux x86-64, macOS x86-64/arm64, and Windows x86-64 to the GitHub
release. Maintainer setup is documented in [RELEASING.md](RELEASING.md).

## License

MIT
