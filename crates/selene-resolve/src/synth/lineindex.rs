//! Byte-offset → line number, in O(log n).
//!
//! # Why this is not `src[..off].split('\n').count()`
//!
//! Every synthesizer and every framework extractor turns a regex match's byte
//! offset into a 1-based line (the line goes into a node id, or into an edge's
//! `registeredAt`). Done the naive way — re-slicing and re-counting newlines per
//! match — a file with *m* matches costs O(n·m), which on a large file is
//! quadratic. That was TS #1235: the ungated per-match `slice().split()` made
//! synthesis take 20+ minutes on real corpora.
//!
//! Build the newline table **once per file**, then binary-search it.

/// Byte offsets of every `\n` in a source file, ascending.
pub struct LineIndex {
    newlines: Vec<usize>,
}

impl LineIndex {
    /// One pass over the source.
    pub fn new(src: &str) -> Self {
        Self {
            newlines: src
                .bytes()
                .enumerate()
                .filter(|(_, b)| *b == b'\n')
                .map(|(i, _)| i)
                .collect(),
        }
    }

    /// The 1-based line containing `byte_offset`.
    ///
    /// An offset **on** a `\n` belongs to the line that newline *terminates* —
    /// i.e. the line it ends, not the next one. (A match never starts on a
    /// newline in practice; the rule is pinned so the boundary is not a coin
    /// flip.)
    pub fn line_at(&self, byte_offset: usize) -> u32 {
        // `partition_point` = the count of newlines strictly before the offset.
        // Lines are 1-based, so add 1.
        1 + self.newlines.partition_point(|&nl| nl < byte_offset) as u32
    }

    /// Number of lines (a trailing newline does not open a new one).
    pub fn len(&self) -> usize {
        self.newlines.len() + 1
    }

    pub fn is_empty(&self) -> bool {
        false // there is always at least one line
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The property: for every offset, `line_at` agrees with a naive count.
    /// Multi-byte UTF-8 included — the index is over BYTES, and a char boundary
    /// is irrelevant to a newline count.
    #[test]
    fn line_at_agrees_with_the_naive_count_everywhere() {
        let src = "aaa\nbb→bb\n\nccc\u{feff}\nddd";
        let idx = LineIndex::new(src);

        for off in 0..=src.len() {
            let naive = 1 + src.as_bytes()[..off]
                .iter()
                .filter(|&&b| b == b'\n')
                .count() as u32;
            assert_eq!(
                idx.line_at(off),
                naive,
                "offset {off} (byte {:?})",
                src.as_bytes().get(off)
            );
        }
    }

    /// An offset sitting exactly ON a newline belongs to the line that newline
    /// terminates.
    #[test]
    fn an_offset_on_a_newline_belongs_to_the_line_it_ends() {
        let src = "a\nb\nc";
        let idx = LineIndex::new(src);
        assert_eq!(idx.line_at(0), 1, "'a'");
        assert_eq!(idx.line_at(1), 1, "the \\n ending line 1");
        assert_eq!(idx.line_at(2), 2, "'b'");
        assert_eq!(idx.line_at(3), 2, "the \\n ending line 2");
        assert_eq!(idx.line_at(4), 3, "'c'");
    }

    #[test]
    fn a_source_with_no_newlines_is_one_line() {
        let idx = LineIndex::new("no newlines here");
        assert_eq!(idx.line_at(0), 1);
        assert_eq!(idx.line_at(10), 1);
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn an_empty_source_is_one_line() {
        let idx = LineIndex::new("");
        assert_eq!(idx.line_at(0), 1);
        assert_eq!(idx.len(), 1);
    }
}
