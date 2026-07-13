# Token Usage Tracker

[![CI](https://github.com/hdtinh57/token-usage-tracker/actions/workflows/ci.yml/badge.svg)](https://github.com/hdtinh57/token-usage-tracker/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/hdtinh57/token-usage-tracker)](https://github.com/hdtinh57/token-usage-tracker/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

A lightweight, realtime desktop widget for Windows that tracks [Claude Code](https://claude.com/claude-code) and [Codex CLI](https://github.com/openai/codex) token usage on your machine — cost, token counts, and quota windows — by reading local session log files directly.

No API keys, no database, no background service, no telemetry. Everything is computed locally from files already on disk.

![Token Usage Tracker screenshot](assets/screenshot-clean.png)

## Features

- **Live cost & token totals** for the current local day (input / output / cache, estimated USD cost)
- **Claude quota bars** — real 5-hour and 7-day usage windows pulled from your Claude account, with time-to-reset
- **Codex quota** tracked from its own session logs
- **Per-request activity feed**, newest first
- **24-hour usage chart**, Claude and Codex stacked per hour
- **Editable, hot-reloaded pricing** via a plain `pricing.json` next to the executable
- Reads existing `.jsonl` session logs only — doesn't modify them, doesn't call any Claude/Codex API to fetch usage data (quota excepted, see below)

## Installation

### Download (recommended)

Grab `token-tracker.exe` and `pricing_default.json` from the [latest release](https://github.com/hdtinh57/token-usage-tracker/releases/latest), put them in the same folder, and run the exe.

### Build from source

Requires the [Rust toolchain](https://rustup.rs/) (stable).

```
git clone https://github.com/hdtinh57/token-usage-tracker.git
cd token-usage-tracker
cargo run --release
```

## Usage

On first launch, a `pricing.json` is created next to the executable with default per-model prices (USD per 1,000,000 tokens). Edit it any time — the app hot-reloads it within ~20s. Model names resolve by longest-prefix match, so routine version bumps (e.g. `claude-sonnet-4-6` → `claude-sonnet-4-7`) keep pricing correct without an edit.

The window is a fixed 360×450, top to bottom:

- **Hero figure** — today's estimated cost, total tokens, and an in/out/cache breakdown, with a live/idle pulse indicator
- **Quota** — Claude's session (5h) and weekly (7d) windows plus Codex's window, each with time-to-reset and a fill bar
- **Activity** — a scrolling feed of individual requests
- **Tokens / hour** — a 24-bar chart of today's usage by local hour

All figures are scoped to **the current local calendar day** and reset at local midnight.

## How it works

A background thread polls two local log roots:

- `%USERPROFILE%\.claude\projects\**\*.jsonl` (Claude Code)
- `%USERPROFILE%\.codex\sessions\**\*.jsonl` (Codex CLI)

using a two-tier schedule — a fast tail of recently-active files (~2s) and a slower directory-discovery sweep (~20s) — and publishes a coalesced in-memory snapshot the UI reads once a second.

### Claude quota polling

Claude's transcripts carry no rate-limit data, so `src/quota.rs` polls the same `api.anthropic.com/api/oauth/usage` endpoint Claude Code's own `/usage` screen reads, authenticated with the OAuth token Claude Code already stores at `%USERPROFILE%\.claude\.credentials.json`. This is the only network call the app makes — read-only, every 3 minutes, and a failed poll (offline, logged out) keeps the last good reading instead of blanking the display.

## Development

```
cargo test               # 56 unit tests: parsing, aggregation, tailing, discovery, quota
cargo build --release
```

Project layout:

| File | Responsibility |
|---|---|
| `src/model.rs` | `UsageEvent`, `Totals`, `Stats` — day-scoped aggregation, rollover, repricing, filtered views |
| `src/pricing.rs` | Pricing table, longest-prefix model matching, hot-reloadable `pricing.json` |
| `src/parse_claude.rs` | Claude Code jsonl line parser |
| `src/parse_codex.rs` | Codex CLI jsonl parser (stateful: model from `turn_context`, tokens from `token_count`) |
| `src/quota.rs` | Polls the account's real Claude 5h/7d quota windows over the Claude Code OAuth token |
| `src/tail.rs` | Partial-line buffering + file offset tracking (truncation-safe) |
| `src/discovery.rs` | Recursive log discovery, mtime-based active-file selection |
| `src/worker.rs` | Background polling loop wiring the above together |
| `src/ui.rs` | `egui` UI |

No database, no file-watch crate (`notify`), no `walkdir` — polling and directory recursion are hand-written to keep the dependency footprint small (see `Cargo.toml`: `chrono`, `eframe`, `serde`, `serde_json`, `ureq`).

## Known limitations

- Single instance only — no cross-process lock
- A rare rename-while-writing race during log archival can drop a trailing line
- Hour-bucket alignment assumes a stable local clock (no DST / manual clock-change correction)
- Windows only

## Contributing

Issues and PRs welcome. Run `cargo test` and `cargo build --release` before submitting — CI runs both on every push and PR.

## License

[MIT](LICENSE)
