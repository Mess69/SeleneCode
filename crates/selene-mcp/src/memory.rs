//! The session-memory journal (graph-platform PRD 2026-08-18 §6 / F5).
//!
//! Every successful `explore` appends one compact line to
//! `.selene/memory.jsonl`: when, what was asked, and the answer's headline
//! (the roots explore started from). `recall` reads it back — "you explored
//! this on the 12th; the roots were X and Y" — so knowledge accumulates
//! across sessions instead of evaporating with them.
//!
//! Deliberately OUT of the graph and OUT of explore's output: explore stays
//! byte-deterministic (its goldens unchanged); the journal is a side file the
//! purge audit already covers (purge deletes `.selene/` wholesale). Local
//! only — the "nothing leaves the machine" invariant covers memory too.
//! Opt-out: `SELENE_NO_MEMORY=1`. Best-effort: a full disk or a read-only
//! `.selene/` must never fail the explore that tried to journal.

use std::io::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// One remembered exploration.
#[derive(Debug, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unix millis at append time.
    pub ts: u64,
    /// The question as asked.
    pub query: String,
    /// The answer's headline — explore's "Starting from: …" seeds line, when found.
    pub headline: String,
}

/// The journal's cap: oldest lines are dropped past this many.
const MAX_ENTRIES: usize = 500;

fn journal_path(root: &Path) -> std::path::PathBuf {
    root.join(".selene").join("memory.jsonl")
}

fn memory_disabled() -> bool {
    std::env::var("SELENE_NO_MEMORY").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Append one entry. Best-effort by contract: every failure is swallowed.
pub fn remember(root: &Path, query: &str, answer_text: &str) {
    if memory_disabled() {
        return;
    }
    let headline = answer_text
        .lines()
        .find(|l| l.contains("Starting from:"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let entry = MemoryEntry {
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        query: query.chars().take(300).collect(),
        headline: headline.chars().take(300).collect(),
    };
    let path = journal_path(root);
    let Ok(line) = serde_json::to_string(&entry) else {
        return;
    };
    // Cap by rewriting when oversized — cheap at 500 × ~300 B.
    let existing = read(root);
    if existing.len() >= MAX_ENTRIES {
        let keep: Vec<&MemoryEntry> = existing[existing.len() - (MAX_ENTRIES - 1)..]
            .iter()
            .collect();
        let mut buf = String::new();
        for e in keep {
            if let Ok(l) = serde_json::to_string(e) {
                buf.push_str(&l);
                buf.push('\n');
            }
        }
        buf.push_str(&line);
        buf.push('\n');
        let _ = std::fs::write(&path, buf);
        return;
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{line}");
    }
}

/// Read the whole journal (empty on any failure — an answer, not an error).
pub fn read(root: &Path) -> Vec<MemoryEntry> {
    let Ok(text) = std::fs::read_to_string(journal_path(root)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Render `recall`: the past explorations most relevant to `query` (word
/// overlap; empty query = the most recent ones). Success-shaped always.
pub fn render_recall(root: &Path, query: Option<&str>) -> String {
    let entries = read(root);
    if entries.is_empty() {
        return "## Nothing remembered yet\n\nEvery successful `explore` is journaled here; \
                ask a few questions first. (Local only; opt out with `SELENE_NO_MEMORY=1`.)\n"
            .to_string();
    }

    let scored: Vec<&MemoryEntry> = match query {
        Some(q) if !q.trim().is_empty() => {
            let words: Vec<String> = q
                .to_lowercase()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let mut with_scores: Vec<(usize, &MemoryEntry)> = entries
                .iter()
                .map(|e| {
                    let hay = format!("{} {}", e.query, e.headline).to_lowercase();
                    (words.iter().filter(|w| hay.contains(*w)).count(), e)
                })
                .filter(|(s, _)| *s > 0)
                .collect();
            with_scores.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.ts.cmp(&a.1.ts)));
            with_scores.into_iter().take(5).map(|(_, e)| e).collect()
        }
        _ => entries.iter().rev().take(5).collect(),
    };

    if scored.is_empty() {
        return "## No remembered exploration matches\n\nNothing in this project's journal \
                touches those words. Ask `explore` — the answer will be remembered.\n"
            .to_string();
    }

    let mut out = String::from("## Remembered explorations\n\n");
    for e in &scored {
        let days_ms = 86_400_000;
        let age = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| (d.as_millis() as u64).saturating_sub(e.ts) / days_ms)
            .unwrap_or(0);
        let when = if age == 0 {
            "today".to_string()
        } else {
            format!("{age}d ago")
        };
        out.push_str(&format!("- **{}** ({when})\n", e.query));
        if !e.headline.is_empty() {
            out.push_str(&format!("  {}\n", e.headline));
        }
    }
    out.push_str("\nRe-run `explore` with any of these questions for the full, current answer.\n");
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn temp_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".selene")).unwrap();
        dir
    }

    #[test]
    fn remember_then_recall_finds_the_matching_question() {
        let root = temp_root();
        remember(
            root.path(),
            "how does the daemon shut down",
            "Found 12 symbols. Starting from: `run_daemon`, `accept_loop`.\n",
        );
        remember(
            root.path(),
            "what renders the galaxy",
            "Starting from: `build_data`.\n",
        );
        let md = render_recall(root.path(), Some("daemon shutdown"));
        assert!(md.contains("how does the daemon shut down"), "{md}");
        assert!(md.contains("run_daemon"), "headline kept: {md}");
        assert!(!md.contains("galaxy"), "unrelated entry filtered: {md}");
    }

    #[test]
    fn empty_journal_and_no_match_are_answers_not_errors() {
        let root = temp_root();
        assert!(render_recall(root.path(), None).contains("Nothing remembered"));
        remember(root.path(), "a", "b");
        assert!(render_recall(root.path(), Some("zzz")).contains("No remembered exploration"));
    }

    #[test]
    fn journaling_never_fails_the_caller_even_without_a_selene_dir() {
        let dir = tempfile::tempdir().unwrap(); // no .selene/
        remember(dir.path(), "q", "a"); // must not panic
        assert!(read(dir.path()).is_empty());
    }
}
