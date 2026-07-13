use std::collections::{HashMap, HashSet, VecDeque};

use chrono::NaiveDate;

use crate::history::History;
#[cfg(test)]
use crate::history::RawTotals;
use crate::pricing::PricingTable;
use crate::quota::ClaudeQuota;

pub const FEED_CAPACITY: usize = 20;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
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
    /// Soonest account-level quota reset the provider reported alongside this
    /// event (Codex reports it directly; Claude has no such field so this is
    /// always `None` there — Claude's reset is inferred from event spacing).
    pub reset_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Percent of that window's quota already consumed, 0–100, as the provider
    /// reported it (Codex ships this on every event; Claude's comes from
    /// `crate::quota` instead). `None` where unreported.
    pub reset_used_percent: Option<f64>,
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

#[derive(Debug, Clone)]
pub struct FeedEntry {
    pub ts: chrono::DateTime<chrono::Utc>,
    pub source: Source,
    pub model: String,
    pub tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone)]
pub struct Stats {
    pub current_day: NaiveDate,
    pub history: History,
    pub by_model: HashMap<(Source, String), Totals>,
    pub by_hour: HashMap<(Source, String), [Totals; 24]>,
    pub unknown_pricing_models: HashSet<String>,
    /// Most-recent-first is not enforced here — newest is at the back.
    /// Not cleared on day rollover: this is a live activity feed, not a
    /// today-scoped aggregate, so it keeps rolling across midnight.
    pub feed: VecDeque<FeedEntry>,
    /// Quota state, like the feed, is real-time account state rather than a
    /// today-scoped aggregate, so it also survives day rollover.
    pub codex_reset_at: Option<chrono::DateTime<chrono::Utc>>,
    pub codex_used_percent: Option<f64>,
    codex_last_event: Option<chrono::DateTime<chrono::Utc>>,
    /// Reported by the account, not derived from events. `Default` (both
    /// windows `None`) until the first successful poll, and left at the last
    /// good reading if a later poll fails.
    pub claude_quota: ClaudeQuota,
    /// Timestamp of the last successful account quota poll. Kept separately
    /// from window resets so the UI can say when the displayed quota is old.
    pub quota_updated_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Stats {
    pub fn new(today: NaiveDate) -> Self {
        Stats {
            current_day: today,
            history: History::default(),
            by_model: HashMap::new(),
            by_hour: HashMap::new(),
            unknown_pricing_models: HashSet::new(),
            feed: VecDeque::new(),
            codex_reset_at: None,
            codex_used_percent: None,
            codex_last_event: None,
            claude_quota: ClaudeQuota::default(),
            quota_updated_at: None,
        }
    }

    /// When the current Claude 5h session window resets, as the account reports it.
    pub fn claude_reset_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.claude_quota.five_hour.map(|window| window.resets_at)
    }

    /// When the current Claude 7-day window resets, as the account reports it.
    pub fn claude_weekly_reset_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.claude_quota.seven_day.map(|window| window.resets_at)
    }

    pub fn quota_is_stale_at(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        let stale_after = chrono::Duration::from_std(crate::quota::POLL_INTERVAL * 2)
            .expect("quota poll interval fits chrono duration");
        self.quota_updated_at
            .is_none_or(|updated| now - updated > stale_after)
    }

    pub fn rollover_if_needed(&mut self, today: NaiveDate) {
        if self.current_day != today {
            self.by_model.clear();
            self.by_hour.clear();
            self.unknown_pricing_models.clear();
            self.current_day = today;
        }
    }

    fn push_feed(&mut self, ev: &UsageEvent, cost: f64) {
        let entry = FeedEntry {
            ts: ev.ts,
            source: ev.source,
            model: ev.model.clone(),
            tokens: ev.input + ev.output + ev.cache_read + ev.cache_write,
            cost,
        };
        // Events arrive file-by-file (all of one session's lines, then the
        // next), not merged by timestamp, so a plain push_back+cap let a
        // single high-volume source's file flood the last N slots and evict
        // every entry from the other source. Insert by timestamp instead so
        // the cap always keeps the truly most-recent events across sources.
        let pos = self.feed.partition_point(|e| e.ts <= entry.ts);
        self.feed.insert(pos, entry);
        while self.feed.len() > FEED_CAPACITY {
            self.feed.pop_front();
        }
    }

    /// Only Codex's quota comes from events — it ships `resets_at` on each
    /// one. Claude's arrives out-of-band, from `crate::quota`.
    fn track_quota(&mut self, ev: &UsageEvent) -> bool {
        if ev.source != Source::Codex {
            return false;
        }
        // Startup scans files independently, so ingestion order is not
        // event-time order. Only the newest event can describe the current
        // account quota.
        if self.codex_last_event.is_none_or(|last| ev.ts > last) {
            self.codex_last_event = Some(ev.ts);
            self.codex_reset_at = ev.reset_at;
            self.codex_used_percent = ev.reset_used_percent;
            return true;
        }
        false
    }
}

