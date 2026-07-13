# Token Usage Tracker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a lightweight Rust desktop app that reads Claude Code + Codex CLI local log files and shows realtime token usage, cost estimate, and an hourly usage chart — no database, no external service.

**Architecture:** Single binary (`eframe`/`egui`), one background worker thread doing two-tier polling (fast tail of an "active" file set, slow directory discovery), publishing a coalesced `Stats` snapshot the UI thread reads. All aggregation is derived from small in-memory maps keyed by `(Source, model)` — no per-event history is retained.

**Tech Stack:** Rust (stable, 2021 edition), `eframe` + `egui_plot` for GUI/chart, `serde`/`serde_json` for parsing, `chrono` for time handling.

## Global Constraints

- No database, no persisted offsets/cache files, no background OS service — only an in-process worker thread.
- No file-watch crate (e.g. `notify`) — polling only, per spec.
- No `walkdir` — directory recursion is hand-written.
- Dependencies limited to: `eframe`, `egui_plot`, `serde` (+derive), `serde_json`, `chrono`.
- All timestamps in Claude Code and Codex CLI logs are UTC (`Z`-suffixed, RFC 3339); bucketing/day-rollover always uses `chrono::Local` conversion of the event's own `timestamp` field, never file mtime or wall-clock-at-parse-time.
- Aggregates (`by_model`, `by_hour`) are scoped to the current local calendar day only; they reset at local midnight (see Task 3).
- Spec source of truth: `docs/superpowers/specs/2026-07-13-token-usage-tracker-design.md`.

