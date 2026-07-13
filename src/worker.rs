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
            SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn today_ts(hour: u32) -> String {
        let today = chrono::Local::now().date_naive();
        chrono::Local
            .from_local_datetime(&today.and_hms_opt(hour, 0, 0).unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc)
            .to_rfc3339()
    }

    #[test]
    fn startup_rescan_ingests_todays_events_from_both_roots() {
        let claude_root = temp_root("claude");
        let codex_root = temp_root("codex");
        let pricing_path = temp_root("pricing_dir").join("pricing.json");
        let claude_line = format!(r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":100,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#, today_ts(9));
        std::fs::write(claude_root.join("session1.jsonl"), format!("{}\n", claude_line)).unwrap();
        let codex_lines = format!("{{\"timestamp\":\"{}\",\"type\":\"turn_context\",\"payload\":{{\"turn_id\":\"t1\",\"model\":\"gpt-5.5\"}}}}\n{{\"timestamp\":\"{}\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"token_count\",\"info\":{{\"last_token_usage\":{{\"input_tokens\":40,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_output_tokens\":0,\"total_tokens\":45}},\"total_token_usage\":{{\"input_tokens\":40,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_output_tokens\":0,\"total_tokens\":45}}}}}}}}\n", today_ts(9), today_ts(9));
        std::fs::write(codex_root.join("rollout1.jsonl"), codex_lines).unwrap();
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(chrono::Local::now().date_naive()))));
        let mut worker = Worker::new(claude_root.clone(), codex_root.clone(), pricing_path, snapshot.clone()).unwrap();
        worker.startup();
        worker.publish();
        assert_eq!(crate::model::totals_for(&snapshot.lock().unwrap().clone(), None, None).input, 140);
        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }

    #[test]
    fn fast_tick_picks_up_appended_lines_after_startup() {
        let claude_root = temp_root("claude2");
        let codex_root = temp_root("codex2");
        let pricing_path = temp_root("pricing_dir2").join("pricing.json");
        std::fs::write(claude_root.join("session1.jsonl"), b"").unwrap();
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(chrono::Local::now().date_naive()))));
        let mut worker = Worker::new(claude_root.clone(), codex_root.clone(), pricing_path, snapshot.clone()).unwrap();
        worker.startup();
        let line = format!(r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":7,"output_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#, today_ts(11));
        use std::io::Write;
        std::fs::OpenOptions::new().append(true).open(claude_root.join("session1.jsonl")).unwrap().write_all(format!("{}\n", line).as_bytes()).unwrap();
        worker.fast_tick();
        assert_eq!(crate::model::totals_for(&snapshot.lock().unwrap().clone(), None, None).input, 7);
        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }

    #[test]
    fn slow_tick_discovers_a_brand_new_file_created_after_startup() {
        let claude_root = temp_root("claude3");
        let codex_root = temp_root("codex3");
        let pricing_path = temp_root("pricing_dir3").join("pricing.json");
        let snapshot = Arc::new(Mutex::new(Arc::new(crate::model::Stats::new(chrono::Local::now().date_naive()))));
        let mut worker = Worker::new(claude_root.clone(), codex_root.clone(), pricing_path, snapshot.clone()).unwrap();
        worker.startup();
        let line = format!(r#"{{"type":"assistant","timestamp":"{}","message":{{"model":"claude-sonnet-4-6","usage":{{"input_tokens":3,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}}}}"#, today_ts(12));
        std::fs::write(claude_root.join("brand_new.jsonl"), format!("{}\n", line)).unwrap();
        worker.slow_tick();
        worker.fast_tick();
        assert_eq!(crate::model::totals_for(&snapshot.lock().unwrap().clone(), None, None).input, 3);
        let _ = std::fs::remove_dir_all(claude_root);
        let _ = std::fs::remove_dir_all(codex_root);
    }
}
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use chrono::Local;

use crate::discovery::{collect_mtimes, select_active, walk_jsonl_files};
use crate::model::{ingest_event, reprice, Stats};
use crate::parse_claude;
use crate::parse_codex::CodexSessionParser;
use crate::pricing::PricingTable;
use crate::tail::Tailer;

