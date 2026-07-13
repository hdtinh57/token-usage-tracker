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