pub fn ingest_event(stats: &mut Stats, ev: &UsageEvent, pricing: &PricingTable) -> bool {
    let (cost_delta, unknown) = match pricing.lookup(&ev.model) {
        Some(p) => (
            p.cost_for_tokens(ev.input, ev.output, ev.cache_read, ev.cache_write),
            false,
        ),
        None => (0.0, true),
    };
    // Feed and quota state are live-account views, not today-scoped
    // aggregates: they update from every parsed event regardless of the day
    // filter below.
    stats.push_feed(ev, cost_delta);
    let codex_quota_updated = stats.track_quota(ev);

    let local_ts = ev.ts.with_timezone(&chrono::Local);
    if local_ts.date_naive() != stats.current_day {
        return codex_quota_updated;
    }
    let hour = local_ts.hour_index();

    if unknown && [ev.input, ev.output, ev.cache_read, ev.cache_write] != [0; 4] {
        stats.unknown_pricing_models.insert(ev.model.clone());
    }

    let key = (ev.source, ev.model.clone());
    stats
        .by_model
        .entry(key.clone())
        .or_default()
        .add_tokens(ev, cost_delta);
    stats
        .by_hour
        .entry(key)
        .or_insert_with(|| [Totals::default(); 24])[hour]
        .add_tokens(ev, cost_delta);
    codex_quota_updated
}

pub fn reprice(stats: &mut Stats, pricing: &PricingTable) {
    for ((_, model), totals) in &mut stats.by_model {
        if let Some(price) = pricing.lookup(model) {
            totals.cost = price.cost_for_tokens(
                totals.input,
                totals.output,
                totals.cache_read,
                totals.cache_write,
            );
        }
    }
    for ((_, model), hours) in &mut stats.by_hour {
        if let Some(price) = pricing.lookup(model) {
            for totals in hours {
                totals.cost = price.cost_for_tokens(
                    totals.input,
                    totals.output,
                    totals.cache_read,
                    totals.cache_write,
                );
            }
        }
    }
}

pub fn totals_for(stats: &Stats, model: Option<&str>, source: Option<Source>) -> Totals {
    let mut acc = Totals::default();
    for ((src, current_model), totals) in &stats.by_model {
        if model.is_some_and(|filter| current_model != filter)
            || source.is_some_and(|filter| *src != filter)
        {
            continue;
        }
        acc.merge(totals);
    }
    acc
}

pub fn hourly_totals_for(
    stats: &Stats,
    model: Option<&str>,
    source: Option<Source>,
) -> [Totals; 24] {
    let mut acc = [Totals::default(); 24];
    for ((src, current_model), hours) in &stats.by_hour {
        if model.is_some_and(|filter| current_model != filter)
            || source.is_some_and(|filter| *src != filter)
        {
            continue;
        }
        for (acc_total, hour_total) in acc.iter_mut().zip(hours) {
            acc_total.merge(hour_total);
        }
    }
    acc
}