const ACTIVE_WINDOW: Duration = Duration::from_secs(30 * 60);
const STARTUP_WINDOW: Duration = Duration::from_secs(48 * 60 * 60);
pub const FAST_TICK: Duration = Duration::from_secs(2);
pub const SLOW_TICK: Duration = Duration::from_secs(20);

pub struct Worker {
    claude_root: PathBuf,
    codex_root: PathBuf,
    pricing_path: PathBuf,
    tailer: Tailer,
    codex_parsers: HashMap<PathBuf, CodexSessionParser>,
    active_set: Vec<PathBuf>,
    pricing: PricingTable,
    pricing_mtime: Option<SystemTime>,
    stats: Stats,
    snapshot: Arc<Mutex<Arc<Stats>>>,
}

impl Worker {
    pub fn new(
        claude_root: PathBuf,
        codex_root: PathBuf,
        pricing_path: PathBuf,
        snapshot: Arc<Mutex<Arc<Stats>>>,
    ) -> std::io::Result<Self> {
        let pricing = PricingTable::load_or_init(&pricing_path)?;
        let pricing_mtime = std::fs::metadata(&pricing_path).and_then(|m| m.modified()).ok();
        Ok(Self {
            claude_root,
            codex_root,
            pricing_path,
            tailer: Tailer::new(),
            codex_parsers: HashMap::new(),
            active_set: Vec::new(),
            pricing,
            pricing_mtime,
            stats: Stats::new(Local::now().date_naive()),
            snapshot,
        })
    }

    fn all_jsonl_files(&self) -> Vec<PathBuf> {
        let mut paths = walk_jsonl_files(&self.claude_root);
        paths.extend(walk_jsonl_files(&self.codex_root));
        paths
    }

    fn refresh_active_set(&mut self, window: Duration) {
        self.active_set = select_active(&collect_mtimes(&self.all_jsonl_files()), window, SystemTime::now());
    }

    pub fn startup(&mut self) {
        let recent = select_active(&collect_mtimes(&self.all_jsonl_files()), STARTUP_WINDOW, SystemTime::now());
        for path in recent {
            self.ingest_whole_file(&path);
        }
        self.refresh_active_set(ACTIVE_WINDOW);
    }

    fn ingest_whole_file(&mut self, path: &Path) {
        let Ok(bytes) = std::fs::read(path) else { return };
        let is_codex = path.starts_with(&self.codex_root);
        for line in String::from_utf8_lossy(&bytes).lines() {
            self.ingest_line(path, line, is_codex);
        }
        self.tailer.prime(path, bytes.len() as u64);
    }

    fn ingest_line(&mut self, path: &Path, line: &str, is_codex: bool) {
        let event = if is_codex {
            self.codex_parsers.entry(path.to_path_buf()).or_default().process_line(line)
        } else {
            parse_claude::parse_line(line)
        };
        if let Some(event) = event {
            ingest_event(&mut self.stats, &event, &self.pricing);
        }
    }

    pub fn fast_tick(&mut self) {
        self.stats.rollover_if_needed(Local::now().date_naive());
        for path in self.active_set.clone() {
            let is_codex = path.starts_with(&self.codex_root);
            match self.tailer.poll(&path) {
                Ok(lines) => for line in lines { self.ingest_line(&path, &line, is_codex); },
                Err(error) => eprintln!("warning: failed reading {}: {error}", path.display()),
            }
        }
        self.publish();
    }

    pub fn slow_tick(&mut self) {
        self.refresh_active_set(ACTIVE_WINDOW);
        self.reload_pricing_if_changed();
    }

    fn reload_pricing_if_changed(&mut self) {
        let mtime = std::fs::metadata(&self.pricing_path).and_then(|m| m.modified()).ok();
        if mtime != self.pricing_mtime {
            if let Ok(pricing) = PricingTable::load_or_init(&self.pricing_path) {
                self.pricing = pricing;
                self.pricing_mtime = mtime;
                reprice(&mut self.stats, &self.pricing);
            }
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
                self.slow_tick();
                last_slow_tick = std::time::Instant::now();
            }
        }
    }
}
