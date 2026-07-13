# Token Tracker Personal Widget Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver a low-overhead personal Windows Token Tracker with validated local pricing, unlimited local history, event-driven log ingestion, tray behavior, and quota alerts.

**Architecture:** Preserve the existing parsers and tailer as the only ingestion path. Add application data, a daily raw-token ledger, and a watcher that identifies files for the worker to tail; retain an infrequent recovery scan. Publish immutable `Stats` snapshots to the existing egui UI, and keep notification state local and keyed to quota reset windows.

**Tech Stack:** Rust 2024, chrono, serde/serde_json, eframe/egui, ureq, `notify` (Windows `ReadDirectoryChangesW` through `RecommendedWatcher`), `tray-icon`, and `notify-rust` for Windows notifications.

---

## File map

| File | Change |
|---|---|
| `src/paths.rs` | Resolve `%LOCALAPPDATA%\\TokenTracker` and owned-file paths. |
| `src/pricing.rs` | Validate prices and keep the last valid table on reload. |
| `src/history.rs` | Atomically persist daily raw-token totals and aggregate week/history views. |
| `src/watch.rs` | Wrap one recursive `RecommendedWatcher` per log root and debounce changed JSONL paths. |
| `src/alerts.rs` | Decide and persist once-per-threshold quota notifications. |
| `src/settings.rs` | Persist close behavior and notification enablement. |
| `src/tray.rs` | Own the live tray icon and translate menu events into app commands. |
| `src/model.rs` | Expose day/week/history totals without changing parser semantics. |
| `src/worker.rs` | Wire persistence, watcher events, recovery scans, quota freshness, and alerts. |
| `src/ui.rs` | Add Today/Week/History views and visible stale/configuration state. |
| `src/main.rs` | Create app paths, worker, tray integration, and close behavior. |
| `Cargo.toml` | Add only watcher, tray, and notification dependencies. |
| `.github/workflows/ci.yml` | Require tests, Clippy, and release build. |
| `README.md` | Document app-data location, tray behavior, history, and alert thresholds. |

### Task 1: Establish application data and safe pricing

**Files:**
- Create: `src/paths.rs`
- Modify: `src/main.rs`, `src/pricing.rs`, `src/worker.rs`
- Test: inline `#[cfg(test)]` modules in `src/paths.rs` and `src/pricing.rs`

- [ ] **Step 1: Write failing pricing-validation tests**

```rust
#[test]
fn rejects_negative_or_non_finite_prices() {
    assert!(Price::new(-1.0, 1.0, 1.0, 1.0).is_err());
    assert!(Price::new(f64::NAN, 1.0, 1.0, 1.0).is_err());
}

#[test]
fn malformed_reload_keeps_the_previous_table() {
    let valid = r#"{\"gpt-5\":{\"input\":1.0,\"output\":1.0,\"cache_read\":1.0,\"cache_write\":1.0}}"#;
    let previous = PricingTable::parse(valid).unwrap();
    assert!(PricingTable::parse("{").is_err());
    assert!(previous.lookup("gpt-5").is_some());
}
```

- [ ] **Step 2: Run the targeted tests and verify they fail**

Run: `cargo test pricing::tests::rejects_negative_or_non_finite_prices pricing::tests::malformed_reload_keeps_the_previous_table`

Expected: FAIL because `Price::new` and `PricingTable::parse` do not exist.

- [ ] **Step 3: Implement owned paths and validation**

```rust
pub struct AppPaths { pub root: PathBuf, pub pricing: PathBuf, pub settings: PathBuf, pub history: PathBuf }

pub fn app_paths() -> std::io::Result<AppPaths> {
    let root = PathBuf::from(std::env::var("LOCALAPPDATA").or_else(|_| std::env::var("APPDATA"))?)
        .join("TokenTracker");
    std::fs::create_dir_all(&root)?;
    Ok(AppPaths { pricing: root.join("pricing.json"), settings: root.join("settings.json"), history: root.join("history.json"), root })
}
```

Make `PricingTable::parse(&str) -> Result<PricingTable, String>` deserialize then reject every price where `!value.is_finite() || value < 0.0`. Change reload to replace `self.pricing` only after successful parsing; retain `pricing_mtime` only for successful parses so a corrected file retries immediately. Initialize the default pricing in `AppPaths::pricing`, not beside the executable.

- [ ] **Step 4: Run formatting and targeted tests**

Run: `cargo fmt --check; cargo test pricing::tests paths::tests`

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/paths.rs src/pricing.rs src/worker.rs src/main.rs
git commit -m "feat: validate pricing in app data"
```

### Task 2: Add an atomic unlimited daily history ledger

**Files:**
- Create: `src/history.rs`
- Modify: `src/model.rs`, `src/worker.rs`, `src/main.rs`
- Test: inline tests in `src/history.rs`

- [ ] **Step 1: Write failing ledger tests**

```rust
#[test]
fn save_then_load_preserves_raw_tokens_by_model() {
    let day = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap();
    let ledger = History::from_day(day, &stats);
    ledger.save_atomic(&path).unwrap();
    assert_eq!(History::load(&path).unwrap().days[&day].by_model, ledger.days[&day].by_model);
}

