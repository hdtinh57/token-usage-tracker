const MAX_LEFTOVER: usize = 1024 * 1024;

pub struct LineSplitter {
    leftover: Vec<u8>,
}

impl LineSplitter {
    pub fn new() -> Self {
        LineSplitter {
            leftover: Vec::new(),
        }
    }

    pub fn feed(&mut self, chunk: &[u8]) -> Vec<String> {
        self.leftover.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(pos) = self.leftover.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.leftover.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1]).into_owned();
            lines.push(line);
        }
        if self.leftover.len() > MAX_LEFTOVER {
            eprintln!(
                "warning: discarding oversized partial line ({} bytes, no newline seen)",
                self.leftover.len()
            );
            self.leftover.clear();
        }
        lines
    }

    pub fn reset(&mut self) {
        self.leftover.clear();
    }
}

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

struct FileTailState {
    offset: u64,
    splitter: LineSplitter,
}

pub struct Tailer {
    files: HashMap<PathBuf, FileTailState>,
}

impl Tailer {
    pub fn new() -> Self {
        Tailer {
            files: HashMap::new(),
        }
    }

    pub fn prime(&mut self, path: &Path, offset: u64) {
        self.files.insert(
            path.to_path_buf(),
            FileTailState {
                offset,
                splitter: LineSplitter::new(),
            },
        );
    }

    pub fn poll(&mut self, path: &Path) -> std::io::Result<Vec<String>> {
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => {
                self.files.remove(path);
                return Ok(Vec::new());
            }
        };
        let current_size = metadata.len();
        let state = self
            .files
            .entry(path.to_path_buf())
            .or_insert_with(|| FileTailState {
                offset: 0,
                splitter: LineSplitter::new(),
            });

        if current_size < state.offset {
            eprintln!(
                "warning: {} shrank ({} -> {} bytes); correcting offset in place, not re-parsing",
                path.display(),
                state.offset,
                current_size
            );
            state.offset = current_size;
            state.splitter.reset();
        }

        if current_size == state.offset {
            return Ok(Vec::new());
        }

        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(state.offset))?;
        let mut buf = vec![0u8; (current_size - state.offset) as usize];
        file.read_exact(&mut buf)?;
        state.offset = current_size;
        Ok(state.splitter.feed(&buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_with_no_newline_yields_no_lines_yet() {
        let mut s = LineSplitter::new();
        let lines = s.feed(b"partial line no newline");
        assert!(lines.is_empty());
    }

    #[test]
    fn feed_completing_a_partial_line_yields_it() {
        let mut s = LineSplitter::new();
        s.feed(b"hello ");
        let lines = s.feed(b"world\n");
        assert_eq!(lines, vec!["hello world".to_string()]);
    }

    #[test]
    fn feed_with_multiple_lines_in_one_chunk_yields_all() {
        let mut s = LineSplitter::new();
        let lines = s.feed(b"a\nb\nc\n");
        assert_eq!(
            lines,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn trailing_partial_line_after_complete_ones_is_buffered_not_returned() {
        let mut s = LineSplitter::new();
        let lines = s.feed(b"a\nb\npartial");
        assert_eq!(lines, vec!["a".to_string(), "b".to_string()]);
        let more = s.feed(b" done\n");
        assert_eq!(more, vec!["partial done".to_string()]);
    }

    #[test]
    fn oversized_leftover_without_newline_is_discarded_not_grown_forever() {
        let mut s = LineSplitter::new();
        let big = vec![b'x'; MAX_LEFTOVER + 10];
        let lines = s.feed(&big);
        assert!(lines.is_empty());
        let lines2 = s.feed(b"fresh\n");
        assert_eq!(lines2, vec!["fresh".to_string()]);
    }

    #[test]
    fn reset_clears_buffered_partial_line() {
        let mut s = LineSplitter::new();
        s.feed(b"partial no newline");
        s.reset();
        let lines = s.feed(b"\n");
        assert_eq!(lines, vec!["".to_string()]);
    }
}

#[cfg(test)]
mod tailer_tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tt_tailer_test_{}_{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("log.jsonl")
    }

    #[test]
    fn poll_reads_lines_written_since_last_offset_only() {
        let path = temp_file("basic");
        std::fs::write(&path, b"line1\nline2\n").unwrap();
        let mut t = Tailer::new();

        let first = t.poll(&path).unwrap();
        assert_eq!(first, vec!["line1".to_string(), "line2".to_string()]);

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"line3\n").unwrap();
        drop(f);

        let second = t.poll(&path).unwrap();
        assert_eq!(second, vec!["line3".to_string()]);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn poll_on_unchanged_file_yields_no_new_lines() {
        let path = temp_file("unchanged");
        std::fs::write(&path, b"line1\n").unwrap();
        let mut t = Tailer::new();
        t.poll(&path).unwrap();
        let again = t.poll(&path).unwrap();
        assert!(again.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn truncation_corrects_offset_in_place_without_duplicating_old_prefix() {
        let path = temp_file("truncate");
        std::fs::write(&path, b"aaaaaaaaaa\nbbbbbbbbbb\n").unwrap();
        let mut t = Tailer::new();
        let first = t.poll(&path).unwrap();
        assert_eq!(first.len(), 2);

        // Simulate truncation: rewrite the file much shorter.
        std::fs::write(&path, b"short\n").unwrap();
        let after_truncate = t.poll(&path).unwrap();
        // Must not re-emit the old prefix, and must not error.
        assert!(after_truncate.is_empty() || after_truncate == vec!["short".to_string()]);

        // Subsequent appends must be picked up normally from the corrected offset.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"next\n").unwrap();
        drop(f);
        let after_append = t.poll(&path).unwrap();
        assert!(after_append.contains(&"next".to_string()));

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn deleted_file_is_removed_from_tracking_without_error() {
        let path = temp_file("delete");
        std::fs::write(&path, b"line1\n").unwrap();
        let mut t = Tailer::new();
        t.poll(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let result = t.poll(&path).unwrap();
        assert!(result.is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn prime_sets_offset_to_skip_already_ingested_content() {
        let path = temp_file("prime");
        std::fs::write(&path, b"already-read-during-startup-rescan\n").unwrap();
        let size = std::fs::metadata(&path).unwrap().len();

        let mut t = Tailer::new();
        t.prime(&path, size);
        let lines = t.poll(&path).unwrap();
        assert!(
            lines.is_empty(),
            "primed offset must skip pre-existing content"
        );

        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(b"new-line\n").unwrap();
        drop(f);
        let lines2 = t.poll(&path).unwrap();
        assert_eq!(lines2, vec!["new-line".to_string()]);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
