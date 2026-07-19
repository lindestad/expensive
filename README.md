# expensive

A small terminal dashboard for OpenCode spend.

<img width="1324" height="958" alt="image" src="https://github.com/user-attachments/assets/027ae18e-7e22-437b-a58c-5a4cc3acf60d" />



`expensive` reads OpenCode's local SQLite database directly and turns the same
kind of accounting you get from `opencode stats` into a live, fast dashboard you
can keep open while you work.

## What It Shows

- total cost
- total tokens
- input and output tokens
- cache read and cache write tokens
- model-level breakdown with message counts, cost, tokens, and share of spend
- daily, weekly, monthly, and all-time views
- calendar view for day, week, and month spend
- previous-period cost and token deltas, biggest model increase, and active-period projection
- token or cost history graphs without reloading data
- all-project, current-directory, and individual-project scopes
- configurable compact names for providers and models

Daily usage defaults to "since 04:00" in your local timezone, which fits the
usual late-night coding accounting better than a strict midnight cutoff.

## Install

Install the published crate:

```bash
cargo install --locked expensive
```

## Development

Install from a local checkout:

```bash
cargo install --locked --path .
```

Or run without installing:

```bash
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
| `r` | Refresh now |
| `g` | Toggle the graph between tokens and cost |
| `p` | Choose all, current-directory, or individual-project scope |
| `Esc` | Back one level; quit from the dashboard |
| `q` | Quit |

By default, `expensive` refreshes every 60 seconds. The database query is cheap;
on the machine this was built on, direct SQLite aggregation was roughly two
orders of magnitude faster than parsing `opencode stats`.

## Options

```bash
expensive --daily-start 05:00
expensive --week-start sunday
expensive --color-theme ocean
expensive --theme-scope all
expensive --refresh 10
expensive --no-refresh
expensive --scope current
expensive --db ~/.local/share/opencode/opencode.db
```

Check database access and schema compatibility without opening the TUI:

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
local `YYYY-MM-DDTHH:MM[:SS]`, or RFC 3339. Reports include totals, models,
comparison data, projections, and token/cost buckets.

`expensive` finds the OpenCode database in this order:

1. `--db <path>`
2. `OPENCODE_DB_PATH`
3. `opencode db path`
4. `~/.local/share/opencode/opencode.db`

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
deepest OpenCode worktree containing the directory where `expensive` started.

Provider aliases replace only the provider prefix, so `github-copilot/model`
becomes `gc/model`. Model aliases replace the complete displayed model name.
Model keys may be a full `provider/model` (preferred) or a bare model ID. All
matches are exact and aliases affect labels only; raw database IDs remain in
JSON reports.

Press `?` to edit the regular settings and both alias maps from the TUI. Alias
editors support add/edit with `Enter`, delete with `d`, and field switching with
`Tab`.

`week_start` can be `monday` or `sunday`. `auto_refresh` and
`show_comparison` can be `true` or `false`; the previous-period panel is hidden
by default. Themes are `aurora`, `ember`,
`ocean`, `forest`, and `graphite`. `theme_scope = "calendar"` applies the
theme to the Calendar heatmap only; `theme_scope = "all"` applies it to the
entire TUI.

CLI flags override file values. Settings changed in the TUI are written back to
this file, including the last selected project scope.

## Notes

The app uses OpenCode's stored assistant message usage fields:

- `cost`
- `tokens.input`
- `tokens.output`
- `tokens.cache.read`
- `tokens.cache.write`
- `providerID`
- `modelID`
- `variant`

That means totals should track OpenCode's own cost and token accounting without
rerunning the slower stats command.

`expensive` validates the required OpenCode tables, columns, and SQLite JSON
support before querying. `expensive doctor` also reports optional project-scope
support, detected OpenCode versions, and malformed message JSON.

## Compatibility and Releases

The minimum supported Rust version is 1.88. CI checks formatting, Clippy, tests,
crate packaging, and the MSRV. Version tags publish the crate and attach native
archives for Linux x86-64, macOS x86-64/arm64, and Windows x86-64 to the GitHub
release. Maintainer setup is documented in [RELEASING.md](RELEASING.md).

## License

MIT
