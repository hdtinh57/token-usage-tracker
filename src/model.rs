use std::collections::{HashMap, HashSet, VecDeque};

use chrono::NaiveDate;

use crate::pricing::PricingTable;

pub const FEED_CAPACITY: usize = 20;

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
    pub by_model: HashMap<(Source, String), Totals>,
    pub by_hour: HashMap<(Source, String), [Totals; 24]>,
    pub unknown_pricing_models: HashSet<String>,
    /// Most-recent-first is not enforced here — newest is at the back.
    /// Not cleared on day rollover: this is a live activity feed, not a
    /// today-scoped aggregate, so it keeps rolling across midnight.
    pub feed: VecDeque<FeedEntry>,
}

impl Stats {
    pub fn new(today: NaiveDate) -> Self {
        Stats {
            current_day: today,
            by_model: HashMap::new(),
            by_hour: HashMap::new(),
            unknown_pricing_models: HashSet::new(),
            feed: VecDeque::new(),
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

    fn push_feed(&mut self, ev: &UsageEvent, cost: f64) {
        self.feed.push_back(FeedEntry {
            ts: ev.ts,
            source: ev.source,
            model: ev.model.clone(),
            tokens: ev.input + ev.output + ev.cache_read + ev.cache_write,
            cost,
        });
        while self.feed.len() > FEED_CAPACITY {
            self.feed.pop_front();
        }
    }
}

pub fn ingest_event(stats: &mut Stats, ev: &UsageEvent, pricing: &PricingTable) {
    let (cost_delta, unknown) = match pricing.lookup(&ev.model) {
        Some(p) => (
            p.cost_for_tokens(ev.input, ev.output, ev.cache_read, ev.cache_write),
            false,
        ),
        None => (0.0, true),
    };
    // Feed is a live activity view, not a today-scoped aggregate: every
    // successfully parsed event lands here regardless of the day filter below.
    stats.push_feed(ev, cost_delta);

    let local_ts = ev.ts.with_timezone(&chrono::Local);
    if local_ts.date_naive() != stats.current_day {
        return;
    }
    let hour = local_ts.hour_index();

    if unknown {
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
    use chrono::{NaiveDate, TimeZone, Utc};
    use crate::pricing::{Price, PricingTable};
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

    // --- Stats / ingest_event / rollover / reprice / views tests ---

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
}
