use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime};

use chrono::Local;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::alerts::AlertState;
use crate::discovery::{collect_mtimes, select_active, walk_jsonl_files};
use crate::history::RawTotals;
use crate::model::{Source, Stats, ingest_event, reprice};
use crate::parse_claude::ClaudeSessionParser;
use crate::parse_codex::CodexSessionParser;
use crate::pricing::PricingTable;
use crate::quota;
use crate::settings::Settings;
use crate::tail::Tailer;
use crate::watch::PendingPaths;

const ACTIVE_WINDOW: Duration = Duration::from_secs(30 * 60);
const STARTUP_WINDOW: Duration = Duration::from_secs(48 * 60 * 60);
pub const FAST_TICK: Duration = Duration::from_secs(1);
pub const SLOW_TICK: Duration = Duration::from_secs(10 * 60);

pub struct Worker {
    claude_root: PathBuf,
    codex_root: PathBuf,
    pricing_path: PathBuf,
    history_path: PathBuf,
    alerts_path: PathBuf,
    settings_path: PathBuf,
    settings: Settings,
    alerts: AlertState,
    watcher: RecommendedWatcher,
    watch_events: mpsc::Receiver<notify::Result<notify::Event>>,
    pending_paths: PendingPaths,
    tailer: Tailer,
    claude_parsers: HashMap<PathBuf, ClaudeSessionParser>,
    codex_parsers: HashMap<PathBuf, CodexSessionParser>,
    active_set: Vec<PathBuf>,
    pricing: PricingTable,
    pricing_mtime: Option<SystemTime>,
    stats: Stats,
    startup_history: HashMap<chrono::NaiveDate, HashMap<(crate::model::Source, String), RawTotals>>,
    last_quota_poll: Option<std::time::Instant>,
    snapshot: Arc<Mutex<Arc<Stats>>>,
}