#[cfg(test)]
pub fn history_totals_for(
    stats: &Stats,
    start: NaiveDate,
    end: NaiveDate,
    model: Option<&str>,
    source: Option<Source>,
) -> RawTotals {
    stats.history.totals_in(start..=end, model, source)
}

#[cfg(test)]
pub fn weekly_totals_for(
    stats: &Stats,
    pricing: &PricingTable,
    model: Option<&str>,
    source: Option<Source>,
) -> Totals {
    let mut totals = stats.history.priced_totals_in(
        (stats.current_day - chrono::Duration::days(6))..=stats.current_day,
        pricing,
        model,
        source,
    );
    for ((entry_source, entry_model), current) in &stats.by_model {
        if model.is_some_and(|filter| entry_model != filter)
            || source.is_some_and(|filter| *entry_source != filter)
        {
            continue;
        }
        totals.input += current.input;
        totals.output += current.output;
        totals.cache_read += current.cache_read;
        totals.cache_write += current.cache_write;
        totals.requests += current.requests;
        if let Some(price) = pricing.lookup(entry_model) {
            totals.cost += price.cost_for_tokens(
                current.input,
                current.output,
                current.cache_read,
                current.cache_write,
            );
        }
    }
    totals
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{Price, PricingTable};
    use chrono::{NaiveDate, TimeZone, Utc};
    use std::collections::HashMap;

    fn ev(input: u64, output: u64) -> UsageEvent {
        UsageEvent {
            ts: Utc::now(),
            source: Source::Claude,
            model: "claude-sonnet-4".to_string(),
            input,
            output,
            cache_read: 0,
            cache_write: 0,
            reset_at: None,
            reset_used_percent: None,
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
        let t = Totals {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            ..Default::default()
        };
        assert_eq!(t.total_tokens(), 10);
    }

    // --- Stats / ingest_event / rollover / reprice / views tests ---

    fn event_on(date: NaiveDate, hour: u32, source: Source, model: &str, input: u64) -> UsageEvent {
        let ts = chrono::Local
            .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
            .unwrap()
            .with_timezone(&Utc);
        UsageEvent {
            ts,
            source,
            model: model.to_string(),
            input,
            output: 0,
            cache_read: 0,
            cache_write: 0,
            reset_at: None,
            reset_used_percent: None,
        }
    }

    fn table_with_sonnet() -> PricingTable {
        let mut m = HashMap::new();
        m.insert(
            "claude-sonnet-4".to_string(),
            Price {
                input: 1.0,
                output: 1.0,
                cache_read: 1.0,
                cache_write: 1.0,
            },
        );
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
        let ev = event_on(
            yesterday,
            10,
            Source::Claude,
            "claude-sonnet-4-6",
            1_000_000,
        );
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

        stats.rollover_if_needed(today);
        assert_eq!(totals_for(&stats, None, None).input, 500);

        let tomorrow = today.succ_opt().unwrap();
        stats.rollover_if_needed(tomorrow);
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
        assert!(
            stats
                .unknown_pricing_models
                .contains("some-brand-new-model")
        );
    }

    #[test]
    fn zero_token_unknown_model_does_not_invalidate_pricing() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        let ev = event_on(today, 10, Source::Claude, "<synthetic>", 0);

        ingest_event(&mut stats, &ev, &table_with_sonnet());

        assert!(stats.unknown_pricing_models.is_empty());
    }

    #[test]
    fn totals_for_filters_by_model_and_source_independently() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        ingest_event(
            &mut stats,
            &event_on(today, 1, Source::Claude, "claude-sonnet-4-6", 100),
            &table_with_sonnet(),
        );
        ingest_event(
            &mut stats,
            &event_on(today, 2, Source::Codex, "gpt-5.5", 200),
            &table_with_sonnet(),
        );

        assert_eq!(totals_for(&stats, None, None).input, 300);
        assert_eq!(totals_for(&stats, None, Some(Source::Claude)).input, 100);
        assert_eq!(totals_for(&stats, None, Some(Source::Codex)).input, 200);
        assert_eq!(
            totals_for(&stats, Some("claude-sonnet-4-6"), None).input,
            100
        );
        assert_eq!(totals_for(&stats, Some("gpt-5.5"), None).input, 200);
    }

    #[test]
    fn hourly_totals_for_buckets_by_local_hour_and_respects_filters() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        ingest_event(
            &mut stats,
            &event_on(today, 3, Source::Claude, "claude-sonnet-4-6", 100),
            &table_with_sonnet(),
        );
        ingest_event(
            &mut stats,
            &event_on(today, 3, Source::Codex, "gpt-5.5", 50),
            &table_with_sonnet(),
        );
        ingest_event(
            &mut stats,
            &event_on(today, 9, Source::Claude, "claude-sonnet-4-6", 7),
            &table_with_sonnet(),
        );

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
        ingest_event(
            &mut stats,
            &event_on(today, 5, Source::Claude, "claude-sonnet-4-6", 1_000_000),
            &table_with_sonnet(),
        );
        assert!((totals_for(&stats, None, None).cost - 1.0).abs() < 1e-9);

        let mut m = HashMap::new();
        m.insert(
            "claude-sonnet-4".to_string(),
            Price {
                input: 2.0,
                output: 2.0,
                cache_read: 2.0,
                cache_write: 2.0,
            },
        );
        let new_pricing = PricingTable::from_map(m);
        reprice(&mut stats, &new_pricing);

        assert!((totals_for(&stats, None, None).cost - 2.0).abs() < 1e-9);
        assert!((hourly_totals_for(&stats, None, None)[5].cost - 2.0).abs() < 1e-9);
    }

    #[test]
    fn weekly_totals_use_raw_history_with_current_pricing() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap();
        let mut stats = Stats::new(today);
        stats.history.add(
            today - chrono::Duration::days(6),
            Source::Claude,
            "claude-sonnet-4-6",
            crate::history::RawTotals {
                input: 1_000_000,
                requests: 1,
                ..Default::default()
            },
        );
        stats.history.add(
            today - chrono::Duration::days(7),
            Source::Claude,
            "claude-sonnet-4-6",
            crate::history::RawTotals {
                input: 9_000_000,
                requests: 1,
                ..Default::default()
            },
        );
        stats.by_model.insert(
            (Source::Claude, "claude-sonnet-4-6".to_string()),
            Totals {
                input: 1_000_000,
                requests: 1,
                ..Default::default()
            },
        );
        let mut pricing = HashMap::new();
        pricing.insert(
            "claude-sonnet-4".to_string(),
            Price {
                input: 2.0,
                output: 0.0,
                cache_read: 0.0,
                cache_write: 0.0,
            },
        );
        let totals = weekly_totals_for(&stats, &PricingTable::from_map(pricing), None, None);
        assert_eq!(
            history_totals_for(&stats, today - chrono::Duration::days(6), today, None, None).input,
            1_000_000
        );
        assert_eq!(totals.input, 2_000_000);
        assert_eq!(totals.requests, 2);
        assert_eq!(totals.cost, 4.0);
    }

    #[test]
    fn feed_caps_at_capacity_keeping_only_the_most_recent() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        for i in 0..(FEED_CAPACITY as u64 + 5) {
            ingest_event(
                &mut stats,
                &event_on(today, 10, Source::Claude, "claude-sonnet-4-6", i),
                &table_with_sonnet(),
            );
        }
        assert_eq!(stats.feed.len(), FEED_CAPACITY);
        // Oldest 5 (input 0..5) must have been evicted; newest kept.
        assert_eq!(stats.feed.back().unwrap().tokens, FEED_CAPACITY as u64 + 4);
        assert_eq!(stats.feed.front().unwrap().tokens, 5);
    }

    #[test]
    fn feed_receives_events_even_when_discarded_from_todays_aggregate() {
        let today = chrono::Local::now().date_naive();
        let yesterday = today.pred_opt().unwrap();
        let mut stats = Stats::new(today);
        ingest_event(
            &mut stats,
            &event_on(yesterday, 10, Source::Claude, "claude-sonnet-4-6", 42),
            &table_with_sonnet(),
        );
        assert_eq!(totals_for(&stats, None, None).input, 0); // not in today's aggregate
        assert_eq!(stats.feed.len(), 1); // but visible in the live feed
        assert_eq!(stats.feed.back().unwrap().tokens, 42);
    }

    #[test]
    fn feed_stays_time_ordered_even_when_ingested_out_of_order() {
        // Regression: events are ingested file-by-file (all of one session's
        // lines, then the next), not merged by timestamp across sources. A
        // plain push_back+cap let a later-ingested but earlier-timestamped
        // batch evict the true most-recent entries. Feed order must reflect
        // event time, not ingestion order.
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        ingest_event(
            &mut stats,
            &event_on(today, 11, Source::Claude, "claude-sonnet-4-6", 300),
            &table_with_sonnet(),
        );
        ingest_event(
            &mut stats,
            &event_on(today, 9, Source::Codex, "gpt-5.5", 100),
            &table_with_sonnet(),
        );
        ingest_event(
            &mut stats,
            &event_on(today, 10, Source::Codex, "gpt-5.5", 200),
            &table_with_sonnet(),
        );

        let ordered: Vec<u64> = stats.feed.iter().map(|e| e.tokens).collect();
        assert_eq!(ordered, vec![100, 200, 300]);
    }

    #[test]
    fn claude_events_do_not_move_the_quota_window() {
        // Claude's window is server-side state fetched from the account; no
        // amount of local event traffic may invent or shift it.
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        ingest_event(
            &mut stats,
            &event_on(today, 9, Source::Claude, "claude-sonnet-4-6", 1),
            &table_with_sonnet(),
        );
        assert!(stats.claude_reset_at().is_none());
        assert!(stats.claude_weekly_reset_at().is_none());
    }

    #[test]
    fn codex_reset_at_tracks_the_soonest_reported_reset() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        let mut ev = event_on(today, 9, Source::Codex, "gpt-5.5", 1);
        let reset = Utc::now() + chrono::Duration::hours(3);
        ev.reset_at = Some(reset);
        ingest_event(&mut stats, &ev, &table_with_sonnet());
        assert_eq!(stats.codex_reset_at, Some(reset));
    }

    #[test]
    fn quota_state_uses_the_newest_event_not_ingestion_order() {
        let today = chrono::Local::now().date_naive();
        let mut stats = Stats::new(today);
        let mut newest = event_on(today, 10, Source::Codex, "gpt-5.5", 1);
        newest.reset_at = Some(Utc::now() + chrono::Duration::hours(2));
        let mut older = event_on(today, 9, Source::Codex, "gpt-5.5", 1);
        older.reset_at = Some(Utc::now() + chrono::Duration::hours(1));

        ingest_event(&mut stats, &newest, &table_with_sonnet());
        ingest_event(&mut stats, &older, &table_with_sonnet());
        assert_eq!(stats.codex_reset_at, newest.reset_at);
    }

    #[test]
    fn quota_is_stale_after_two_poll_intervals() {
        let mut stats = Stats::new(chrono::Local::now().date_naive());
        let now = Utc::now();
        let stale_after = chrono::Duration::from_std(crate::quota::POLL_INTERVAL * 2).unwrap();
        assert!(stats.quota_is_stale_at(now));
        stats.quota_updated_at = Some(now - stale_after);
        assert!(!stats.quota_is_stale_at(now));
        stats.quota_updated_at = Some(now - stale_after - chrono::Duration::seconds(1));
        assert!(stats.quota_is_stale_at(now));
    }
}
