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

Correction from the prior revision: "drop tracking on truncation" was under-specified — it left open exactly the question now being asked (how does it come back?). The precise rule below replaces it: **correct the offset in place, never drop the entry**. That removes the need for any rediscovery mechanism at all, so there's no gap to describe.

- Track byte offset + leftover partial-line buffer per file path, in memory only (never persisted — see Restart consistency below).
- Normal case, `current_size >= stored_offset`: read `[stored_offset, current_size)`, process complete lines, buffer any trailing partial line, advance `stored_offset` to `current_size`.
- Truncation case, `current_size < stored_offset`: this is abnormal for these append-only logs (real rewrite, or a filesystem race giving a stale/short length read). We cannot retract what's already folded into `Totals` from the old prefix — that's done and irreversible. So we don't try to reconcile old vs. new content at all: log a warning once, **set `stored_offset = current_size` and clear the leftover buffer**, and continue tailing the *same* tracking entry next tick as if `current_size` were the new starting point. No removal from the tracking map, no rediscovery step, no window where the file is untracked. Whatever the process appends after this point is read normally on the very next fast tick. The only cost is that bytes written between "old stored_offset" and "current_size at detection" are neither double-counted nor retroactively recovered — they're simply skipped, which is the safe direction to err in.
- Deletion case (stat fails, path genuinely gone — e.g. moved to `archived_sessions`): remove the entry from the tracking map. This is a true end-of-life for that path; these apps name session files with embedded UUIDs, so a deleted path is never reused for different content, and there is nothing further to ingest from it.
- Only emit a `UsageEvent` for a line once a trailing newline is seen (avoids parsing a partially-written JSON line). Cap the leftover-buffer at 1MB; if a "line" exceeds that without a newline, discard the buffer and log a warning instead of growing it unbounded (protects against a corrupt/never-terminated line).

### Startup and restart consistency
Rescan files with mtime in the last 48h (a cheap superset prefilter) then hand off to the tail loop. No cache file, no persisted offsets — every process start rebuilds all in-memory state from raw logs. This is deliberate: persisting offsets across restarts is exactly the mechanism that causes classic double-count/missed-line bugs (stale offset vs. a file that got truncated, renamed, or edited while the app was down). Recomputing from source on every start trades a sub-second reparse for eliminating that whole bug class.

Which events actually count is decided per-event, not per-file: see "Accounting window" below. The 48h file-selection window is only a performance prefilter and is intentionally generous — correctness never depends on it being exact.

### Accounting window (today, local time)
All three aggregates (`by_model`, `by_source`, `by_hour`) are scoped to **the current local calendar day** — not "process lifetime" and not "last 48h". This removes an inconsistency in the original design where `by_hour` reset daily but `by_model`/`by_source` totals implicitly kept growing for as long as the process stayed running, so displayed lifetime cost would jump around depending on how long the app happened to be up when you looked, and would not match after a restart.

Rule: every log line carries its own `timestamp` field (UTC, `Z`-suffixed) in both Claude Code and Codex CLI formats. Parse it, convert to local time via `chrono::Local`, and use that — never the file's mtime, never wall-clock-at-parse-time — to decide (a) which hour bucket an event belongs to and (b) whether it belongs to "today" at all.

**Two different clocks, two different jobs — do not conflate them:**
- *Ingestion time* (wall clock, `Local::now()`) only decides *when the rollover check runs*. It plays no role in deciding which bucket an event lands in.
- *Event timestamp* (parsed from the log line, converted to local time) is the *only* input to (a) which hour bucket an event goes into and (b) whether it counts at all today.

Rollover check, O(1), not a rescan: the worker keeps `current_day: NaiveDate`, initialized to `Local::now().date()` at process start (including on a restart — see below). On each fast tick, compare `current_day` to `Local::now().date()`. If different, zero out `by_model`/`by_source`/`by_hour` and update `current_day`.

**Single ingestion rule, used identically by startup rescan and live tail** — this is what guarantees the two paths can't drift apart: every `UsageEvent`, regardless of where it came from, passes through one function:

```
if event.local_date() == current_day { fold event into by_model/by_source/by_hour }
else { discard event }
```

There is no separate "historical mode" for the startup rescan — it feeds the 48h-prefiltered lines through this exact same check. Concretely:
- **Startup rebuild**: `current_day` is set once, before any file is read. The rescan will typically encounter lines from yesterday (e.g. a session file untouched since last night still falls inside the 48h prefilter) — those are compared against `current_day` and discarded, same as they would be during live tail. Only lines whose own timestamp is actually today survive into the rebuilt aggregates.
- **Late-arriving "today" events** (read a while after being written, whether via slow-tick discovery or startup rescan): still added normally — lateness of *ingestion* doesn't matter, only the event's own date does.
- **Events from a previous day** arriving during live tail (e.g. a session still flushing lines dated just before midnight, read just after rollover already fired): discarded. Worked example — a line timestamped `23:59:58` local: if it's read and folded *before* the rollover check advances `current_day`, it lands in today's hour-23 bucket; if it's read *after* rollover already advanced `current_day` to the next date, it's discarded. This is a deliberate, bounded, one-sided loss (never a double count) — accepted because this is a "today" dashboard, not an audit-grade historical ledger.
- **Future-dated events** (system clock skew on the writer's side): `event.local_date() == current_day` is false for a future date too, so these are discarded by the same rule — no special-case needed.

### Parsing resilience
Parse each line as `serde_json::Value` first, then pull fields defensively (`.get(...)` chains with zero/None fallback) rather than strict typed structs — a renamed/added field in either tool's log format degrades gracefully (that line's missing fields count as 0) instead of failing the whole line. Model-name matching against the pricing table is **longest-prefix**, not exact-string — a routine model version bump (e.g. `claude-sonnet-4-6` → `claude-sonnet-4-7`, `gpt-5.5` → `gpt-5.6`) still resolves to the right price tier instead of spuriously flipping to "unknown pricing". Only a genuinely new model family falls back to cost=0 + an "unknown pricing" flag shown in the UI, rather than crashing.

