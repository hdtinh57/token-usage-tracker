use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const DEBOUNCE: Duration = Duration::from_millis(250);

pub struct PendingPaths {
    paths: HashMap<PathBuf, Instant>,
}

impl PendingPaths {
    pub fn new() -> Self {
        Self {
            paths: HashMap::new(),
        }
    }

    pub fn push(&mut self, path: PathBuf, now: Instant) {
        if path.extension().and_then(|extension| extension.to_str()) == Some("jsonl") {
            self.paths.insert(path, now);
        }
    }

    pub fn take_ready(&mut self, now: Instant) -> Vec<PathBuf> {
        let mut ready = Vec::new();
        self.paths.retain(|path, queued_at| {
            if now.duration_since(*queued_at) >= DEBOUNCE {
                ready.push(path.clone());
                false
            } else {
                true
            }
        });
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    #[test]
    fn pending_paths_keeps_jsonl_only_and_debounces_duplicates_for_250ms() {
        let start = Instant::now();
        let log = PathBuf::from("session.jsonl");
        let mut pending = PendingPaths::new();

        pending.push(log.clone(), start);
        pending.push(PathBuf::from("notes.txt"), start);
        pending.push(log.clone(), start + Duration::from_millis(100));

        assert!(
            pending
                .take_ready(start + Duration::from_millis(249))
                .is_empty()
        );
        assert_eq!(
            pending.take_ready(start + Duration::from_millis(350)),
            vec![log]
        );
    }
}
