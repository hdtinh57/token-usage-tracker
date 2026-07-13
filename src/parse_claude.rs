use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::model::{Source, UsageEvent};

#[cfg(test)]
pub fn parse_line(line: &str) -> Option<UsageEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    parse_value(&v)
}

fn parse_value(v: &Value) -> Option<UsageEvent> {
    let message = v.get("message")?;
    let usage = message.get("usage")?;
    let model = message.get("model")?.as_str()?.to_string();
    let ts_str = v.get("timestamp")?.as_str()?;
    let ts: DateTime<Utc> = DateTime::parse_from_rfc3339(ts_str)
        .ok()?
        .with_timezone(&Utc);

    let get_u64 = |field: &str| usage.get(field).and_then(|x| x.as_u64()).unwrap_or(0);

    Some(UsageEvent {
        ts,
        source: Source::Claude,
        model,
        input: get_u64("input_tokens"),
        output: get_u64("output_tokens"),
        cache_read: get_u64("cache_read_input_tokens"),
        cache_write: get_u64("cache_creation_input_tokens"),
        reset_at: None,
        reset_used_percent: None,
    })
}

#[derive(Default)]
pub struct ClaudeSessionParser {
    seen_message_ids: HashSet<String>,
}

impl ClaudeSessionParser {
    pub fn process_line(&mut self, line: &str) -> Option<UsageEvent> {
        let v: Value = serde_json::from_str(line).ok()?;
        let message_id = v
            .get("message")
            .and_then(|message| message.get("id"))
            .and_then(|id| id.as_str());
        if message_id.is_some_and(|id| self.seen_message_ids.contains(id)) {
            return None;
        }
        let event = parse_value(&v)?;
        if let Some(message_id) = message_id {
            self.seen_message_ids.insert(message_id.to_owned());
        }
        Some(event)
    }
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
        let ev = parse_line(line)
            .expect("should parse: missing fields default to 0, don't fail the line");
        assert_eq!(ev.input, 5);
        assert_eq!(ev.output, 0);
        assert_eq!(ev.cache_read, 0);
        assert_eq!(ev.cache_write, 0);
    }

    #[test]
    fn malformed_json_returns_none_not_a_panic() {
        assert!(parse_line("not json at all {{{").is_none());
    }

    #[test]
    fn duplicate_assistant_message_id_is_counted_once() {
        let line = r#"{"type":"assistant","timestamp":"2026-07-13T10:15:30.000Z","message":{"id":"msg_1","model":"claude-sonnet-5","usage":{"input_tokens":5}}}"#;
        let mut parser = ClaudeSessionParser::default();
        assert!(parser.process_line(line).is_some());
        assert!(parser.process_line(line).is_none());
    }
}
