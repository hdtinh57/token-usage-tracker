use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

impl Price {
    pub fn new(
        input: f64,
        output: f64,
        cache_read: f64,
        cache_write: f64,
    ) -> Result<Self, &'static str> {
        if [input, output, cache_read, cache_write]
            .iter()
            .any(|price| !price.is_finite() || *price < 0.0)
        {
            return Err("prices must be finite and non-negative");
        }
        Ok(Self {
            input,
            output,
            cache_read,
            cache_write,
        })
    }

    pub fn cost_for_tokens(
        &self,
        input: u64,
        output: u64,
        cache_read: u64,
        cache_write: u64,
    ) -> f64 {
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

#[derive(serde::Deserialize)]
struct RawPrice {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

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

    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: HashMap<String, RawPrice> =
            serde_json::from_str(text).map_err(|error| error.to_string())?;
        raw.into_iter()
            .map(|(model, price)| {
                Price::new(
                    price.input,
                    price.output,
                    price.cache_read,
                    price.cache_write,
                )
                .map(|price| (model, price))
                .map_err(|error| error.to_string())
            })
            .collect::<Result<HashMap<_, _>, _>>()
            .map(Self::from_map)
    }

    pub fn load(path: &Path) -> std::io::Result<PricingTable> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
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
            Price {
                input: 3.0,
                output: 15.0,
                cache_read: 0.3,
                cache_write: 3.75,
            },
        );
        prices.insert(
            "gpt-5".to_string(),
            Price {
                input: 5.0,
                output: 15.0,
                cache_read: 0.5,
                cache_write: 5.0,
            },
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
        let price = Price {
            input: 3.0,
            output: 15.0,
            cache_read: 0.3,
            cache_write: 3.75,
        };
        let cost = price.cost_for_tokens(1_000_000, 100_000, 500_000, 200_000);
        let expected = 3.0 + 1.5 + 0.15 + 0.75;
        assert!((cost - expected).abs() < 1e-9);
    }

    #[test]
    fn load_reads_valid_pricing() {
        let dir = std::env::temp_dir().join(format!(
            "tt_pricing_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("pricing.json");
        std::fs::write(
            &path,
            r#"{ "claude-sonnet-5": { "input": 2.0, "output": 10.0, "cache_read": 0.2, "cache_write": 2.5 } }"#,
        )
        .unwrap();

        let table = PricingTable::load(&path).unwrap();
        assert!(table.lookup("claude-sonnet-5").is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn price_new_rejects_negative_and_non_finite_values() {
        assert!(Price::new(-1.0, 1.0, 1.0, 1.0).is_err());
        assert!(Price::new(f64::INFINITY, 1.0, 1.0, 1.0).is_err());
    }

    #[test]
    fn parse_rejects_invalid_price_in_any_entry() {
        let result = PricingTable::parse(
            r#"{
                "valid": { "input": 1.0, "output": 1.0, "cache_read": 1.0, "cache_write": 1.0 },
                "invalid": { "input": -1.0, "output": 1.0, "cache_read": 1.0, "cache_write": 1.0 }
            }"#,
        );

        assert!(result.is_err());
    }
}
