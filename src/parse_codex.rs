use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{Source, UsageEvent};

/// One `rate_limits` window exactly as Codex logged it.
#[derive(Debug, Clone, Copy)]
struct RateWindow {
    resets_at: DateTime<Utc>,
    used_percent: Option<f64>,
}

#[derive(Default)]
pub struct CodexSessionParser {
    current_model: Option<String>,
}

impl CodexSessionParser {
    pub fn process_line(&mut self, line: &str) -> Option<UsageEvent> {
        let v: Value = serde_json::from_str(line).ok()?;
        let event_type = v.get("type")?.as_str()?;

        if event_type == "turn_context" {
            if let Some(model) = v
                .get("payload")
                .and_then(|p| p.get("model"))
                .and_then(|m| m.as_str())
            {
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
        let ts: DateTime<Utc> = DateTime::parse_from_rfc3339(ts_str)
            .ok()?
            .with_timezone(&Utc);
        let last = payload.get("info")?.get("last_token_usage")?;
        let get_u64 = |field: &str| last.get(field).and_then(|x| x.as_u64()).unwrap_or(0);

        let cache_read = get_u64("cached_input_tokens");
        let soonest = Self::soonest_window(payload);
        Some(UsageEvent {
            ts,
            source: Source::Codex,
            model,
            // Codex's total_tokens proves cached input is already included in input_tokens.
            // Store the non-cached portion so totals and pricing do not count it twice.
            input: get_u64("input_tokens").saturating_sub(cache_read),
            output: get_u64("output_tokens"),
            cache_read,
            cache_write: 0,
            reset_at: soonest.map(|window| window.resets_at),
            reset_used_percent: soonest.and_then(|window| window.used_percent),
        })
    }

    /// Codex reports both a short session window and a longer weekly one
    /// under `rate_limits.primary`/`.secondary`; surface whichever resets
    /// first since that is the one that actually blocks the next request.
    ///
    /// Unlike Claude, these are the server's own numbers — Codex writes the
    /// rate-limit snapshot the API returned straight into the rollout log — so
    /// they are taken as-is, never inferred from event timestamps.
    fn soonest_window(payload: &Value) -> Option<RateWindow> {
        let rate_limits = payload.get("rate_limits")?;
        ["primary", "secondary"]
            .iter()
            .filter_map(|name| {
                let window = rate_limits.get(name)?;
                Some(RateWindow {
                    resets_at: DateTime::from_timestamp(window.get("resets_at")?.as_i64()?, 0)?,
                    used_percent: window.get("used_percent").and_then(|p| p.as_f64()),
                })
            })
            .min_by_key(|window| window.resets_at)
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
        assert_eq!(ev.input, 90);
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
        assert_eq!(sum_input, 135);
        assert_eq!(sum_output, 30);
        assert_eq!(sum_cache, 15);
        assert_eq!(sum_input + sum_cache + sum_output, 180);
    }

    #[test]
    fn soonest_window_reports_the_usage_of_the_limit_that_resets_first() {
        // Shape copied from a real rollout log: the secondary window resets
        // sooner here, so its `used_percent` — not the primary's 51% — is the
        // one that describes the limit about to bite.
        const WITH_LIMITS: &str = r#"{"timestamp":"2026-07-13T10:00:05.000Z","type":"event_msg","payload":{"type":"token_count","info":{"last_token_usage":{"input_tokens":100,"cached_input_tokens":10,"output_tokens":20,"total_tokens":120}},"rate_limits":{"primary":{"used_percent":51.0,"window_minutes":10080,"resets_at":2000000000},"secondary":{"used_percent":8.0,"window_minutes":300,"resets_at":1900000000}}}}"#;

        let mut p = CodexSessionParser::default();
        p.process_line(TURN_CONTEXT);
        let ev = p.process_line(WITH_LIMITS).expect("should parse");
        assert_eq!(ev.reset_at, DateTime::from_timestamp(1_900_000_000, 0));
        assert_eq!(ev.reset_used_percent, Some(8.0));
    }

    #[test]
    fn missing_rate_limits_leaves_both_reset_fields_empty() {
        let mut p = CodexSessionParser::default();
        p.process_line(TURN_CONTEXT);
        let ev = p.process_line(TOKEN_COUNT_1).expect("should parse");
        assert_eq!(ev.reset_at, None);
        assert_eq!(ev.reset_used_percent, None);
    }

    #[test]
    fn unrelated_event_types_are_skipped() {
        let mut p = CodexSessionParser::default();
        let line = r#"{"timestamp":"2026-07-13T10:00:00.000Z","type":"session_meta","payload":{"id":"abc"}}"#;
        assert!(p.process_line(line).is_none());
    }
}