impl Worker {
    pub fn new(
        claude_root: PathBuf,
        codex_root: PathBuf,
        pricing_path: PathBuf,
        history_path: PathBuf,
        snapshot: Arc<Mutex<Arc<Stats>>>,
    ) -> std::io::Result<Self> {
        let pricing = PricingTable::load(&pricing_path)?;
        let app_data = history_path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "history path has no parent",
            )
        })?;
        let settings = Settings::load(&app_data.join("settings.json"))?;
        let settings_path = app_data.join("settings.json");
        let alerts_path = app_data.join("alerts.json");
        let alerts = AlertState::load(&alerts_path)?;
        let pricing_mtime = std::fs::metadata(&pricing_path)
            .and_then(|m| m.modified())
            .ok();
        let mut stats = Stats::new(Local::now().date_naive());
        stats.history = crate::history::History::load(&history_path)?;
        let (watch_tx, watch_events) = mpsc::channel();
        let watcher = RecommendedWatcher::new(
            move |event| {
                let _ = watch_tx.send(event);
            },
            notify::Config::default(),
        )
        .map_err(std::io::Error::other)?;
        let mut worker = Self {
            claude_root,
            codex_root,
            pricing_path,
            history_path: history_path.clone(),
            alerts_path,
            settings_path,
            settings,
            alerts,
            watcher,
            watch_events,
            pending_paths: PendingPaths::new(),
            tailer: Tailer::new(),
            claude_parsers: HashMap::new(),
            codex_parsers: HashMap::new(),
            active_set: Vec::new(),
            pricing,
            pricing_mtime,
            stats,
            startup_history: HashMap::new(),
            last_quota_poll: None,
            snapshot,
        };
        worker.watch_roots();
        Ok(worker)
    }

    fn all_jsonl_files(&self) -> Vec<PathBuf> {
        let mut paths = walk_jsonl_files(&self.claude_root);
        paths.extend(walk_jsonl_files(&self.codex_root));
        paths
    }

    fn refresh_active_set(&mut self, window: Duration) {
        self.active_set = select_active(
            &collect_mtimes(&self.all_jsonl_files()),
            window,
            SystemTime::now(),
        );
    }

    pub fn startup(&mut self) {
        let recent = select_active(
            &collect_mtimes(&self.all_jsonl_files()),
            STARTUP_WINDOW,
            SystemTime::now(),
        );
        for path in recent {
            self.ingest_whole_file(&path);
        }
        if !self.startup_history.is_empty() {
            let mut recovered = false;
            for (day, entries) in std::mem::take(&mut self.startup_history) {
                if !self.stats.history.contains_day(day) {
                    self.stats.history.replace_day(day, entries.into_iter());
                    recovered = true;
                }
            }
            if recovered && let Err(error) = self.stats.history.save(&self.history_path) {
                eprintln!("warning: failed saving recovered usage history: {error}");
            }
        }
        self.refresh_active_set(ACTIVE_WINDOW);
    }

    fn ingest_whole_file(&mut self, path: &Path) {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let is_codex = path.starts_with(&self.codex_root);
        for line in String::from_utf8_lossy(&bytes).lines() {
            self.ingest_line(path, line, is_codex, true);
        }
        self.tailer.prime(path, bytes.len() as u64);
    }

    fn ingest_line(&mut self, path: &Path, line: &str, is_codex: bool, today_only: bool) {
        let event = if is_codex {
            self.codex_parsers
                .entry(path.to_path_buf())
                .or_default()
                .process_line(line)
        } else {
            self.claude_parsers
                .entry(path.to_path_buf())
                .or_default()
                .process_line(line)
        };
        if let Some(event) = event {
            let day = event.ts.with_timezone(&Local).date_naive();
            if today_only && day < self.stats.current_day {
                self.startup_history
                    .entry(day)
                    .or_default()
                    .entry((event.source, event.model.clone()))
                    .or_default()
                    .merge(&RawTotals {
                        input: event.input,
                        output: event.output,
                        cache_read: event.cache_read,
                        cache_write: event.cache_write,
                        requests: 1,
                    });
            } else if (!today_only || day == self.stats.current_day)
                && ingest_event(&mut self.stats, &event, &self.pricing)
            {
                self.decide_alerts(
                    Source::Codex,
                    "session",
                    self.stats.codex_used_percent,
                    self.stats.codex_reset_at,
                );
            }
        }
    }

    pub fn fast_tick(&mut self) {
        self.persist_completed_day_if_needed();
        let mut recover = false;
        while let Ok(event) = self.watch_events.try_recv() {
            match event {
                Ok(event) if matches!(event.kind, EventKind::Create(_) | EventKind::Modify(_)) => {
                    let now = std::time::Instant::now();
                    for path in event.paths {
                        self.pending_paths.push(path, now);
                    }
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("warning: file watcher error: {error}");
                    recover = true;
                }
            }
        }
        for path in self.pending_paths.take_ready(std::time::Instant::now()) {
            self.tail_path(&path);
        }
        for path in self.active_set.clone() {
            self.tail_path(&path);
        }
        if recover {
            self.recovery_tick();
        }
        self.publish();
    }

    fn tail_path(&mut self, path: &Path) {
        let is_codex = path.starts_with(&self.codex_root);
        match self.tailer.poll(path) {
            Ok(lines) => {
                for line in lines {
                    self.ingest_line(path, &line, is_codex, false);
                }
            }
            Err(error) => eprintln!("warning: failed reading {}: {error}", path.display()),
        }
    }

    fn persist_completed_day_if_needed(&mut self) {
        let today = Local::now().date_naive();
        if self.stats.current_day == today {
            return;
        }
        self.stats.history.replace_day(
            self.stats.current_day,
            self.stats
                .by_model
                .iter()
                .map(|(key, totals)| (key.clone(), RawTotals::from_totals(totals))),
        );
        if let Err(error) = self.stats.history.save(&self.history_path) {
            eprintln!("warning: failed saving usage history: {error}");
            return;
        }
        self.stats.rollover_if_needed(today);
    }

    pub fn recovery_tick(&mut self) {
        self.refresh_active_set(ACTIVE_WINDOW);
        self.watch_roots();
        self.reload_pricing_if_changed();
        self.refresh_claude_quota();
    }

    fn watch_roots(&mut self) {
        for root in [&self.claude_root, &self.codex_root] {
            if root.exists()
                && let Err(error) = self.watcher.watch(root, RecursiveMode::Recursive)
            {
                eprintln!("warning: failed watching {}: {error}", root.display());
            }
        }
    }

    /// Polls the account for Claude's real quota windows. A failed poll (no
    /// network, expired token) leaves the last good reading in place rather
    /// than blanking the rows.
    ///
    /// ponytail: the request blocks this worker thread for up to its timeout,
    /// which can stall a fast tick — it is one call every 3 minutes against a
    /// 10s timeout, so a tick lands late at worst. Move it to its own thread
    /// if the feed ever visibly stutters.
    fn refresh_claude_quota(&mut self) {
        if self
            .last_quota_poll
            .is_some_and(|last| last.elapsed() < quota::POLL_INTERVAL)
        {
            return;
        }
        self.last_quota_poll = Some(std::time::Instant::now());
        if let Some(fetched) = quota::fetch() {
            self.stats.claude_quota = fetched;
            self.stats.quota_updated_at = Some(chrono::Utc::now());
            self.decide_alerts(
                Source::Claude,
                "session",
                fetched.five_hour.map(|window| window.utilization),
                fetched.five_hour.map(|window| window.resets_at),
            );
            self.decide_alerts(
                Source::Claude,
                "week",
                fetched.seven_day.map(|window| window.utilization),
                fetched.seven_day.map(|window| window.resets_at),
            );
        }
    }

    fn decide_alerts(
        &mut self,
        source: Source,
        window: &str,
        utilization: Option<f64>,
        reset_at: Option<chrono::DateTime<chrono::Utc>>,
    ) {
        if let Ok(settings) = Settings::load(&self.settings_path) {
            self.settings = settings;
        }
        if !self.settings.notifications_enabled {
            return;
        }
        let (Some(utilization), Some(reset_at)) = (utilization, reset_at) else {
            return;
        };
        let crossed = self.alerts.crossed(source, window, utilization, reset_at);
        if !crossed.is_empty()
            && let Err(error) = self.alerts.save(&self.alerts_path)
        {
            eprintln!("warning: failed saving quota alert state: {error}");
        }
        for threshold in crossed {
            notify_quota(source, window, threshold);
        }
    }

    fn reload_pricing_if_changed(&mut self) {
        let mtime = std::fs::metadata(&self.pricing_path)
            .and_then(|m| m.modified())
            .ok();
        if mtime != self.pricing_mtime
            && let Ok(pricing) = PricingTable::load(&self.pricing_path)
        {
            self.pricing = pricing;
            self.pricing_mtime = mtime;
            reprice(&mut self.stats, &self.pricing);
        }
    }

    pub fn publish(&self) {
        *self.snapshot.lock().unwrap() = Arc::new(self.stats.clone());
    }

    pub fn run(mut self) {
        self.startup();
        self.publish();
        let mut last_slow_tick = std::time::Instant::now();
        loop {
            std::thread::sleep(FAST_TICK);
            self.fast_tick();
            if last_slow_tick.elapsed() >= SLOW_TICK {
                self.recovery_tick();
                last_slow_tick = std::time::Instant::now();
            }
        }
    }
}

