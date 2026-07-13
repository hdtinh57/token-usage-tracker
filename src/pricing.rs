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
    use std::collections::HashMap;

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
