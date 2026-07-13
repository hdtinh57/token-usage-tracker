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
