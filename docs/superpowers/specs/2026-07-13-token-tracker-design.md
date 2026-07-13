# Token Tracker — personal widget design

## Goal

Evolve Token Tracker into a reliable personal Windows widget first, while
keeping a clean path to a distributable Windows application later. The first
release prioritizes accurate costs and quotas, low resource use, timely quota
alerts, and local day/week history.

## Scope

- Track Claude Code and Codex CLI usage from their local JSONL logs.
- Replace frequent full-tree polling with file-system change notifications.
- Maintain a compact, local, unlimited history of daily usage.
- Show current-day, current-week, and historical usage in the widget.
- Run in the system tray when configured; otherwise exit when the window is
  closed.
- Notify once at each 80%, 90%, 95%, and 100% quota threshold for each
  provider window.

## Non-goals

- No cloud sync, account system, telemetry, remote database, or web dashboard.
- No reconstructed Claude quota: Claude quota remains authoritative only when
  returned by its account endpoint.
- No general reporting engine or arbitrary date-range analytics in the first
  release.

## Architecture

```text
Claude/Codex JSONL logs -- file events --> parser/tailer --> today stats --> widget/tray
                                      \-> daily history ledger
Claude usage endpoint -- periodic --> quota state ------------> widget/alerts

fallback: infrequent recovery scan discovers missed changes and renamed files
```

The watcher identifies changed, created, and renamed log files. The existing
tailer and parsers remain the single ingestion path. Events are debounced
briefly before tailing so a write burst is read once. A conservative periodic
scan remains as recovery for watcher overflows, missed events, and archival
renames; it is not the normal update mechanism.

The worker publishes immutable snapshots to the UI as it does today. The quota
request remains rate-limited and preserves the most recent successful response
on failure.

## Data model and persistence

Application-owned files live in `%LOCALAPPDATA%\\TokenTracker`:

- `settings.json`: close behavior and alert preferences.
- `pricing.json`: user-editable model prices.
- `history.json`: unlimited daily token totals grouped by date, source, and
  model.

The history ledger stores token components, not a frozen price. Costs are
recalculated from the current pricing table, allowing a price correction to
update all history consistently. It stores completed days; on every startup,
the current day is rebuilt from source logs. A ledger update writes a temporary
file and atomically replaces the old file so a crash cannot leave invalid
history behind.

An invalid `pricing.json`, missing fields, negative prices, or non-finite
prices does not replace the last valid table. The widget exposes a concise
configuration-error state. A failed or unavailable quota query keeps the last
good quota value and marks it stale rather than presenting a false value.

Existing `pricing.json` beside the executable is not deleted or modified.

## Widget and tray behavior

The widget remains compact. It offers three lightweight views:

- **Today:** cost, token totals, current quota windows, activity, and hourly
  chart.
- **Week:** current-week totals and daily usage.
- **History:** compact daily/weekly totals from the local ledger.

The tray menu provides show/hide widget, close behavior (exit or minimize to
tray), notification enablement, an action to open `pricing.json`, and quit.
The default close behavior is minimize to tray.

Windows notifications are sent once per quota window when utilization first
crosses 80%, 90%, 95%, and 100%. Notification state is keyed by provider,
quota-window reset time, and threshold. A new reset time opens a fresh set of
thresholds. Notifications identify the provider and its quota window.

## Verification

- Unit tests cover pricing validation and retention of the last good table,
  atomic history writes, rollover, aggregation, watcher event handling and
  recovery scanning, and alert deduplication/reset behavior.
- Existing parsing, tailing, aggregation, and quota parsing tests remain.
- CI runs `cargo test`, `cargo clippy --all-targets -- -D warnings`, and a
  release build on Windows.

## Delivery order

1. Make pricing loading safe and move application-owned files to local app
   data.
2. Add the daily ledger and week/history aggregation.
3. Replace normal polling with watcher-driven ingestion plus recovery scans.
4. Add tray behavior and quota threshold notifications.
5. Add view switching and the compact history UI.

This order keeps every intermediate version usable: data correctness and
recovery come before convenience UI.
