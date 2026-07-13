use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::ops::RangeInclusive;
use std::path::Path;

use chrono::{Duration, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::model::{Source, Totals};
use crate::pricing::PricingTable;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawTotals {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub requests: u64,
}

impl RawTotals {
    pub fn merge(&mut self, other: &Self) {
        self.input += other.input;
        self.output += other.output;
        self.cache_read += other.cache_read;
        self.cache_write += other.cache_write;
        self.requests += other.requests;
    }

    pub fn from_totals(totals: &Totals) -> Self {
        Self {
            input: totals.input,
            output: totals.output,
            cache_read: totals.cache_read,
            cache_write: totals.cache_write,
            requests: totals.requests,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct History {
    days: BTreeMap<NaiveDate, BTreeMap<(Source, String), RawTotals>>,
}

#[derive(Serialize, Deserialize)]
struct StoredDay {
    day: NaiveDate,
    entries: Vec<StoredEntry>,
}

#[derive(Serialize, Deserialize)]
struct StoredEntry {
    source: Source,
    model: String,
    #[serde(flatten)]
    totals: RawTotals,
}

impl History {
    pub fn load(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let stored: Vec<StoredDay> = serde_json::from_reader(BufReader::new(File::open(path)?))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let mut history = Self::default();
        for day in stored {
            for entry in day.entries {
                history.add(day.day, entry.source, &entry.model, entry.totals);
            }
        }
        Ok(history)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let temp = path.with_file_name("history.json.tmp");
        let file = File::create(&temp)?;
        let mut writer = BufWriter::new(file);
        let stored: Vec<_> = self
            .days
            .iter()
            .map(|(day, entries)| StoredDay {
                day: *day,
                entries: entries
                    .iter()
                    .map(|((source, model), totals)| StoredEntry {
                        source: *source,
                        model: model.clone(),
                        totals: *totals,
                    })
                    .collect(),
            })
            .collect();
        serde_json::to_writer(&mut writer, &stored)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        std::fs::rename(temp, path)
    }

    pub fn add(&mut self, day: NaiveDate, source: Source, model: &str, totals: RawTotals) {
        self.days
            .entry(day)
            .or_default()
            .entry((source, model.to_owned()))
            .or_default()
            .merge(&totals);
    }

    pub fn replace_day(
        &mut self,
        day: NaiveDate,
        totals: impl Iterator<Item = ((Source, String), RawTotals)>,
    ) {
        self.days.insert(day, totals.collect());
    }

    pub fn contains_day(&self, day: NaiveDate) -> bool {
        self.days.contains_key(&day)
    }

    pub fn totals_for(
        &self,
        day: NaiveDate,
        model: Option<&str>,
        source: Option<Source>,
    ) -> RawTotals {
        self.totals_in(day..=day, model, source)
    }

    pub fn totals_in(
        &self,
        days: RangeInclusive<NaiveDate>,
        model: Option<&str>,
        source: Option<Source>,
    ) -> RawTotals {
        let mut result = RawTotals::default();
        for (_, entries) in self.days.range(days) {
            for ((entry_source, entry_model), totals) in entries {
                if model.is_none_or(|filter| entry_model == filter)
                    && source.is_none_or(|filter| *entry_source == filter)
                {
                    result.merge(totals);
                }
            }
        }
        result
    }

    pub fn current_week_totals(
        &self,
        today: NaiveDate,
        pricing: &PricingTable,
        model: Option<&str>,
        source: Option<Source>,
    ) -> Totals {
        let start = today - Duration::days(6);
        self.priced_totals_in(start..=today, pricing, model, source)
    }

    pub fn priced_totals_in(
        &self,
        days: RangeInclusive<NaiveDate>,
        pricing: &PricingTable,
        model: Option<&str>,
        source: Option<Source>,
    ) -> Totals {
        let mut result = Totals::default();
        for (_, entries) in self.days.range(days) {
            for ((entry_source, entry_model), raw) in entries {
                if model.is_some_and(|filter| entry_model != filter)
                    || source.is_some_and(|filter| *entry_source != filter)
                {
                    continue;
                }
                result.input += raw.input;
                result.output += raw.output;
                result.cache_read += raw.cache_read;
                result.cache_write += raw.cache_write;
                result.requests += raw.requests;
                if let Some(price) = pricing.lookup(entry_model) {
                    result.cost += price.cost_for_tokens(
                        raw.input,
                        raw.output,
                        raw.cache_read,
                        raw.cache_write,
                    );
                }
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Source;
    use chrono::NaiveDate;

    #[test]
    fn saves_and_loads_raw_usage_by_iso_day_source_and_model() {
        let dir = std::env::temp_dir().join(format!(
            "tt_history_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.json");
        let day = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap();
        let mut history = History::default();

        history.add(
            day,
            Source::Claude,
            "claude-sonnet-4",
            RawTotals {
                input: 10,
                output: 2,
                cache_read: 3,
                cache_write: 4,
                requests: 1,
            },
        );
        history.save(&path).unwrap();

        assert!(path.exists());
        assert!(!path.with_file_name("history.json.tmp").exists());
        let loaded = History::load(&path).unwrap();
        assert_eq!(loaded.totals_for(day, None, Some(Source::Claude)).input, 10);
        assert_eq!(
            loaded
                .totals_for(day, Some("claude-sonnet-4"), None)
                .cache_write,
            4
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("2026-07-13"));

        let _ = std::fs::remove_dir_all(dir);
    }
}
