# Token Usage Tracker

Lightweight realtime desktop app (Windows) that shows [Claude Code](https://claude.com/claude-code) and [Codex CLI](https://github.com/openai/codex) token usage — input/output/cache tokens, estimated cost, and an hourly usage chart — by reading local session log files. No API keys, no database, no background service.

## How it works

A background thread polls two local log roots:

- `%USERPROFILE%\.claude\projects\**\*.jsonl` (Claude Code)
- `%USERPROFILE%\.codex\sessions\**\*.jsonl` (Codex CLI)

using a two-tier schedule — a fast tail of recently-active files (~2s) and a slower directory-discovery sweep (~20s) — and publishes a coalesced in-memory snapshot the UI reads once a second. Everything shown is scoped to **the current local calendar day** and resets at local midnight. Full design rationale (truncation handling, day-rollover semantics, repricing correctness) is in [`docs/superpowers/specs/2026-07-13-token-usage-tracker-design.md`](docs/superpowers/specs/2026-07-13-token-usage-tracker-design.md).

## Running it

```
cargo run --release
```

On first run, a `pricing.json` file is created next to the executable with default per-model prices (USD per 1,000,000 tokens). Edit it any time — prices are: hot-reloaded within ~20s. Model names are matched by longest-prefix, so routine version bumps (e.g. `claude-sonnet-4-6` → `claude-sonnet-4-7`) keep resolving to the right entry without an edit.

## UI

- **Model** / **Source** dropdowns filter both the stat rows and the hourly chart.
- Stat rows: Input / Output / Cache read / Cache write / Total tokens / Est. cost.
- Line chart: tokens per hour-of-day, today only.

## Development

```
cargo test      # 42 unit tests across parsing, aggregation, tailing, and discovery
cargo build --release
```

Project layout:

| File | Responsibility |
|---|---|
| `src/model.rs` | `UsageEvent`, `Totals`, `Stats` — day-scoped aggregation, rollover, repricing, filtered views |
| `src/pricing.rs` | Pricing table, longest-prefix model matching, hot-reloadable `pricing.json` |
| `src/parse_claude.rs` | Claude Code jsonl line parser |
| `src/parse_codex.rs` | Codex CLI jsonl parser (stateful: model from `turn_context`, tokens from `token_count`) |
| `src/tail.rs` | Partial-line buffering + file offset tracking (truncation-safe) |
| `src/discovery.rs` | Recursive log discovery, mtime-based active-file selection |
| `src/worker.rs` | Background polling loop wiring the above together |
| `src/ui.rs` | `egui` UI |

No database, no file-watch crate (`notify`), no `walkdir` — polling and directory recursion are hand-written per the design's dependency budget.

## Known limitations

Single instance only (no cross-process lock), a rare rename-while-writing race during log archival can drop a trailing line, and hour-bucket alignment assumes a stable local clock (no DST/manual-clock-change correction). See the spec's "Known limitations" section for details.
