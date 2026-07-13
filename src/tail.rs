const MAX_LEFTOVER: usize = 1024 * 1024;

pub struct LineSplitter {
    leftover: Vec<u8>,
}

impl LineSplitter {
    pub fn new() -> Self {
        LineSplitter { leftover: Vec::new() }
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
        assert_eq!(lines, vec!["a".to_string(), "b".to_string(), "c".to_string()]);
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
