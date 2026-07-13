//! Authoritative Claude quota state, straight from the account.
//!
//! Claude's transcripts carry no rate-limit data, and the window cannot be
//! reconstructed from them: it is server-side state anchored to the account's
//! first request of the window, which no local log records. Inferring it from
//! event timestamps — the shape ccusage uses, floor-to-hour plus a 5h chain —
//! drifts by tens of minutes against what the account actually reports.
//!
//! So ask the account. This is the same endpoint Claude Code's own `/usage`
//! screen reads, authenticated with the OAuth token Claude Code already stores
//! on disk. Read-only, and only ever contacted at `POLL_INTERVAL` — the
//! endpoint rate-limits per access token, and a `claude-code/<version>`
//! User-Agent is required to stay out of its aggressive 429 bucket.

use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Deserialize;

const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const USER_AGENT: &str = "claude-code/2.0.0";
const OAUTH_BETA: &str = "oauth-2025-04-20";
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);

/// Safe cadence for this endpoint per its rate limiter. The windows it reports
/// are hours long, so polling faster buys nothing anyway.
pub const POLL_INTERVAL: Duration = Duration::from_secs(180);

/// One quota window as the account reports it.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Window {
    /// Percent of the window's limit consumed, 0–100.
    pub utilization: f64,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct ClaudeQuota {
    /// The 5-hour session window. `None` when no window is currently open.
    pub five_hour: Option<Window>,
    /// The 7-day window, across all models.
    pub seven_day: Option<Window>,
}

fn credentials_path() -> Option<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .ok()?;
    Some(
        PathBuf::from(home)
            .join(".claude")
            .join(".credentials.json"),
    )
}

/// Reads Claude Code's stored OAuth access token. Re-read on every poll rather
/// than cached: Claude Code refreshes this file, and a cached token would go
/// stale and 401 for the rest of the process's life.
fn access_token() -> Option<String> {
    #[derive(Deserialize)]
    struct Credentials {
        #[serde(rename = "claudeAiOauth")]
        oauth: Oauth,
    }
    #[derive(Deserialize)]
    struct Oauth {
        #[serde(rename = "accessToken")]
        access_token: String,
    }

    let raw = std::fs::read_to_string(credentials_path()?).ok()?;
    let credentials: Credentials = serde_json::from_str(&raw).ok()?;
    Some(credentials.oauth.access_token)
}

/// `None` on any failure — not logged in, offline, token expired, endpoint
/// changed. The caller keeps the last good reading rather than blanking the
/// display, so a transient failure costs nothing.
pub fn fetch() -> Option<ClaudeQuota> {
    let token = access_token()?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .build()
        .into();
    let body = agent
        .get(USAGE_URL)
        .header("Authorization", &format!("Bearer {token}"))
        .header("anthropic-beta", OAUTH_BETA)
        .header("User-Agent", USER_AGENT)
        .call()
        .ok()?
        .body_mut()
        .read_to_string()
        .ok()?;
    serde_json::from_str(&body).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_usage_payload_shape_the_endpoint_returns() {
        let raw = r#"{
            "five_hour": {"utilization": 16.0, "resets_at": "2026-07-13T11:20:00.528743+00:00"},
            "seven_day": {"utilization": 27.0, "resets_at": "2026-07-16T10:00:00.951713+00:00"},
            "seven_day_opus": null,
            "extra_usage": {"is_enabled": false}
        }"#;
        let quota: ClaudeQuota = serde_json::from_str(raw).expect("should parse");
        let session = quota.five_hour.expect("five_hour window present");
        assert_eq!(session.utilization, 16.0);
        assert_eq!(
            session.resets_at.to_rfc3339(),
            "2026-07-13T11:20:00.528743+00:00"
        );
        assert_eq!(quota.seven_day.unwrap().utilization, 27.0);
    }

    #[test]
    fn a_window_the_account_reports_as_absent_is_none_not_an_error() {
        let quota: ClaudeQuota = serde_json::from_str(r#"{"five_hour": null, "seven_day": null}"#)
            .expect("should parse");
        assert!(quota.five_hour.is_none());
        assert!(quota.seven_day.is_none());
    }
}
