# expensive

A small terminal dashboard for OpenCode spend.

<img width="1278" height="719" alt="image" src="https://github.com/user-attachments/assets/13e191cd-f0f6-46aa-bbe2-352f66b87d87" />


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

Daily usage defaults to "since 04:00" in your local timezone, which fits the
usual late-night coding accounting better than a strict midnight cutoff.

## Install

From this repo:

```bash
cargo install --path .
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
| `Tab` | Next time window |
| `Shift+Tab` | Previous time window |
| click a top tab | Jump to that time window |
| `r` | Refresh now |
| `q` / `Esc` | Quit |

By default, `expensive` refreshes every 60 seconds. The database query is cheap;
on the machine this was built on, direct SQLite aggregation was roughly two
orders of magnitude faster than parsing `opencode stats`.

## Options

```bash
expensive --daily-start 05:00
expensive --refresh 10
expensive --no-refresh
expensive --db ~/.local/share/opencode/opencode.db
```

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
refresh_seconds = 60
scope = "all"
```

Only `scope = "all"` is supported today.

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

## License

MIT
