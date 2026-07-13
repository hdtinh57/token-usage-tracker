use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Write};
use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::model::Source;

const THRESHOLDS: [u8; 4] = [80, 90, 95, 100];

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct AlertKey {
    source: Source,
    window: String,
    reset_at: chrono::DateTime<Utc>,
    threshold: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertState {
    emitted: HashSet<AlertKey>,
}

impl AlertState {
    pub fn load(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        serde_json::from_reader(BufReader::new(File::open(path)?))
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let temp = path.with_file_name("alerts.json.tmp");
        let file = File::create(&temp)?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        writer.flush()?;
        writer.get_ref().sync_all()?;
        std::fs::rename(temp, path)
    }

    pub fn crossed(
        &mut self,
        source: Source,
        window: &str,
        utilization: f64,
        reset_at: chrono::DateTime<Utc>,
    ) -> Vec<u8> {
        self.emitted
            .retain(|key| key.source != source || key.window != window || key.reset_at == reset_at);
        THRESHOLDS
            .into_iter()
            .filter(|&threshold| utilization >= f64::from(threshold))
            .filter(|&threshold| {
                self.emitted.insert(AlertKey {
                    source,
                    window: window.to_owned(),
                    reset_at,
                    threshold,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_threshold_is_emitted_once_per_reset_window() {
        let reset = Utc::now() + chrono::Duration::hours(1);
        let mut alerts = AlertState::default();
        assert_eq!(
            alerts.crossed(Source::Claude, "session", 91.0, reset),
            vec![80, 90]
        );
        assert!(
            alerts
                .crossed(Source::Claude, "session", 96.0, reset)
                .contains(&95)
        );
        assert!(
            alerts
                .crossed(Source::Claude, "session", 96.0, reset)
                .is_empty()
        );
    }

    #[test]
    fn a_new_reset_prunes_the_previous_window_and_persists() {
        let dir = temp_dir("alerts");
        let path = dir.join("alerts.json");
        let first = Utc::now() + chrono::Duration::hours(1);
        let second = first + chrono::Duration::hours(5);
        let mut alerts = AlertState::default();

        assert_eq!(
            alerts.crossed(Source::Codex, "session", 80.0, first),
            vec![80]
        );
        assert_eq!(
            alerts.crossed(Source::Codex, "session", 80.0, second),
            vec![80]
        );
        alerts.save(&path).unwrap();
        let mut replacement = AlertState::default();
        replacement.crossed(Source::Claude, "week", 90.0, second);
        replacement.save(&path).unwrap();

        assert_eq!(AlertState::load(&path).unwrap(), replacement);
        assert!(!path.with_file_name("alerts.json.tmp").exists());
        let _ = std::fs::remove_dir_all(dir);
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tt_{name}_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
