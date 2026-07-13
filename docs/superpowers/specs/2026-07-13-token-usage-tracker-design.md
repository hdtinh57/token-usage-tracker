# Token Usage Tracker — Design

## Purpose
Lightweight realtime desktop utility (Windows) that shows Claude Code + Codex CLI token usage (input/output/cache), estimated cost per model, and usage-frequency chart — reading local log files only, no API keys.

## Data sources
- Claude Code: `~/.claude/projects/**/*.jsonl` — `message.model`, `message.usage.{input_tokens,output_tokens,cache_creation_input_tokens,cache_read_input_tokens}`
- Codex CLI: `~/.codex/sessions/**/*.jsonl` — model from `session_meta`/`turn_context.payload.model`; tokens from `event_msg` lines where `payload.type=="token_count"`, using `payload.info.last_token_usage.{input_tokens,cached_input_tokens,output_tokens}` (delta per turn, not the cumulative `total_token_usage`).

## Architecture
Single Rust binary (`eframe`/`egui` GUI), one background worker thread, no database, no external service.

### Two-tier file polling
- **Fast tick (1-2s):** tail only the "active set" — files with mtime within the last ~30 min. Cheap regardless of total historical file count.
- **Slow tick (~20s):** recursive walk of both log roots to discover new files and refresh the active set. A plain walk over thousands of files is still <100ms; no directory-mtime pruning needed.

### Tail robustness
- Track byte offset + leftover partial-line buffer per file path.
- If `current_size < stored_offset`, treat as rewritten: reset offset to 0, re-parse.
- If a tracked file disappears (archived/deleted), drop it from tracking — not an error.
- Only emit a `UsageEvent` for a line once a trailing newline is seen (avoids parsing a partially-written JSON line).

### Startup
Rescan files modified in the last 48h to rebuild today's hourly chart, then hand off to the tail loop. No cache file — reparsing a couple of MB of recent logs is <1s, and this keeps the design DB-free.

### Parsing resilience
Parse each line as `serde_json::Value` first, then pull fields defensively (`.get(...)` chains with zero/None fallback) rather than strict typed structs — a renamed/added field in either tool's log format degrades gracefully (that line's missing fields count as 0) instead of failing the whole line. A model string that doesn't match the pricing table still accumulates tokens with cost=0 and an "unknown pricing" flag, rather than crashing.

### Data model (extensible without a DB)
Atomic unit: `UsageEvent { ts, source: Claude|Codex, model, input, output, cache_read, cache_write }`.

Every "view" is a fold of the event stream into `HashMap<Key, Totals>`:
- `by_model: HashMap<String, Totals>`
- `by_hour: [Totals; 24]` (ring buffer, rolls over at local midnight)
- `by_source: HashMap<Source, Totals>`

`Totals { input, output, cache_read, cache_write, requests, cost }` — `cost` is accumulated at ingest time (tokens × price, once), never recomputed at render time. Adding a future view (e.g. by-project, by-day) means adding one more fold over `UsageEvent`, not a schema change.

### Worker → UI data flow
Worker owns all aggregation state and writes a coalesced snapshot (`Arc<Stats>`) into a `Mutex<Arc<Stats>>` once per fast tick — it does not push per-line messages to the UI. UI calls `ctx.request_repaint_after(Duration::from_secs(1))` and clones the current `Arc` each wake. This decouples log ingestion volume/burstiness from UI update rate.

### Pricing
`pricing.json` next to the executable: `model -> {input, output, cache_read, cache_write}` price per 1M tokens. Ships with current known defaults; user edits the file when providers change pricing (no rebuild needed).

### UI (egui)
- Model combo box filter (default "All")
- Stat rows: Input / Output / Cache read / Cache write / Total tokens / Est. cost
- Line chart (`egui_plot`) of tokens per hour-of-day (24 points), rolling daily

### Error handling
Any file/line-level failure (bad UTF-8, malformed JSON, missing fields) is skipped and logged to stderr; never crashes the app.

### Testing
One `#[test]` per log source parsing a small fixture jsonl, asserting parsed `UsageEvent`s and resulting `Totals`/cost match expected values. No broader suite.

### Dependencies
`eframe`, `egui_plot`, `serde`, `serde_json`, `chrono`. Directory recursion hand-written (~10 lines) instead of adding `walkdir`.
