use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub fn walk_jsonl_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_inner(root, &mut out);
    out
}

fn walk_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_inner(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

pub fn collect_mtimes(paths: &[PathBuf]) -> Vec<(PathBuf, SystemTime)> {
    paths
        .iter()
        .filter_map(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .map(|mt| (p.clone(), mt))
        })
        .collect()
}

pub fn select_active(entries: &[(PathBuf, SystemTime)], within: Duration, now: SystemTime) -> Vec<PathBuf> {
    entries
        .iter()
        .filter(|(_, mtime)| {
            now.duration_since(*mtime).unwrap_or(Duration::ZERO) <= within
        })
        .map(|(p, _)| p.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tt_discovery_test_{}_{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn walk_finds_jsonl_files_recursively_and_skips_other_extensions() {
        let root = temp_dir("walk");
        std::fs::create_dir_all(root.join("2026/07/13")).unwrap();
        std::fs::write(root.join("top.jsonl"), b"{}").unwrap();
        std::fs::write(root.join("2026/07/13/rollout.jsonl"), b"{}").unwrap();
        std::fs::write(root.join("notes.txt"), b"ignore me").unwrap();

        let mut found = walk_jsonl_files(&root);
        found.sort();
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|p| p.extension().unwrap() == "jsonl"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn walk_on_missing_root_returns_empty_not_an_error() {
        let missing = std::env::temp_dir().join("tt_discovery_definitely_does_not_exist");
        let found = walk_jsonl_files(&missing);
        assert!(found.is_empty());
    }

    #[test]
    fn select_active_filters_by_recency_window() {
        let now = SystemTime::now();
        let recent = now - Duration::from_secs(60);
        let old = now - Duration::from_secs(60 * 60 * 24 * 3);
        let entries = vec![
            (PathBuf::from("recent.jsonl"), recent),
            (PathBuf::from("old.jsonl"), old),
        ];
        let active = select_active(&entries, Duration::from_secs(30 * 60), now);
        assert_eq!(active, vec![PathBuf::from("recent.jsonl")]);
    }

    #[test]
    fn collect_mtimes_reads_real_file_metadata() {
        let root = temp_dir("mtimes");
        let file = root.join("a.jsonl");
        std::fs::write(&file, b"{}").unwrap();
        let mtimes = collect_mtimes(&[file.clone()]);
        assert_eq!(mtimes.len(), 1);
        assert_eq!(mtimes[0].0, file);
        let age = SystemTime::now()
            .duration_since(mtimes[0].1)
            .unwrap_or(Duration::ZERO);
        assert!(age < Duration::from_secs(60));

        let _ = std::fs::remove_dir_all(&root);
    }
}