**Note on two refinements made during planning (not in the original spec text, but required for it to work correctly — flagged for visibility, not silently diverging):**
1. The spec described `by_source: HashMap<Source, Totals>` as an independently-ingested aggregate. Storing it that way makes it impossible to re-price correctly after a `pricing.json` change (a source bucket mixes multiple models with different prices, and the per-model breakdown needed to re-price it would already be lost). Instead, `by_model` is keyed by `(Source, String)` and is the only ingested aggregate; `by_source` and "All models" totals are **derived on demand** by folding the (small, bounded-by-distinct-models-seen-today) `by_model` map. Same fix applied to `by_hour`, which is now keyed by `(Source, model)` too — this also makes the hourly chart correctly respect the Model/Source filters, which a single flat `[Totals; 24]` could not do.
2. `pricing.json` is a flat `model -> price` map (matches the spec's own description) rather than a `{"prices": {...}}` wrapper.

---

### Task 1: Project scaffold and core types

**Files:**
- Create: `Cargo.toml` (via `cargo init`)
- Create: `src/main.rs`
- Create: `src/model.rs`

**Interfaces:**
- Produces: `Source` (enum: `Claude`, `Codex`), `UsageEvent` struct, `Totals` struct with `add_tokens`/`merge` methods — used by every later task.

- [ ] **Step 1: Initialize the Cargo project**

Run in `C:\Users\huynh\token-tracker`:
```
cargo init --name token-tracker
```
Expected: creates `Cargo.toml`, `src/main.rs`, appends to existing `.gitignore` (or creates one) — does not touch `docs/`.

- [ ] **Step 2: Add dependencies**

```
cargo add eframe egui_plot
cargo add serde --features derive
cargo add serde_json chrono
```
Expected: `Cargo.toml` now lists all five under `[dependencies]`.

- [ ] **Step 3: Write `src/model.rs` with core types and tests**

```rust
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    Claude,
    Codex,
}

#[derive(Debug, Clone)]
pub struct UsageEvent {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub source: Source,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Totals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub requests: u64,
    pub cost: f64,
}

impl Totals {
    pub fn add_tokens(&mut self, ev: &UsageEvent, cost_delta: f64) {
        self.input += ev.input;
        self.output += ev.output;
        self.cache_read += ev.cache_read;
        self.cache_write += ev.cache_write;
        self.requests += 1;
        self.cost += cost_delta;
    }

    pub fn merge(&mut self, other: &Totals) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.requests += other.requests;
        self.cost += other.cost;
    }

    pub fn total_tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn ev(input: u64, output: u64) -> UsageEvent {
        UsageEvent {
            ts: Utc::now(),
            source: Source::Claude,
            model: "claude-sonnet-4".to_string(),
            input,
            output,
            cache_read: 0,
            cache_write: 0,
        }
    }

    #[test]
    fn add_tokens_accumulates_fields_and_requests() {
        let mut t = Totals::default();
        t.add_tokens(&ev(100, 50), 0.01);
        t.add_tokens(&ev(10, 5), 0.001);
        assert_eq!(t.input, 110);
        assert_eq!(t.output, 55);
        assert_eq!(t.requests, 2);
        assert!((t.cost - 0.011).abs() < 1e-9);
    }

    #[test]
    fn merge_sums_two_totals() {
        let mut a = Totals::default();
        a.add_tokens(&ev(100, 50), 1.0);
        let mut b = Totals::default();
        b.add_tokens(&ev(10, 5), 0.5);
        a.merge(&b);
        assert_eq!(a.input, 110);
        assert_eq!(a.output, 55);
        assert_eq!(a.requests, 2);
        assert!((a.cost - 1.5).abs() < 1e-9);
    }

    #[test]
    fn total_tokens_sums_all_four_components() {
        let mut t = Totals::default();
        t.input = 1;
        t.output = 2;
        t.cache_read = 3;
        t.cache_write = 4;
        assert_eq!(t.total_tokens(), 10);
    }
}
```

- [ ] **Step 4: Wire the module into `src/main.rs`**

```rust
mod model;

fn main() {
    println!("token-tracker scaffold ok");
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test`
Expected: 3 tests pass (`add_tokens_accumulates_fields_and_requests`, `merge_sums_two_totals`, `total_tokens_sums_all_four_components`), 0 failures.

- [ ] **Step 6: Commit**

```
git add Cargo.toml Cargo.lock src/main.rs src/model.rs .gitignore
git commit -m "Scaffold Rust project with core UsageEvent/Totals types"
```

---

### Task 2: Pricing table with longest-prefix lookup and hot-reloadable file

**Files:**
- Create: `src/pricing.rs`
- Create: `pricing_default.json`
- Modify: `src/main.rs` (add `mod pricing;`)

**Interfaces:**
- Consumes: `UsageEvent` (from Task 1, `src/model.rs`).
- Produces: `Price { input, output, cache_read, cache_write }` with `cost_for_tokens(input, output, cache_read, cache_write) -> f64`; `PricingTable` with `lookup(&self, model: &str) -> Option<&Price>` and `load_or_init(path: &std::path::Path) -> std::io::Result<PricingTable>`. Used by Task 3 (`ingest_event`/`reprice`) and Task 9 (worker).

- [ ] **Step 1: Create the default pricing file**

Create `pricing_default.json` at the project root (prices are USD per 1,000,000 tokens; keys are matched as prefixes against the model string seen in logs — see Step 3):

```json
{
  "claude-opus-4": { "input": 15.0, "output": 75.0, "cache_read": 1.5, "cache_write": 18.75 },
  "claude-sonnet-4": { "input": 3.0, "output": 15.0, "cache_read": 0.3, "cache_write": 3.75 },
  "claude-haiku-4": { "input": 0.8, "output": 4.0, "cache_read": 0.08, "cache_write": 1.0 },
  "gpt-5": { "input": 5.0, "output": 15.0, "cache_read": 0.5, "cache_write": 5.0 }
}
```

- [ ] **Step 2: Write the failing tests first, in `src/pricing.rs`**

```rust
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Price {
    pub fn cost_for_tokens(&self, input: u64, output: u64, cache_read: u64, cache_write: u64) -> f64 {
        input as f64 / 1_000_000.0 * self.input
            + output as f64 / 1_000_000.0 * self.output
            + cache_read as f64 / 1_000_000.0 * self.cache_read
            + cache_write as f64 / 1_000_000.0 * self.cache_write
    }
}

#[derive(Debug, Clone, Default)]
pub struct PricingTable {
    prices: HashMap<String, Price>,
}

const DEFAULT_PRICING_JSON: &str = include_str!("../pricing_default.json");

impl PricingTable {
    pub fn from_map(prices: HashMap<String, Price>) -> Self {
        PricingTable { prices }
    }

    pub fn lookup(&self, model: &str) -> Option<&Price> {
        self.prices
            .keys()
            .filter(|k| model.starts_with(k.as_str()))
            .max_by_key(|k| k.len())
            .and_then(|k| self.prices.get(k))
    }

    pub fn load_or_init(path: &Path) -> std::io::Result<PricingTable> {
        if !path.exists() {
            std::fs::write(path, DEFAULT_PRICING_JSON)?;
        }
        let text = std::fs::read_to_string(path)?;
        let map: HashMap<String, Price> = serde_json::from_str(&text).unwrap_or_default();
        Ok(PricingTable::from_map(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_table() -> PricingTable {
        let mut prices = HashMap::new();
        prices.insert(
            "claude-sonnet-4".to_string(),
            Price { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 },
        );
        prices.insert(
            "gpt-5".to_string(),
            Price { input: 5.0, output: 15.0, cache_read: 0.5, cache_write: 5.0 },
        );
        PricingTable::from_map(prices)
    }

    #[test]
    fn longest_prefix_match_survives_minor_version_bumps() {
        let table = sample_table();
        assert!(table.lookup("claude-sonnet-4-6").is_some());
        assert!(table.lookup("claude-sonnet-4-7").is_some());
        assert!(table.lookup("gpt-5.5").is_some());
        assert!(table.lookup("gpt-5.6").is_some());
    }

    #[test]
    fn unrelated_model_is_unknown() {
        let table = sample_table();
        assert!(table.lookup("claude-opus-4-1").is_none());
        assert!(table.lookup("totally-unknown-model").is_none());
    }

    #[test]
    fn cost_for_tokens_computes_expected_dollars() {
        let price = Price { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 };
        // 1,000,000 input + 100,000 output + 500,000 cache_read + 200,000 cache_write
        let cost = price.cost_for_tokens(1_000_000, 100_000, 500_000, 200_000);
        let expected = 3.0 + 1.5 + 0.15 + 0.75;
        assert!((cost - expected).abs() < 1e-9);
    }

    #[test]
    fn load_or_init_writes_default_when_missing_then_loads_it() {
        let dir = std::env::temp_dir().join(format!(
            "tt_pricing_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pricing.json");
        assert!(!path.exists());

        let table = PricingTable::load_or_init(&path).unwrap();
        assert!(path.exists());
        assert!(table.lookup("claude-sonnet-4-6").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
```

- [ ] **Step 3: Wire the module into `src/main.rs`**

```rust
mod model;
mod pricing;

fn main() {
    println!("token-tracker scaffold ok");
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: all Task 1 tests plus `longest_prefix_match_survives_minor_version_bumps`, `unrelated_model_is_unknown`, `cost_for_tokens_computes_expected_dollars`, `load_or_init_writes_default_when_missing_then_loads_it` — all pass.

- [ ] **Step 5: Commit**

```
git add pricing_default.json src/pricing.rs src/main.rs
git commit -m "Add pricing table with longest-prefix model matching and hot-reloadable file"
```

---

### Task 3: Stats aggregation — accounting window, rollover, repricing, filtered views

**Files:**
- Modify: `src/model.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Source`, `UsageEvent`, `Totals` (Task 1); `PricingTable` (Task 2, `crate::pricing::PricingTable`).
- Produces: `Stats::new(today: chrono::NaiveDate) -> Stats`, `Stats::rollover_if_needed(&mut self, today: NaiveDate)`, `ingest_event(stats: &mut Stats, ev: &UsageEvent, pricing: &PricingTable)`, `reprice(stats: &mut Stats, pricing: &PricingTable)`, `totals_for(stats: &Stats, model: Option<&str>, source: Option<Source>) -> Totals`, `hourly_totals_for(stats: &Stats, model: Option<&str>, source: Option<Source>) -> [Totals; 24]`, `Stats::model_keys(&self) -> Vec<(Source, String)>`. All used by Task 9 (worker) and Task 10 (UI).

- [ ] **Step 1: Write the failing tests first, appended to `src/model.rs`** (add to the existing `#[cfg(test)] mod tests` block, and add the non-test code above it as shown in Step 2)

```rust
    // --- Stats / ingest_event / rollover / reprice / views tests ---

    use chrono::{NaiveDate, TimeZone};
    use crate::pricing::{Price, PricingTable};
    use std::collections::HashMap;

    fn event_on(date: NaiveDate, hour: u32, source: Source, model: &str, input: u64) -> UsageEvent {
        let ts = chrono::Local
            .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
            .unwrap()
            .with_timezone(&Utc);
        UsageEvent { ts, source, model: model.to_string(), input, output: 0, cache_read: 0, cache_write: 0 }
    }

    fn table_with_sonnet() -> PricingTable {
        let mut m = HashMap::new();
        m.insert("claude-sonnet-4".to_string(), Price { input: 1.0, output: 1.0, cache_read: 1.0, cache_write: 1.0 });
        PricingTable::from_map(m)
    }

    #[test]
    fn ingest_event_adds_event_matching_current_day() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        let ev = event_on(today, 10, Source::Claude, "claude-sonnet-4-6", 1_000_000);
        ingest_event(&mut stats, &ev, &table_with_sonnet());
        let t = totals_for(&stats, None, None);
        assert_eq!(t.input, 1_000_000);
        assert!((t.cost - 1.0).abs() < 1e-9);
    }

    #[test]
    fn ingest_event_discards_event_from_a_different_day() {
        let today = chrono::Local::now().date_naive();
        let yesterday = today.pred_opt().unwrap();
        let mut stats = Stats::new(today);
        let ev = event_on(yesterday, 10, Source::Claude, "claude-sonnet-4-6", 1_000_000);
        ingest_event(&mut stats, &ev, &table_with_sonnet());
        assert_eq!(totals_for(&stats, None, None).input, 0);
    }

    #[test]
    fn ingest_event_discards_future_dated_event() {
        let today = chrono::Local::now().date_naive();
        let tomorrow = today.succ_opt().unwrap();
        let mut stats = Stats::new(today);
        let ev = event_on(tomorrow, 10, Source::Claude, "claude-sonnet-4-6", 1_000_000);
        ingest_event(&mut stats, &ev, &table_with_sonnet());
        assert_eq!(totals_for(&stats, None, None).input, 0);
    }

    #[test]
    fn rollover_if_needed_clears_state_on_day_change_and_noops_otherwise() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        let ev = event_on(today, 10, Source::Claude, "claude-sonnet-4-6", 500);
        ingest_event(&mut stats, &ev, &table_with_sonnet());
        assert_eq!(totals_for(&stats, None, None).input, 500);

        stats.rollover_if_needed(today); // same day: no-op
        assert_eq!(totals_for(&stats, None, None).input, 500);

        let tomorrow = today.succ_opt().unwrap();
        stats.rollover_if_needed(tomorrow); // day changed: clears
        assert_eq!(totals_for(&stats, None, None).input, 0);
        assert_eq!(stats.current_day, tomorrow);
    }

    #[test]
    fn unknown_model_still_accumulates_tokens_with_zero_cost_and_is_flagged() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        let ev = event_on(today, 10, Source::Claude, "some-brand-new-model", 100);
        ingest_event(&mut stats, &ev, &table_with_sonnet());
        let t = totals_for(&stats, None, None);
        assert_eq!(t.input, 100);
        assert_eq!(t.cost, 0.0);
        assert!(stats.unknown_pricing_models.contains("some-brand-new-model"));
    }

    #[test]
    fn totals_for_filters_by_model_and_source_independently() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        ingest_event(&mut stats, &event_on(today, 1, Source::Claude, "claude-sonnet-4-6", 100), &table_with_sonnet());
        ingest_event(&mut stats, &event_on(today, 2, Source::Codex, "gpt-5.5", 200), &table_with_sonnet());

        assert_eq!(totals_for(&stats, None, None).input, 300);
        assert_eq!(totals_for(&stats, None, Some(Source::Claude)).input, 100);
        assert_eq!(totals_for(&stats, None, Some(Source::Codex)).input, 200);
        assert_eq!(totals_for(&stats, Some("claude-sonnet-4-6"), None).input, 100);
        assert_eq!(totals_for(&stats, Some("gpt-5.5"), None).input, 200);
    }

    #[test]
    fn hourly_totals_for_buckets_by_local_hour_and_respects_filters() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        ingest_event(&mut stats, &event_on(today, 3, Source::Claude, "claude-sonnet-4-6", 100), &table_with_sonnet());
        ingest_event(&mut stats, &event_on(today, 3, Source::Codex, "gpt-5.5", 50), &table_with_sonnet());
        ingest_event(&mut stats, &event_on(today, 9, Source::Claude, "claude-sonnet-4-6", 7), &table_with_sonnet());

        let all = hourly_totals_for(&stats, None, None);
        assert_eq!(all[3].input, 150);
        assert_eq!(all[9].input, 7);
        assert_eq!(all[0].input, 0);

        let claude_only = hourly_totals_for(&stats, None, Some(Source::Claude));
        assert_eq!(claude_only[3].input, 100);
        assert_eq!(claude_only[9].input, 7);
    }

    #[test]
    fn reprice_updates_cost_for_by_model_and_by_hour_from_stored_token_components() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        ingest_event(&mut stats, &event_on(today, 5, Source::Claude, "claude-sonnet-4-6", 1_000_000), &table_with_sonnet());
        assert!((totals_for(&stats, None, None).cost - 1.0).abs() < 1e-9);

        let mut m = HashMap::new();
        m.insert("claude-sonnet-4".to_string(), Price { input: 2.0, output: 2.0, cache_read: 2.0, cache_write: 2.0 });
        let new_pricing = PricingTable::from_map(m);
        reprice(&mut stats, &new_pricing);

        assert!((totals_for(&stats, None, None).cost - 2.0).abs() < 1e-9);
        assert!((hourly_totals_for(&stats, None, None)[5].cost - 2.0).abs() < 1e-9);
    }
```

- [ ] **Step 2: Add the non-test implementation above the test module in `src/model.rs`**

```rust
use std::collections::{HashMap, HashSet};
use chrono::{NaiveDate, Utc};
use crate::pricing::PricingTable;

// ... (Source, UsageEvent, Totals from Task 1 stay unchanged above this) ...

#[derive(Debug, Clone)]
pub struct Stats {
    pub current_day: NaiveDate,
    pub by_model: HashMap<(Source, String), Totals>,
    pub by_hour: HashMap<(Source, String), [Totals; 24]>,
    pub unknown_pricing_models: HashSet<String>,
}

impl Stats {
    pub fn new(today: NaiveDate) -> Self {
        Stats {
            current_day: today,
            by_model: HashMap::new(),
            by_hour: HashMap::new(),
            unknown_pricing_models: HashSet::new(),
        }
    }

    pub fn rollover_if_needed(&mut self, today: NaiveDate) {
        if self.current_day != today {
            self.by_model.clear();
            self.by_hour.clear();
            self.unknown_pricing_models.clear();
            self.current_day = today;
        }
    }

    pub fn model_keys(&self) -> Vec<(Source, String)> {
        let mut keys: Vec<(Source, String)> = self.by_model.keys().cloned().collect();
        keys.sort_by(|a, b| a.1.cmp(&b.1));
        keys
    }
}

pub fn ingest_event(stats: &mut Stats, ev: &UsageEvent, pricing: &PricingTable) {
    let local_ts = ev.ts.with_timezone(&chrono::Local);
    if local_ts.date_naive() != stats.current_day {
        return;
    }
    let hour = local_ts.hour_index();

    let (cost_delta, unknown) = match pricing.lookup(&ev.model) {
        Some(p) => (p.cost_for_tokens(ev.input, ev.output, ev.cache_read, ev.cache_write), false),
        None => (0.0, true),
    };
    if unknown {
        stats.unknown_pricing_models.insert(ev.model.clone());
    }

    let key = (ev.source, ev.model.clone());
    stats.by_model.entry(key.clone()).or_default().add_tokens(ev, cost_delta);
    stats
        .by_hour
        .entry(key)
        .or_insert_with(|| [Totals::default(); 24])[hour]
        .add_tokens(ev, cost_delta);
}

pub fn reprice(stats: &mut Stats, pricing: &PricingTable) {
    for ((_, model), totals) in stats.by_model.iter_mut() {
        if let Some(price) = pricing.lookup(model) {
            totals.cost = price.cost_for_tokens(totals.input, totals.output, totals.cache_read, totals.cache_write);
        }
    }
    for ((_, model), hours) in stats.by_hour.iter_mut() {
        if let Some(price) = pricing.lookup(model) {
            for t in hours.iter_mut() {
                t.cost = price.cost_for_tokens(t.input, t.output, t.cache_read, t.cache_write);
            }
        }
    }
}

pub fn totals_for(stats: &Stats, model: Option<&str>, source: Option<Source>) -> Totals {
    let mut acc = Totals::default();
    for ((src, m), totals) in stats.by_model.iter() {
        if let Some(mf) = model {
            if m != mf {
                continue;
            }
        }
        if let Some(sf) = source {
            if *src != sf {
                continue;
            }
        }
        acc.merge(totals);
    }
    acc
}

pub fn hourly_totals_for(stats: &Stats, model: Option<&str>, source: Option<Source>) -> [Totals; 24] {
    let mut acc = [Totals::default(); 24];
    for ((src, m), hours) in stats.by_hour.iter() {
        if let Some(mf) = model {
            if m != mf {
                continue;
            }
        }
        if let Some(sf) = source {
            if *src != sf {
                continue;
            }
        }
        for h in 0..24 {
            acc[h].merge(&hours[h]);
        }
    }
    acc
}

trait HourIndex {
    fn hour_index(&self) -> usize;
}
impl HourIndex for chrono::DateTime<chrono::Local> {
    fn hour_index(&self) -> usize {
        use chrono::Timelike;
        self.hour() as usize
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test`
Expected: all Task 1 + Task 2 tests plus the 8 new `model` tests pass.

- [ ] **Step 4: Commit**

```
git add src/model.rs
git commit -m "Add Stats aggregation: day-scoped rollover, ingest, reprice, filtered views"
```

---

### Task 4: Claude Code log-line parser

**Files:**
- Create: `src/parse_claude.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Source`, `UsageEvent` (Task 1).
- Produces: `parse_line(line: &str) -> Option<UsageEvent>`. Used by Task 9 (worker).

- [ ] **Step 1: Write the failing tests first**

```rust
use chrono::{DateTime, Utc};
use serde_json::Value;
use crate::model::{Source, UsageEvent};

pub fn parse_line(line: &str) -> Option<UsageEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    let message = v.get("message")?;
    let usage = message.get("usage")?;
    let model = message.get("model")?.as_str()?.to_string();
    let ts_str = v.get("timestamp")?.as_str()?;
    let ts: DateTime<Utc> = DateTime::parse_from_rfc3339(ts_str).ok()?.with_timezone(&Utc);

    let get_u64 = |field: &str| usage.get(field).and_then(|x| x.as_u64()).unwrap_or(0);

    Some(UsageEvent {
        ts,
        source: Source::Claude,
        model,
        input: get_u64("input_tokens"),
        output: get_u64("output_tokens"),
        cache_read: get_u64("cache_read_input_tokens"),
        cache_write: get_u64("cache_creation_input_tokens"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_usage_bearing_assistant_line() {
        let line = r#"{"type":"assistant","timestamp":"2026-07-13T10:15:30.000Z","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":20,"cache_read_input_tokens":10}}}"#;
        let ev = parse_line(line).expect("should parse");
        assert_eq!(ev.model, "claude-sonnet-4-6");
        assert_eq!(ev.input, 100);
        assert_eq!(ev.output, 50);
        assert_eq!(ev.cache_write, 20);
        assert_eq!(ev.cache_read, 10);
        assert_eq!(ev.ts.to_rfc3339(), "2026-07-13T10:15:30+00:00");
    }

    #[test]
    fn non_usage_line_returns_none() {
        let line = r#"{"type":"user","timestamp":"2026-07-13T10:14:00.000Z","message":{"role":"user","content":"hi"}}"#;
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn missing_optional_usage_field_defaults_to_zero_not_none() {
        let line = r#"{"type":"assistant","timestamp":"2026-07-13T10:15:30.000Z","message":{"model":"claude-sonnet-4-6","usage":{"input_tokens":5}}}"#;
        let ev = parse_line(line).expect("should parse: missing fields default to 0, don't fail the line");
        assert_eq!(ev.input, 5);
        assert_eq!(ev.output, 0);
        assert_eq!(ev.cache_read, 0);
        assert_eq!(ev.cache_write, 0);
    }

    #[test]
    fn malformed_json_returns_none_not_a_panic() {
        assert!(parse_line("not json at all {{{").is_none());
    }
}
```

- [ ] **Step 2: Wire the module into `src/main.rs`**

```rust
mod model;
mod pricing;
mod parse_claude;

fn main() {
    println!("token-tracker scaffold ok");
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test`
Expected: all prior tests plus `parses_a_usage_bearing_assistant_line`, `non_usage_line_returns_none`, `missing_optional_usage_field_defaults_to_zero_not_none`, `malformed_json_returns_none_not_a_panic` pass.

- [ ] **Step 4: Commit**

```
git add src/parse_claude.rs src/main.rs
git commit -m "Add Claude Code jsonl line parser"
```

---

### Task 5: Codex CLI log-line parser (stateful: model from turn_context, tokens from token_count)

**Files:**
- Create: `src/parse_codex.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Source`, `UsageEvent` (Task 1).
- Produces: `CodexSessionParser` (one instance per tracked Codex file) with `process_line(&mut self, line: &str) -> Option<UsageEvent>`. Used by Task 9 (worker), one instance per Codex file path.

- [ ] **Step 1: Write the failing tests first**

```rust
use chrono::{DateTime, Utc};
use serde_json::Value;
use crate::model::{Source, UsageEvent};

#[derive(Default)]
pub struct CodexSessionParser {
    current_model: Option<String>,
}

impl CodexSessionParser {
    pub fn process_line(&mut self, line: &str) -> Option<UsageEvent> {
        let v: Value = serde_json::from_str(line).ok()?;
        let event_type = v.get("type")?.as_str()?;

        if event_type == "turn_context" {
            if let Some(model) = v.get("payload").and_then(|p| p.get("model")).and_then(|m| m.as_str()) {
                self.current_model = Some(model.to_string());
            }
            return None;
        }

        if event_type != "event_msg" {
            return None;
        }
        let payload = v.get("payload")?;
        if payload.get("type").and_then(|t| t.as_str()) != Some("token_count") {
            return None;
        }
        let model = self.current_model.clone()?;
        let ts_str = v.get("timestamp")?.as_str()?;
        let ts: DateTime<Utc> = DateTime::parse_from_rfc3339(ts_str).ok()?.with_timezone(&Utc);
        let last = payload.get("info")?.get("last_token_usage")?;
        let get_u64 = |field: &str| last.get(field).and_then(|x| x.as_u64()).unwrap_or(0);

        Some(UsageEvent {
            ts,
            source: Source::Codex,
            model,
            input: get_u64("input_tokens"),
            output: get_u64("output_tokens"),
            cache_read: get_u64("cached_input_tokens"),
            cache_write: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TURN_CONTEXT: &str = r#"{"timestamp":"2026-07-13T10:00:00.000Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5.5"}}"#;
    const TOKEN_COUNT_1: &str = r#"{"timestamp":"2026-07-13T10:00:05.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":20,"reasoning_output_tokens":0,"total_tokens":120},"total_token_usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":20,"reasoning_output_tokens":0,"total_tokens":120}}}}"#;
    const TOKEN_COUNT_2: &str = r#"{"timestamp":"2026-07-13T10:00:10.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":50,"cached_input_tokens":5,"output_tokens":10,"reasoning_output_tokens":0,"total_tokens":60},"total_token_usage":{"input_tokens":150,"cached_input_tokens":15,"output_tokens":30,"reasoning_output_tokens":0,"total_tokens":180}}}}"#;

    #[test]
    fn turn_context_sets_model_and_emits_no_event() {
        let mut p = CodexSessionParser::default();
        assert!(p.process_line(TURN_CONTEXT).is_none());
    }

    #[test]
    fn token_count_before_any_turn_context_is_skipped() {
        let mut p = CodexSessionParser::default();
        assert!(p.process_line(TOKEN_COUNT_1).is_none());
    }

    #[test]
    fn token_count_after_turn_context_yields_event_with_delta_tokens() {
        let mut p = CodexSessionParser::default();
        p.process_line(TURN_CONTEXT);
        let ev = p.process_line(TOKEN_COUNT_1).expect("should parse");
        assert_eq!(ev.model, "gpt-5.5");
        assert_eq!(ev.input, 100);
        assert_eq!(ev.output, 20);
        assert_eq!(ev.cache_read, 10);
        assert_eq!(ev.cache_write, 0);
    }

    #[test]
    fn sum_of_last_token_usage_matches_final_total_token_usage() {
        // Guards against silently reading the wrong field (delta vs cumulative).
        let mut p = CodexSessionParser::default();
        p.process_line(TURN_CONTEXT);
        let ev1 = p.process_line(TOKEN_COUNT_1).unwrap();
        let ev2 = p.process_line(TOKEN_COUNT_2).unwrap();

        let sum_input = ev1.input + ev2.input;
        let sum_output = ev1.output + ev2.output;
        let sum_cache = ev1.cache_read + ev2.cache_read;

        // Final total_token_usage in TOKEN_COUNT_2's fixture: input=150, output=30, cached=15.
        assert_eq!(sum_input, 150);
        assert_eq!(sum_output, 30);
        assert_eq!(sum_cache, 15);
    }

    #[test]
    fn unrelated_event_types_are_skipped() {
        let mut p = CodexSessionParser::default();
        let line = r#"{"timestamp":"2026-07-13T10:00:00.000Z","type":"session_meta","payload":{"id":"abc"}}"#;
        assert!(p.process_line(line).is_none());
    }
}
```

- [ ] **Step 2: Wire the module into `src/main.rs`**

```rust
mod model;
mod pricing;
mod parse_claude;
mod parse_codex;

fn main() {
    println!("token-tracker scaffold ok");
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test`
Expected: all prior tests plus the 5 new `parse_codex` tests pass.

- [ ] **Step 4: Commit**

```
git add src/parse_codex.rs src/main.rs
git commit -m "Add Codex CLI jsonl parser with turn_context/token_count state machine"
```

---

### Task 6: Pure line-splitting buffer (partial-line handling, size cap)

**Files:**
- Create: `src/tail.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `LineSplitter::new()`, `LineSplitter::feed(&mut self, chunk: &[u8]) -> Vec<String>`, `LineSplitter::reset(&mut self)`. Used later in this same file by `Tailer` (Task 7).

- [ ] **Step 1: Write the failing tests first**

```rust
const MAX_LEFTOVER: usize = 1024 * 1024;

pub struct LineSplitter {
    leftover: Vec<u8>,
}

impl LineSplitter {
    pub fn new() -> Self {
        LineSplitter { leftover: Vec::new() }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.leftover.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.leftover.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.leftover.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]).into_owned();
            lines.push(line);
        }
        if self.leftover.len() > MAX_LEFTOVER {
            eprintln!(
                "warning: discarding oversized partial line ({} bytes, no newline seen)",
                self.leftover.len()
            );
            self.leftover.clear();
        }
        lines
    }

    pub fn reset(&mut self) {
        self.leftover.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_with_no_newline_yields_no_lines_yet() {
        let mut s = LineSplitter::new();
        let lines = s.feed(b"partial line no newline");
        assert!(lines.is_empty());
    }

    #[test]
    fn feed_completing_a_partial_line_yields_it() {
        let mut s = LineSplitter::new();
        s.feed(b"hello ");
        let lines = s.feed(b"world\n");
        assert_eq!(lines, vec!["hello world".to_string()]);
    }

    #[test]
    fn feed_with_multiple_lines_in_one_chunk_yields_all() {
        let mut s = LineSplitter::new();
        let lines = s.feed(b"a\nb\nc\n");
        assert_eq!(lines, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
    }

    #[test]
    fn trailing_partial_line_after_complete_ones_is_buffered_not_returned() {
        let mut s = LineSplitter::new();
        let lines = s.feed(b"a\nb\npartial");
        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
        let more = s.feed(b" done\n");
        assert_eq!(more, vec!["partial done".to_string()]);
    }

    #[test]
    fn oversized_leftover_without_newline_is_discarded_not_grown_forever() {
        let mut s = LineSplitter::new();
        let big = vec![b'x'; MAX_LEFTOVER + 10];
        let lines = s.feed(&big);
        assert!(lines.is_empty());
        // After discard, a fresh small valid line should parse cleanly —
        // proving the garbage was dropped, not silently prepended.
        let lines2 = s.feed(b"fresh\n");
        assert_eq!(lines2, vec!["fresh".to_string()]);
    }

    #[test]
    fn reset_clears_buffered_partial_line() {
        let mut s = LineSplitter::new();
        s.feed(b"partial no newline");
        s.reset();
        let lines = s.feed(b"\n");
        assert_eq!(lines, vec!["".to_string()]);
    }
}
```

- [ ] **Step 2: Wire the module into `src/main.rs`**

```rust
mod model;
mod pricing;
mod parse_claude;
mod parse_codex;
mod tail;

fn main() {
    println!("token-tracker scaffold ok");
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test`
Expected: all prior tests plus the 6 new `tail` tests pass.

- [ ] **Step 4: Commit**

```
git add src/tail.rs src/main.rs
git commit -m "Add LineSplitter: buffered partial-line handling with a size cap"
```

---

### Task 7: File tailer (real I/O — offset tracking, truncation correction, deletion)

**Files:**
- Modify: `src/tail.rs` (append `Tailer` below `LineSplitter`)

**Interfaces:**
- Consumes: `LineSplitter` (Task 6, same file).
- Produces: `Tailer::new()`, `Tailer::poll(&mut self, path: &std::path::Path) -> std::io::Result<Vec<String>>`, `Tailer::prime(&mut self, path: &std::path::Path, offset: u64)`. Used by Task 9 (worker).

- [ ] **Step 1: Write the failing tests first, appended to `src/tail.rs`**

```rust
#[cfg(test)]
mod tailer_tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tt_tailer_test_{}_{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("log.jsonl")
    }

    #[test]
    fn poll_reads_lines_written_since_last_offset_only() {
        let path = temp_file("basic");
        std::fs::write(&path, b"line1\nline2\n").unwrap();
        let mut t = Tailer::new();

        let first = t.poll(&path).unwrap();
        assert_eq!(first, vec!["line1".to_string(), "line2".to_string()]);

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"line3\n").unwrap();
        drop(f);

        let second = t.poll(&path).unwrap();
        assert_eq!(second, vec!["line3".to_string()]);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn poll_on_unchanged_file_yields_no_new_lines() {
        let path = temp_file("unchanged");
        std::fs::write(&path, b"line1\n").unwrap();
        let mut t = Tailer::new();
        t.poll(&path).unwrap();
        let again = t.poll(&path).unwrap();
        assert!(again.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn truncation_corrects_offset_in_place_without_duplicating_old_prefix() {
        let path = temp_file("truncate");
        std::fs::write(&path, b"aaaaaaaaaa\nbbbbbbbbbb\n").unwrap();
        let mut t = Tailer::new();
        let first = t.poll(&path).unwrap();
        assert_eq!(first.len(), 2);

        // Simulate truncation: rewrite the file much shorter.
        std::fs::write(&path, b"short\n").unwrap();
        let after_truncate = t.poll(&path).unwrap();
        // Must not re-emit the old prefix, and must not error.
        assert!(after_truncate.is_empty() || after_truncate == vec!["short".to_string()]);

        // Subsequent appends must be picked up normally from the corrected offset.
        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"next\n").unwrap();
        drop(f);
        let after_append = t.poll(&path).unwrap();
        assert!(after_append.contains(&"next".to_string()));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn deleted_file_is_removed_from_tracking_without_error() {
        let path = temp_file("delete");
        std::fs::write(&path, b"line1\n").unwrap();
        let mut t = Tailer::new();
        t.poll(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let result = t.poll(&path).unwrap();
        assert!(result.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn prime_sets_offset_to_skip_already_ingested_content() {
        let path = temp_file("prime");
        std::fs::write(&path, b"already-read-during-startup-rescan\n").unwrap();
        let size = std::fs::metadata(&path).unwrap().len();

        let mut t = Tailer::new();
        t.prime(&path, size);
        let lines = t.poll(&path).unwrap();
        assert!(lines.is_empty(), "primed offset must skip pre-existing content");

        let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"new-line\n").unwrap();
        drop(f);
        let lines2 = t.poll(&path).unwrap();
        assert_eq!(lines2, vec!["new-line".to_string()]);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
```

- [ ] **Step 2: Add the `Tailer` implementation, appended to `src/tail.rs` above the test modules**

```rust
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

struct FileTailState {
    offset: u64,
    splitter: LineSplitter,
}

pub struct Tailer {
    files: HashMap<PathBuf, FileTailState>,
}

impl Tailer {
    pub fn new() -> Self {
        Tailer { files: HashMap::new() }
    }

    pub fn prime(&mut self, path: &Path, offset: u64) {
        self.files.insert(
            path.to_path_buf(),
            FileTailState { offset, splitter: LineSplitter::new() },
        );
    }

    pub fn poll(&mut self, path: &Path) -> std::io::Result<Vec<String>> {
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => {
                self.files.remove(path);
                return Ok(Vec::new());
            }
        };
        let current_size = metadata.len();
        let state = self
            .files
            .entry(path.to_path_buf())
            .or_insert_with(|| FileTailState { offset: 0, splitter: LineSplitter::new() });

        if current_size < state.offset {
            eprintln!(
                "warning: {} shrank ({} -> {} bytes); correcting offset in place, not re-parsing",
                path.display(),
                state.offset,
                current_size
            );
            state.offset = current_size;
            state.splitter.reset();
        }

        if current_size == state.offset {
            return Ok(Vec::new());
        }

        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(state.offset))?;
        let mut buf = vec![0u8; (current_size - state.offset) as usize];
        file.read_exact(&mut buf)?;
        state.offset = current_size;
        Ok(state.splitter.feed(&buf))
    }
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test`
Expected: all prior tests plus the 5 new `tailer_tests` pass.

- [ ] **Step 4: Commit**

```
git add src/tail.rs
git commit -m "Add Tailer: offset tracking with in-place truncation correction"
```

---

### Task 8: Directory discovery (recursive walk, mtime-based active-set selection)

**Files:**
- Create: `src/discovery.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `walk_jsonl_files(root: &Path) -> Vec<PathBuf>`, `collect_mtimes(paths: &[PathBuf]) -> Vec<(PathBuf, SystemTime)>`, `select_active(entries: &[(PathBuf, SystemTime)], within: Duration, now: SystemTime) -> Vec<PathBuf>`. Used by Task 9 (worker).

- [ ] **Step 1: Write the failing tests first**

```rust
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub fn walk_jsonl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_inner(root, &mut out);
    out
}

fn walk_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_inner(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

pub fn collect_mtimes(paths: &[PathBuf]) -> Vec<(PathBuf, SystemTime)> {
    paths
        .iter()
        .filter_map(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .map(|mt| (p.clone(), mt))
        })
        .collect()
}

pub fn select_active(entries: &[(PathBuf, SystemTime)], within: Duration, now: SystemTime) -> Vec<PathBuf> {
    entries
        .iter()
        .filter(|(_, mtime)| {
            now.duration_since(*mtime).unwrap_or(Duration::ZERO) <= within
        })
        .map(|(p, _)| p.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tt_discovery_test_{}_{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn walk_finds_jsonl_files_recursively_and_skips_other_extensions() {
        let root = temp_dir("walk");
        std::fs::create_dir_all(root.join("2026/07/13")).unwrap();
        std::fs::write(root.join("top.jsonl"), b"{}").unwrap();
        std::fs::write(root.join("2026/07/13/rollout.jsonl"), b"{}").unwrap();
        std::fs::write(root.join("notes.txt"), b"ignore me").unwrap();

        let mut found = walk_jsonl_files(&root);
        found.sort();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|p| p.extension().unwrap() == "jsonl"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn walk_on_missing_root_returns_empty_not_an_error() {
        let missing = std::env::temp_dir().join("tt_discovery_definitely_does_not_exist");
        let found = walk_jsonl_files(&missing);
        assert!(found.is_empty());
    }

    #[test]
    fn select_active_filters_by_recency_window() {
        let now = SystemTime::now();
        let recent = now - Duration::from_secs(60);
        let old = now - Duration::from_secs(60 * 60 * 24 * 3); // 3 days ago
        let entries = vec![
            (PathBuf::from("recent.jsonl"), recent),
            (PathBuf::from("old.jsonl"), old),
        ];
        let active = select_active(&entries, Duration::from_secs(30 * 60), now);
        assert_eq!(active, vec![PathBuf::from("recent.jsonl")]);
    }

    #[test]
    fn collect_mtimes_reads_real_file_metadata() {
        let root = temp_dir("mtimes");
        let file = root.join("a.jsonl");
        std::fs::write(&file, b"{}").unwrap();
        let mtimes = collect_mtimes(&[file.clone()]);
        assert_eq!(mtimes.len(), 1);
        assert_eq!(mtimes[0].0, file);
        // Should be recent (created moments ago).
        let age = SystemTime::now().duration_since(mtimes[0].1).unwrap_or(Duration::ZERO);
        assert!(age < Duration::from_secs(60));

        let _ = std::fs::remove_dir_all(&root);
    }
}
```

- [ ] **Step 2: Wire the module into `src/main.rs`**

```rust
mod model;
mod pricing;
mod parse_claude;
mod parse_codex;
mod tail;
mod discovery;

fn main() {
    println!("token-tracker scaffold ok");
}
```

- [ ] **Step 3: Run the tests**

Run: `cargo test`
Expected: all prior tests plus the 4 new `discovery` tests pass.

- [ ] **Step 4: Commit**

```
git add src/discovery.rs src/main.rs
git commit -m "Add recursive jsonl discovery and mtime-based active-set selection"
```

---

### Task 9: Worker — two-tier polling loop wiring everything together

**Files:**
- Create: `src/worker.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: everything from Tasks 1-8 (`model`, `pricing`, `parse_claude`, `parse_codex`, `tail::Tailer`, `discovery::*`).
- Produces: `Worker::new(claude_root, codex_root, pricing_path, snapshot: Arc<Mutex<Arc<Stats>>>) -> std::io::Result<Worker>`, `Worker::startup(&mut self)` (discovery + historical rescan), `Worker::fast_tick(&mut self)`, `Worker::slow_tick(&mut self)`, `Worker::run(self)` (the infinite loop, used only from `main.rs`). Used by Task 10/11 (`main.rs`).

- [ ] **Step 1: Write the failing tests first**

These exercise `startup`, `fast_tick`, and `slow_tick` directly (no thread, no sleep) against real temp directories shaped like the two log roots, so the test is fast and deterministic.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tt_worker_test_{}_{}",
            name,
            SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn today_ts(hour: u32) -> String {
        let today = chrono::Local::now().date_naive();
        let ts = chrono::Local
            .from_local_datetime(&today.and_hms_opt(hour, 0, 0).unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc);
        ts.to_rfc3339()
    }

    use chrono::TimeZone;

    #[test]
    fn startup_rescan_ingests_todays_events_from_both_roots() {
        let claude_root = temp_root("claude");
        let codex_root = temp_root("codex");
        let pricing_path = temp_root("pricing_dir").join("pricing.json");

        let claude_line = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            today_ts(9)
        );
        std::fs::write(claude_root.join("session1.jsonl"), format!("{}\n", claude_line)).unwrap();

        let codex_lines = format!(
            "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t1\",\"model\":\"gpt-5.5\"}}}}\n{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"last_token_usage\":{{\"input_tokens\":40,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_output_tokens\":0,\"total_tokens\":45}},\"total_token_usage\":{{\"input_tokens\":40,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_output_tokens\":0,\"total_tokens\":45}}}}}}}}\n",
            today_ts(9), today_ts(9)
        );
        std::fs::write(codex_root.join("rollout1.jsonl"), codex_lines).unwrap();

        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(chrono::Local::now().date_naive()))));
        let mut worker = Worker::new(claude_root.clone(), codex_root.clone(), pricing_path, snapshot.clone()).unwrap();
        worker.startup();
        worker.publish();

        let stats = snapshot.lock().unwrap().clone();
        let totals = crate::model::totals_for(&stats, None, None);
        assert_eq!(totals.input, 140); // 100 (Claude) + 40 (Codex)

        let _ = std::fs::remove_dir_all(&claude_root);
        let _ = std::fs::remove_dir_all(&codex_root);
    }

    #[test]
    fn fast_tick_picks_up_appended_lines_after_startup() {
        let claude_root = temp_root("claude2");
        let codex_root = temp_root("codex2");
        let pricing_path = temp_root("pricing_dir2").join("pricing.json");
        std::fs::write(claude_root.join("session1.jsonl"), b"").unwrap();

        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(chrono::Local::now().date_naive()))));
        let mut worker = Worker::new(claude_root.clone(), codex_root.clone(), pricing_path, snapshot.clone()).unwrap();
        worker.startup();

        let claude_line = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":7,"output_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            today_ts(11)
        );
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(claude_root.join("session1.jsonl")).unwrap();
        f.write_all(format!("{}\n", claude_line).as_bytes()).unwrap();
        drop(f);

        worker.fast_tick();
        let stats = snapshot.lock().unwrap().clone();
        assert_eq!(crate::model::totals_for(&stats, None, None).input, 7);

        let _ = std::fs::remove_dir_all(&claude_root);
        let _ = std::fs::remove_dir_all(&codex_root);
    }

    #[test]
    fn slow_tick_discovers_a_brand_new_file_created_after_startup() {
        let claude_root = temp_root("claude3");
        let codex_root = temp_root("codex3");
        let pricing_path = temp_root("pricing_dir3").join("pricing.json");

        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(chrono::Local::now().date_naive()))));
        let mut worker = Worker::new(claude_root.clone(), codex_root.clone(), pricing_path, snapshot.clone()).unwrap();
        worker.startup(); // no files exist yet

        let claude_line = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":3,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            today_ts(12)
        );
        std::fs::write(claude_root.join("brand_new.jsonl"), format!("{}\n", claude_line)).unwrap();

        worker.slow_tick(); // discovers brand_new.jsonl, adds it to the active set
        worker.fast_tick(); // tails it
        let stats = snapshot.lock().unwrap().clone();
        assert_eq!(crate::model::totals_for(&stats, None, None).input, 3);

        let _ = std::fs::remove_dir_all(&claude_root);
        let _ = std::fs::remove_dir_all(&codex_root);
    }
}
```

- [ ] **Step 2: Write the implementation, above the test module, in `src/worker.rs`**

```rust
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::Local;

use crate::discovery::{collect_mtimes, select_active, walk_jsonl_files};
use crate::model::{ingest_event, reprice, Stats};
use crate::parse_claude;
use crate::parse_codex::CodexSessionParser;
use crate::pricing::PricingTable;
use crate::tail::Tailer;

const ACTIVE_WINDOW: Duration = Duration::from_secs(30 * 60);
const STARTUP_WINDOW: Duration = Duration::from_secs(48 * 60 * 60);
pub const FAST_TICK: Duration = Duration::from_secs(2);
pub const SLOW_TICK: Duration = Duration::from_secs(20);

pub struct Worker {
    claude_root: PathBuf,
    codex_root: PathBuf,
    pricing_path: PathBuf,
    tailer: Tailer,
    codex_parsers: HashMap<PathBuf, CodexSessionParser>,
    active_set: Vec<PathBuf>,
    pricing: PricingTable,
    pricing_mtime: Option<SystemTime>,
    stats: Stats,
    snapshot: Arc<Mutex<Arc<Stats>>>,
}

impl Worker {
    pub fn new(
        claude_root: PathBuf,
        codex_root: PathBuf,
        pricing_path: PathBuf,
        snapshot: Arc<Mutex<Arc<Stats>>>,
    ) -> std::io::Result<Self> {
        let pricing = PricingTable::load_or_init(&pricing_path)?;
        let pricing_mtime = std::fs::metadata(&pricing_path).and_then(|m| m.modified()).ok();
        let today = Local::now().date_naive();
        Ok(Worker {
            claude_root,
            codex_root,
            pricing_path,
            tailer: Tailer::new(),
            codex_parsers: HashMap::new(),
            active_set: Vec::new(),
            pricing,
            pricing_mtime,
            stats: Stats::new(today),
            snapshot,
        })
    }

    fn all_jsonl_files(&self) -> Vec<PathBuf> {
        let mut all = walk_jsonl_files(&self.claude_root);
        all.extend(walk_jsonl_files(&self.codex_root));
        all
    }

    fn refresh_active_set(&mut self, window: Duration) {
        let all = self.all_jsonl_files();
        let mtimes = collect_mtimes(&all);
        self.active_set = select_active(&mtimes, window, SystemTime::now());
    }

    pub fn startup(&mut self) {
        // Superset prefilter (48h): correctness of "which events count as today"
        // is decided per-event inside ingest_event, not by this window.
        let all = self.all_jsonl_files();
        let mtimes = collect_mtimes(&all);
        let recent = select_active(&mtimes, STARTUP_WINDOW, SystemTime::now());
        for path in &recent {
            self.ingest_whole_file(path);
        }
        // After the historical rescan, the active set for live tailing is the
        // (usually much smaller) last-30-minutes window.
        self.refresh_active_set(ACTIVE_WINDOW);
    }

    fn ingest_whole_file(&mut self, path: &Path) {
        let Ok(bytes) = std::fs::read(path) else { return };
        let is_codex = path.starts_with(&self.codex_root);
        for line in String::from_utf8_lossy(&bytes).lines() {
            self.ingest_line(path, line, is_codex);
        }
        self.tailer.prime(path, bytes.len() as u64);
    }

    fn ingest_line(&mut self, path: &Path, line: &str, is_codex: bool) {
        let event = if is_codex {
            self.codex_parsers
                .entry(path.to_path_buf())
                .or_default()
                .process_line(line)
        } else {
            parse_claude::parse_line(line)
        };
        if let Some(ev) = event {
            ingest_event(&mut self.stats, &ev, &self.pricing);
        }
    }

    pub fn fast_tick(&mut self) {
        let today = Local::now().date_naive();
        self.stats.rollover_if_needed(today);

        let is_codex_root = self.codex_root.clone();
        for path in self.active_set.clone() {
            let is_codex = path.starts_with(&is_codex_root);
            match self.tailer.poll(&path) {
                Ok(lines) => {
                    for line in lines {
                        self.ingest_line(&path, &line, is_codex);
                    }
                }
                Err(e) => eprintln!("warning: failed reading {}: {e}", path.display()),
            }
        }
        self.publish();
    }

    pub fn slow_tick(&mut self) {
        self.refresh_active_set(ACTIVE_WINDOW);
        self.reload_pricing_if_changed();
    }

    fn reload_pricing_if_changed(&mut self) {
        let mtime = std::fs::metadata(&self.pricing_path).and_then(|m| m.modified()).ok();
        if mtime != self.pricing_mtime {
            if let Ok(table) = PricingTable::load_or_init(&self.pricing_path) {
                self.pricing = table;
                self.pricing_mtime = mtime;
                reprice(&mut self.stats, &self.pricing);
            }
        }
    }

    pub fn publish(&self) {
        *self.snapshot.lock().unwrap() = Arc::new(self.stats.clone());
    }

    pub fn run(mut self) {
        self.startup();
        self.publish();
        let mut last_slow_tick = std::time::Instant::now();
        loop {
            std::thread::sleep(FAST_TICK);
            self.fast_tick();
            if last_slow_tick.elapsed() >= SLOW_TICK {
                self.slow_tick();
                last_slow_tick = std::time::Instant::now();
            }
        }
    }
}
```

`Stats` needs `#[derive(Clone)]` (already added in Task 3) so `Arc::new(self.stats.clone())` works.

- [ ] **Step 3: Wire the module into `src/main.rs`**

```rust
mod model;
mod pricing;
mod parse_claude;
mod parse_codex;
mod tail;
mod discovery;
mod worker;

fn main() {
    println!("token-tracker scaffold ok");
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test`
Expected: all prior tests plus `startup_rescan_ingests_todays_events_from_both_roots`, `fast_tick_picks_up_appended_lines_after_startup`, `slow_tick_discovers_a_brand_new_file_created_after_startup` pass.

- [ ] **Step 5: Commit**

```
git add src/worker.rs src/main.rs
git commit -m "Add Worker: two-tier polling loop wiring discovery, tailing, and ingestion"
```

---

### Task 10: UI (egui) and final main.rs wiring

**Files:**
- Create: `src/ui.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `Stats`, `Source`, `totals_for`, `hourly_totals_for` (Task 3); `Worker` (Task 9).
- Produces: `App::new(snapshot: Arc<Mutex<Arc<Stats>>>) -> App` implementing `eframe::App`. Consumed only by `main.rs`.

This task's correctness can't be unit-tested (it's rendering) — the deliverable is verified by running the app, per the step below.

- [ ] **Step 1: Write `src/ui.rs`**

```rust
use eframe::egui;
use egui_plot::{Line, Plot, PlotPoints};
use std::sync::{Arc, Mutex};

use crate::model::{hourly_totals_for, totals_for, Source, Stats};

pub struct App {
    snapshot: Arc<Mutex<Arc<Stats>>>,
    selected_model: Option<String>,
    selected_source: Option<Source>,
}

impl App {
    pub fn new(snapshot: Arc<Mutex<Arc<Stats>>>) -> Self {
        App { snapshot, selected_model: None, selected_source: None }
    }

    fn source_label(source: Option<Source>) -> &'static str {
        match source {
            None => "All",
            Some(Source::Claude) => "Claude Code",
            Some(Source::Codex) => "Codex",
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        let stats = self.snapshot.lock().unwrap().clone();

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Model:");
                egui::ComboBox::from_id_source("model_filter")
                    .selected_text(self.selected_model.as_deref().unwrap_or("All"))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.selected_model, None, "All");
                        let mut models: Vec<String> =
                            stats.model_keys().into_iter().map(|(_, m)| m).collect();
                        models.dedup();
                        for m in models {
                            let label = m.clone();
                            ui.selectable_value(&mut self.selected_model, Some(m), label);
                        }
                    });

                ui.label("Source:");
                egui::ComboBox::from_id_source("source_filter")
                    .selected_text(Self::source_label(self.selected_source))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.selected_source, None, "All");
                        ui.selectable_value(&mut self.selected_source, Some(Source::Claude), "Claude Code");
                        ui.selectable_value(&mut self.selected_source, Some(Source::Codex), "Codex");
                    });
            });

            ui.separator();

            let totals = totals_for(&stats, self.selected_model.as_deref(), self.selected_source);
            ui.label(format!("Input          {}", totals.input));
            ui.label(format!("Output         {}", totals.output));
            ui.label(format!("Cache read     {}", totals.cache_read));
            ui.label(format!("Cache write    {}", totals.cache_write));
            ui.separator();
            ui.label(format!("Total          {}", totals.total_tokens()));
            ui.label(format!("Est. cost      ${:.2}", totals.cost));

            ui.separator();
            ui.label("Tokens / hour (today)");
            let hours = hourly_totals_for(&stats, self.selected_model.as_deref(), self.selected_source);
            let points: PlotPoints = (0..24)
                .map(|h| [h as f64, hours[h].total_tokens() as f64])
                .collect();
            Plot::new("hourly_chart").view_aspect(3.0).show(ui, |plot_ui| {
                plot_ui.line(Line::new(points));
            });
        });
    }
}
```

- [ ] **Step 2: Wire everything into `src/main.rs`**

```rust
mod discovery;
mod model;
mod parse_claude;
mod parse_codex;
mod pricing;
mod tail;
mod ui;
mod worker;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn main() -> eframe::Result<()> {
    let home = std::env::var("USERPROFILE").expect("USERPROFILE not set");
    let claude_root = PathBuf::from(&home).join(".claude").join("projects");
    let codex_root = PathBuf::from(&home).join(".codex").join("sessions");
    let pricing_path = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("pricing.json")))
        .unwrap_or_else(|| PathBuf::from("pricing.json"));

    let today = chrono::Local::now().date_naive();
    let snapshot = Arc::new(Mutex::new(Arc::new(model::Stats::new(today))));

    let worker = worker::Worker::new(claude_root, codex_root, pricing_path, snapshot.clone())
        .expect("failed to initialize worker (check pricing.json permissions)");
    std::thread::spawn(move || worker.run());

    let options = eframe::NativeOptions::default();
    eframe::run_native(
        "Token Usage Tracker",
        options,
        Box::new(move |_cc| Ok(Box::new(ui::App::new(snapshot)))),
    )
}
```

- [ ] **Step 3: Run the full test suite once more**

Run: `cargo test`
Expected: every test from Tasks 1-9 still passes (this task adds no new automated tests — UI is verified manually next).

- [ ] **Step 4: Manual smoke test**

Run: `cargo run --release`
Expected:
- A window titled "Token Usage Tracker" opens.
- Within a couple of seconds, stat rows show non-zero numbers (assuming `~/.claude/projects` or `~/.codex/sessions` has activity from today) or all-zero rows if there's none yet today.
- The Model and Source combo boxes list entries once at least one event has been ingested, and change the displayed numbers and chart when selected.
- Open Claude Code or Codex CLI in another window, send a message, and confirm the numbers update within a few seconds without restarting the tracker.
- Confirm `pricing.json` was created next to the built executable (e.g. `target/release/pricing.json`) with the four default entries from `pricing_default.json`.

- [ ] **Step 5: Commit**

```
git add src/ui.rs src/main.rs
git commit -m "Add egui UI and wire worker thread + main entry point"
```

---

## Self-Review Notes

- **Spec coverage:** two-tier polling (Task 9), tail robustness incl. in-place truncation correction (Task 7), startup/restart consistency + 48h prefilter (Task 9), accounting window / rollover / single ingestion rule (Task 3), parsing resilience for both log formats (Tasks 4-5), pricing + hot-reload + longest-prefix matching (Task 2, Task 9), worker→UI coalesced snapshot (Task 9 `publish`, Task 10 UI read), UI layout (Task 10), Codex delta-vs-cumulative invariant test (Task 5) — all covered.
- **Deviations from the spec doc, both already called out in Global Constraints:** `by_source` is a derived view over `by_model` rather than an independently stored aggregate (fixes a repricing-correctness gap); `pricing.json` is a flat map, not `{"prices": {...}}`.
- **Known limitation surfaced during planning, not present in the spec's "Known limitations" list:** if a Codex `token_count` event is somehow encountered before any `turn_context` line for that file (shouldn't happen in practice since files are read from the start), it's silently skipped rather than attributed to an "unknown" model — consistent with "Parsing resilience" (skip what can't be confidently attributed), but worth knowing if token counts ever look low for a Codex session.