#[test]
fn weekly_totals_include_only_the_last_seven_dates() {
    assert_eq!(history.week_total(today).input, expected_input);
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test history::tests`

Expected: FAIL because `History` does not exist.

- [ ] **Step 3: Implement the ledger**

Use serializable types mirroring the token fields already in `Totals`; key daily entries by ISO date string and `(Source, model)`. Implement `save_atomic` by writing `history.json.tmp` in the same directory, calling `sync_all`, then `rename` to `history.json`. On a new local day, save the completed `Stats` day before `rollover_if_needed`; on startup only rebuild today from logs. Calculate current-week cost from raw ledger totals and the current `PricingTable`.

```rust
pub fn save_atomic(&self, path: &Path) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    let file = std::fs::File::create(&tmp)?;
    serde_json::to_writer(&file, self).map_err(std::io::Error::other)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)
}
```

- [ ] **Step 4: Run unit tests**

Run: `cargo test history::tests model::tests`

Expected: PASS; existing model aggregation tests remain green.

- [ ] **Step 5: Commit**

```powershell
git add src/history.rs src/model.rs src/worker.rs src/main.rs
git commit -m "feat: persist daily usage history"
```

### Task 3: Introduce watcher-driven ingestion with recovery scanning

**Files:**
- Create: `src/watch.rs`
- Modify: `Cargo.toml`, `src/worker.rs`, `src/main.rs`
- Test: inline tests in `src/watch.rs` and `src/worker.rs`

- [ ] **Step 1: Write the watcher filtering test**

```rust
#[test]
fn changed_jsonl_paths_are_debounced_and_non_jsonl_paths_are_ignored() {
    let mut pending = PendingPaths::new(Duration::from_millis(250));
    pending.push(PathBuf::from("one.jsonl"), now);
    pending.push(PathBuf::from("one.jsonl"), now);
    pending.push(PathBuf::from("notes.txt"), now);
    assert_eq!(pending.ready(now + Duration::from_millis(250)), vec![PathBuf::from("one.jsonl")]);
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test watch::tests::changed_jsonl_paths_are_debounced_and_non_jsonl_paths_are_ignored`

Expected: FAIL because `PendingPaths` does not exist.

- [ ] **Step 3: Implement the smallest watcher wrapper**

Add `notify` and retain the returned `RecommendedWatcher` for the worker lifetime. Watch both roots recursively. Send `notify::Result<Event>` through a standard channel; accept paths with a `.jsonl` extension from create, modify, and rename events. Debounce with a `HashMap<PathBuf, Instant>`; after 250 ms call the existing `Tailer::poll`/`ingest_line` path. Treat watcher errors as a recovery-scan trigger, not as fatal.

```rust
let mut watcher = notify::recommended_watcher(move |event| { let _ = tx.send(event); })?;
watcher.watch(&claude_root, RecursiveMode::Recursive)?;
watcher.watch(&codex_root, RecursiveMode::Recursive)?;
```

- [ ] **Step 4: Change scan cadence and verify fallback behavior**

Replace the 20-second normal scan with a 10-minute recovery scan. Add a worker test that creates a JSONL file after startup, invokes the recovery path, and proves its event is ingested. Keep the existing tail tests unchanged.

Run: `cargo test watch::tests worker::tests`

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add Cargo.toml Cargo.lock src/watch.rs src/worker.rs src/main.rs
git commit -m "feat: watch usage logs"
```

### Task 4: Add settings and quota alert decisions

**Files:**
- Create: `src/settings.rs`, `src/alerts.rs`
- Modify: `src/model.rs`, `src/worker.rs`, `src/main.rs`
- Test: inline tests in `src/settings.rs` and `src/alerts.rs`

- [ ] **Step 1: Write failing alert-deduplication tests**

```rust
#[test]
fn each_threshold_is_emitted_once_per_reset_window() {
    let reset = Utc::now() + chrono::Duration::hours(1);
    let mut alerts = AlertState::default();
    assert_eq!(alerts.crossed(Source::Claude, "session", 91.0, reset), vec![80, 90]);
    assert!(alerts.crossed(Source::Claude, "session", 96.0, reset).contains(&95));
    assert!(alerts.crossed(Source::Claude, "session", 96.0, reset).is_empty());
}
```

- [ ] **Step 2: Run the tests and verify they fail**

Run: `cargo test alerts::tests settings::tests`

Expected: FAIL because `AlertState` and `Settings` do not exist.

- [ ] **Step 3: Implement durable alert state and settings**

Use `Settings { close_to_tray: bool, notifications_enabled: bool }`, defaulting both to `true`. Store emitted alert keys `(Source, window_name, reset_at, threshold)` in `AlertState` and remove keys whose reset time is no longer current. Persist settings and alert state atomically in the app-data directory. Invoke the decision code after every successful Claude quota refresh and after a newer Codex quota event.

- [ ] **Step 4: Run tests**

Run: `cargo test alerts::tests settings::tests worker::tests`

Expected: PASS.

- [ ] **Step 5: Commit**

```powershell
git add src/settings.rs src/alerts.rs src/model.rs src/worker.rs src/main.rs
git commit -m "feat: persist quota alert state"
```

### Task 5: Add tray commands and Windows notifications

**Files:**
- Create: `src/tray.rs`
- Modify: `Cargo.toml`, `src/main.rs`, `src/worker.rs`
- Test: inline tests in `src/tray.rs` and `src/alerts.rs`

- [ ] **Step 1: Write command translation tests**

```rust
#[test]
fn menu_identifiers_map_to_app_commands() {
    assert_eq!(command_for("show-hide"), Some(TrayCommand::ToggleWindow));
    assert_eq!(command_for("quit"), Some(TrayCommand::Quit));
    assert_eq!(command_for("unknown"), None);
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test tray::tests::menu_identifiers_map_to_app_commands`

Expected: FAIL because `TrayCommand` does not exist.

- [ ] **Step 3: Implement tray lifecycle and notifications**

Add `tray-icon` and `notify-rust`; construct the icon and menu on the native UI thread and keep the `TrayIcon` value alive for the full app lifetime. Provide IDs for show/hide, close-to-tray toggle, notifications toggle, open pricing, and quit. Convert menu events to a small `TrayCommand` enum consumed by the eframe application. Use `notify_rust::Notification::new().summary(...).body(...).show()` only behind `#[cfg(target_os = "windows")]`; discard notification errors after logging them, and never access credentials from notification code.

- [ ] **Step 4: Manually verify native behavior**

Run: `cargo run --release`

Expected: tray icon appears; Show/Hide works; Quit ends the process; a test notification appears when triggered by the alert unit-test harness.

- [ ] **Step 5: Commit**

```powershell
git add Cargo.toml Cargo.lock src/tray.rs src/main.rs src/worker.rs
git commit -m "feat: add tray controls and quota notifications"
```

### Task 6: Add compact Today, Week, and History views

**Files:**
- Modify: `src/ui.rs`, `src/model.rs`, `src/history.rs`, `src/main.rs`
- Test: inline tests in `src/ui.rs` and `src/history.rs`

- [ ] **Step 1: Write aggregation and state tests**

```rust
#[test]
fn history_view_state_cycles_without_affecting_today_totals() {
    let mut view = View::Today;
    view = view.next();
    assert_eq!(view, View::Week);
    assert_eq!(View::History.next(), View::Today);
}
```

- [ ] **Step 2: Run the test and verify it fails**

Run: `cargo test ui::tests::history_view_state_cycles_without_affecting_today_totals`

Expected: FAIL because `View` does not exist.

- [ ] **Step 3: Implement only the three approved views**

Add `View::{Today, Week, History}` and compact text controls at the existing widget header. Today preserves the existing dashboard. Week displays summed ledger plus live-today totals. History displays newest daily entries and weekly summaries from `History`; it does not add arbitrary ranges or a charting library. Show a short visible marker when pricing is invalid or quota data is stale.

- [ ] **Step 4: Run tests and inspect the release widget**

Run: `cargo test ui::tests history::tests; cargo run --release`

Expected: PASS; all views fit in the fixed widget without clipping.

- [ ] **Step 5: Commit**

```powershell
git add src/ui.rs src/model.rs src/history.rs src/main.rs
git commit -m "feat: show weekly and historical usage"
```

### Task 7: Finish release hygiene and documentation

**Files:**
- Modify: `.github/workflows/ci.yml`, `.github/workflows/release.yml`, `README.md`, `.gitignore`

- [ ] **Step 1: Make CI enforce the local verification command**

Add this job step before the release build:

```yaml
- name: Lint
  run: cargo clippy --all-targets -- -D warnings
```

- [ ] **Step 2: Align release and README**

Remove `pricing_default.json` from the release instructions and asset list because the application now initializes `%LOCALAPPDATA%\\TokenTracker\\pricing.json`. Document log-only usage tracking, the sole Claude quota endpoint, history location, watcher recovery scans, tray close behavior, and the four alert thresholds.

- [ ] **Step 3: Add a direct regression check**

Run: `cargo fmt --check; cargo test; cargo clippy --all-targets -- -D warnings; cargo build --release`

Expected: all commands exit 0 on Windows.

- [ ] **Step 4: Commit**

```powershell
git add .github/workflows/ci.yml .github/workflows/release.yml README.md .gitignore
git commit -m "docs: document personal widget behavior"
```

## Final acceptance checklist

- [ ] An invalid or unsafe pricing file never changes the displayed cost to zero or a nonsensical value.
- [ ] Restarting preserves completed daily history and recalculates today from local logs.
- [ ] Normal log updates arrive via watcher; recovery scans safely discover missed/renamed logs.
- [ ] Each provider-window threshold sends exactly one notification until that window resets.
- [ ] Closing follows the saved tray/exit preference.
- [ ] `cargo fmt --check`, tests, Clippy, and release build pass on Windows.