### Data model (extensible without a DB)
Atomic unit: `UsageEvent { ts, source: Claude|Codex, model, input, output, cache_read, cache_write }`.

Every "view" is a fold of the event stream into `HashMap<Key, Totals>`:
- `by_model: HashMap<String, Totals>`
- `by_hour: [Totals; 24]` (index = local hour-of-day of the event's own timestamp; see Accounting window)
- `by_source: HashMap<Source, Totals>`

`Totals { input, output, cache_read, cache_write, requests, cost }` — raw token components are always retained alongside `cost`, not just the derived dollar figure. `cost` is computed at ingest time (tokens × price) and added once; it is not recomputed at render time. It **is** recomputed on pricing reload (see Pricing below), which is possible precisely because the raw components are kept, not discarded after the first calculation. Adding a future view (e.g. by-project) means adding one more fold over `UsageEvent`, not a schema change.

### Worker → UI data flow
Worker owns all aggregation state and writes a coalesced snapshot (`Arc<Stats>`) into a `Mutex<Arc<Stats>>` once per fast tick — it does not push per-line messages to the UI. UI calls `ctx.request_repaint_after(Duration::from_secs(1))` and clones the current `Arc` each wake. This decouples log ingestion volume/burstiness from UI update rate.

### Pricing
`pricing.json` next to the executable: `model -> {input, output, cache_read, cache_write}` price per 1M tokens. Ships with current known defaults; user edits the file when providers change pricing (no rebuild needed).

Hot-reload piggybacks on the existing slow tick (~20s): check `pricing.json`'s mtime, and if changed, reload the table and recompute `cost` for every existing `Totals` entry from its already-stored raw token components × the new prices. This is O(number of distinct models/hours/sources) — a handful of entries — never O(number of events processed so far), so it stays cheap regardless of log volume. Cost already accrued reflects whatever price was current at ingest time until the next reload; a reload re-prices the whole current "today" window under the new price rather than leaving it split across two price regimes.

### UI (egui)
Single resizable window, no tabs/menu, system light/dark theme (egui default, no custom theming).

```
┌────────────────────────────────────────────────┐
│ Token Usage Tracker                     [_][□][X]│
├────────────────────────────────────────────────┤
│ Model: [ All ▾ ]        Source: [ All ▾ ]         │
├────────────────────────────────────────────────┤
│  Input           1,234,567 tok                    │
│  Output            234,567 tok                    │
│  Cache read      2,345,678 tok                    │
│  Cache write       123,456 tok                    │
│  ──────────────────────────                      │
│  Total           3,938,268 tok                    │
│  Est. cost              $12.34                    │
├────────────────────────────────────────────────┤
│ Tokens / hour (today)                              │
│  [line chart, x = hour 0-23, y = tokens]           │
└────────────────────────────────────────────────┘
```

- Two combo boxes filter both the stat rows and the chart: **Model** (All / each model seen today) and **Source** (All / Claude Code / Codex).
- Stat rows are plain label + number, no gauges/progress bars — scannable at a glance.
- Chart: `egui_plot` line, x = local hour-of-day (0-23), y = tokens that hour, redrawn from the latest snapshot each UI wake (~1s).

### Error handling
Any file/line-level failure (bad UTF-8, malformed JSON, missing fields) is skipped and logged to stderr; never crashes the app.

### Testing
One `#[test]` per log source parsing a small fixture jsonl, asserting parsed `UsageEvent`s and resulting `Totals`/cost match expected values. Additionally, for Codex: assert that summing `last_token_usage` across all `token_count` events in a session fixture equals the final event's `total_token_usage` — this guards against silently mis-reading "delta since last turn" vs "cumulative for session" if Codex ever reorders/renames these fields, which would otherwise under- or over-count without any visible error. No broader suite.

### Known limitations (accepted, not solved)
- **Single instance assumed.** No cross-process locking; running two copies would double-count via independent offset tracking. Not handled — out of scope for a single-user desktop utility.
- **Rename-while-writing race.** If a session file is archived/moved at the exact moment a new line is appended, polling (vs. OS file-change events) could miss that last line. Accepted tradeoff of a polling-only design; not solvable without added complexity (e.g. `notify`), which this design deliberately avoids.
- **System clock changes / DST.** Hour-bucket alignment assumes a stable local clock during the day. A manual clock change or DST transition can shift bucket boundaries for that day. No NTP-style correction is attempted.

### Dependencies
`eframe`, `egui_plot`, `serde`, `serde_json`, `chrono`. Directory recursion hand-written (~10 lines) instead of adding `walkdir`.