#[cfg(windows)]
fn notify_quota(source: Source, window: &str, threshold: u8) {
    if let Err(error) = notify_rust::Notification::new()
        .summary("Token Usage Tracker")
        .body(&format!("{source:?} {window} quota reached {threshold}%"))
        .show()
    {
        eprintln!("warning: failed showing quota notification: {error}");
    }
}

#[cfg(not(windows))]
fn notify_quota(_source: Source, _window: &str, _threshold: u8) {}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tt_worker_test_{}_{}",
            name,
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn today_ts(hour: u32) -> String {
        date_ts(chrono::Local::now().date_naive(), hour)
    }

    fn date_ts(date: chrono::NaiveDate, hour: u32) -> String {
        chrono::Local
            .from_local_datetime(&date.and_hms_opt(hour, 0, 0).unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc)
            .to_rfc3339()
    }

    #[test]
    fn startup_rescan_ingests_todays_events_from_both_roots() {
        let claude_root = temp_root("claude");
        let codex_root = temp_root("codex");
        let paths = crate::paths::Paths::from_app_data(temp_root("pricing_dir")).unwrap();
        let pricing_path = paths.pricing;
        let history_path = paths.history;
        let claude_line = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            today_ts(9)
        );
        std::fs::write(
            claude_root.join("session1.jsonl"),
            format!("{}\n", claude_line),
        )
        .unwrap();
        let codex_lines = format!(
            "{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t1\",\"model\":\"gpt-5.5\"}}}}\n{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"last_token_usage\":{{\"input_tokens\":40,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_output_tokens\":0,\"total_tokens\":45}},\"total_token_usage\":{{\"input_tokens\":40,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_output_tokens\":0,\"total_tokens\":45}}}}}}}}\n",
            today_ts(9),
            today_ts(9)
        );
        std::fs::write(codex_root.join("rollout1.jsonl"), codex_lines).unwrap();
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(
            chrono::Local::now().date_naive(),
        ))));
        let mut worker = Worker::new(
            claude_root.clone(),
            codex_root.clone(),
            pricing_path,
            history_path,
            snapshot.clone(),
        )
        .unwrap();
        worker.startup();
        worker.publish();
        assert_eq!(
            crate::model::totals_for(&snapshot.lock().unwrap().clone(), None, None).input,
            140
        );
        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }

    #[test]
    fn startup_recovers_yesterdays_usage_to_history_without_adding_it_to_today() {
        let claude_root = temp_root("recover_claude");
        let codex_root = temp_root("recover_codex");
        let paths = crate::paths::Paths::from_app_data(temp_root("recover_history")).unwrap();
        let yesterday = chrono::Local::now().date_naive().pred_opt().unwrap();
        let line = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            date_ts(yesterday, 9)
        );
        std::fs::write(claude_root.join("session.jsonl"), format!("{line}\n")).unwrap();
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(
            chrono::Local::now().date_naive(),
        ))));
        let mut worker = Worker::new(
            claude_root.clone(),
            codex_root.clone(),
            paths.pricing,
            paths.history.clone(),
            snapshot,
        )
        .unwrap();

        worker.startup();

        assert!(worker.stats.by_model.is_empty());
        assert_eq!(
            crate::history::History::load(&paths.history)
                .unwrap()
                .totals_for(yesterday, None, None)
                .input,
            100
        );
        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }

    #[test]
    fn startup_recovery_keeps_existing_day_when_only_one_session_is_recent() {
        let claude_root = temp_root("partial_claude");
        let codex_root = temp_root("partial_codex");
        let paths = crate::paths::Paths::from_app_data(temp_root("partial_history")).unwrap();
        let yesterday = chrono::Local::now().date_naive().pred_opt().unwrap();
        let mut existing = crate::history::History::default();
        existing.add(
            yesterday,
            crate::model::Source::Claude,
            "claude-sonnet-4-6",
            RawTotals {
                input: 10,
                requests: 1,
                ..Default::default()
            },
        );
        existing.add(
            yesterday,
            crate::model::Source::Codex,
            "gpt-5.5",
            RawTotals {
                input: 20,
                requests: 1,
                ..Default::default()
            },
        );
        existing.save(&paths.history).unwrap();
        let line = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":10,"output_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            date_ts(yesterday, 9)
        );
        std::fs::write(
            claude_root.join("recent-session.jsonl"),
            format!("{line}\n"),
        )
        .unwrap();
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(
            chrono::Local::now().date_naive(),
        ))));
        let mut worker = Worker::new(
            claude_root.clone(),
            codex_root.clone(),
            paths.pricing,
            paths.history.clone(),
            snapshot,
        )
        .unwrap();

        worker.startup();

        assert_eq!(
            crate::history::History::load(&paths.history)
                .unwrap()
                .totals_for(yesterday, None, None)
                .input,
            30
        );
        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }

    #[test]
    fn fast_tick_picks_up_appended_lines_after_startup() {
        let claude_root = temp_root("claude2");
        let codex_root = temp_root("codex2");
        let paths = crate::paths::Paths::from_app_data(temp_root("pricing_dir2")).unwrap();
        let pricing_path = paths.pricing;
        let history_path = paths.history;
        std::fs::write(claude_root.join("session1.jsonl"), b"").unwrap();
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(
            chrono::Local::now().date_naive(),
        ))));
        let mut worker = Worker::new(
            claude_root.clone(),
            codex_root.clone(),
            pricing_path,
            history_path,
            snapshot.clone(),
        )
        .unwrap();
        worker.startup();
        let line = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":7,"output_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            today_ts(11)
        );
        use std::io::Write;
        std::fs::OpenOptions::new()
            .append(true)
            .open(claude_root.join("session1.jsonl"))
            .unwrap()
            .write_all(format!("{}\n", line).as_bytes())
            .unwrap();
        worker.fast_tick();
        assert_eq!(
            crate::model::totals_for(&snapshot.lock().unwrap().clone(), None, None).input,
            7
        );
        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }

    #[test]
    fn rollover_saves_completed_day_before_clearing_live_totals() {
        let claude_root = temp_root("rollover_claude");
        let codex_root = temp_root("rollover_codex");
        let paths = crate::paths::Paths::from_app_data(temp_root("rollover_history")).unwrap();
        let history_path = paths.history;
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(
            chrono::Local::now().date_naive(),
        ))));
        let mut worker = Worker::new(
            claude_root.clone(),
            codex_root.clone(),
            paths.pricing,
            history_path.clone(),
            snapshot,
        )
        .unwrap();
        let yesterday = chrono::Local::now().date_naive().pred_opt().unwrap();
        worker.stats.current_day = yesterday;
        worker.stats.by_model.insert(
            (
                crate::model::Source::Claude,
                "claude-sonnet-4-6".to_string(),
            ),
            crate::model::Totals {
                input: 42,
                requests: 1,
                ..Default::default()
            },
        );

        worker.persist_completed_day_if_needed();

        assert_eq!(worker.stats.current_day, chrono::Local::now().date_naive());
        assert!(worker.stats.by_model.is_empty());
        assert_eq!(
            crate::history::History::load(&history_path)
                .unwrap()
                .totals_for(yesterday, None, None)
                .input,
            42
        );

        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }

    #[test]
    fn recovery_tick_discovers_and_ingests_a_brand_new_file_after_startup() {
        let claude_root = temp_root("claude3");
        let codex_root = temp_root("codex3");
        let paths = crate::paths::Paths::from_app_data(temp_root("pricing_dir3")).unwrap();
        let pricing_path = paths.pricing;
        let history_path = paths.history;
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(
            chrono::Local::now().date_naive(),
        ))));
        let mut worker = Worker::new(
            claude_root.clone(),
            codex_root.clone(),
            pricing_path,
            history_path,
            snapshot.clone(),
        )
        .unwrap();
        worker.startup();
        let line = format!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":3,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#,
            today_ts(12)
        );
        std::fs::write(claude_root.join("brand_new.jsonl"), format!("{}\n", line)).unwrap();
        worker.recovery_tick();
        worker.fast_tick();
        assert_eq!(
            crate::model::totals_for(&snapshot.lock().unwrap().clone(), None, None).input,
            3
        );
        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }

    #[test]
    fn malformed_pricing_keeps_the_last_good_table_and_retries_after_correction() {
        let claude_root = temp_root("reload_claude");
        let codex_root = temp_root("reload_codex");
        let paths = crate::paths::Paths::from_app_data(temp_root("reload_pricing")).unwrap();
        let pricing_path = paths.pricing;
        let history_path = paths.history;
        std::fs::write(
            &pricing_path,
            r#"{ "model": { "input": 1.0, "output": 1.0, "cache_read": 1.0, "cache_write": 1.0 } }"#,
        )
        .unwrap();
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(
            chrono::Local::now().date_naive(),
        ))));
        let mut worker = Worker::new(
            claude_root.clone(),
            codex_root.clone(),
            pricing_path.clone(),
            history_path,
            snapshot,
        )
        .unwrap();
        let initial_mtime = worker.pricing_mtime;

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(&pricing_path, "not json").unwrap();
        worker.reload_pricing_if_changed();
        assert_eq!(worker.pricing_mtime, initial_mtime);
        assert_eq!(worker.pricing.lookup("model").unwrap().input, 1.0);

        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(
            &pricing_path,
            r#"{ "model": { "input": 2.0, "output": 2.0, "cache_read": 2.0, "cache_write": 2.0 } }"#,
        )
        .unwrap();
        let corrected_mtime = std::fs::metadata(&pricing_path)
            .unwrap()
            .modified()
            .unwrap();
        worker.reload_pricing_if_changed();
        assert_eq!(worker.pricing_mtime, Some(corrected_mtime));
        assert_eq!(worker.pricing.lookup("model").unwrap().input, 2.0);

        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }
}
