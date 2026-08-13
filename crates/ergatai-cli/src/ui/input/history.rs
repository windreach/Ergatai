//! Persistent input history.
//!
//! Stored as JSONL (one JSON object per line) in `{project_root}/.ergatai/chat_history.jsonl`
//! or `~/.ergatai/chat_history.jsonl` as a fallback.
//!
//! At most [`MAX_ENTRIES`] entries are kept in memory. When the file grows
//! beyond that, older entries are dropped on load (the file itself is
//! append-only and trimmed lazily on rewrite).

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Maximum number of history entries kept in memory.
const MAX_ENTRIES: usize = 1000;

/// One persisted history record.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct HistoryRecord {
    text: String,
    #[serde(default)]
    ts: String,
}

/// Persistent input history with an up/down browsing cursor.
pub struct InputHistory {
    /// In-memory cache of entries (oldest → newest).
    entries: Vec<String>,
    /// Path to the JSONL history file.
    path: PathBuf,
    /// Current position when browsing history (`None` = at fresh input).
    cursor: Option<usize>,
    /// The user's draft input (saved when they first hit ↑).
    draft: String,
}

impl InputHistory {
    /// Load history from disk.
    ///
    /// Resolution order for the history file:
    /// 1. `{cwd}/.ergatai/chat_history.jsonl` if `.ergatai/` exists in cwd.
    /// 2. Walk up from cwd looking for `.ergatai/` (project root detection).
    /// 3. Fallback: `~/.ergatai/chat_history.jsonl`.
    pub fn load() -> Self {
        let path = resolve_history_path();
        let entries = load_entries(&path, MAX_ENTRIES);
        Self {
            entries,
            path,
            cursor: None,
            draft: String::new(),
        }
    }

    /// Record a new input. Skips empty strings and duplicates of the most
    /// recent entry. Appends to the file in JSONL format.
    pub fn add(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.entries.last().map(|s| s.as_str()) == Some(trimmed) {
            // Duplicate of last entry — just reset cursor.
            self.cursor = None;
            self.draft.clear();
            return;
        }
        self.entries.push(trimmed.to_string());
        self.cursor = None;
        self.draft.clear();
        append_record(&self.path, trimmed);
    }

    /// Move the cursor backward (older entry). Returns the entry to display.
    ///
    /// On the first call, the current `input` is saved as `draft` so it can
    /// be restored when the user cycles past the newest entry.
    pub fn prev(&mut self, current_input: &str) -> Option<&str> {
        if self.entries.is_empty() {
            return None;
        }
        match self.cursor {
            None => {
                self.draft = current_input.to_string();
                self.cursor = Some(self.entries.len() - 1);
            }
            Some(0) => {
                // Already at oldest — stay put.
            }
            Some(i) => {
                self.cursor = Some(i - 1);
            }
        }
        self.cursor.map(|i| self.entries[i].as_str())
    }

    /// Move the cursor forward (newer entry). Returns the entry to display,
    /// or the original `draft` when cycling past the newest entry.
    pub fn next(&mut self) -> Option<&str> {
        match self.cursor {
            None => None,
            Some(i) if i + 1 >= self.entries.len() => {
                // Past the newest — return the saved draft.
                self.cursor = None;
                Some(&self.draft)
            }
            Some(i) => {
                self.cursor = Some(i + 1);
                self.cursor.map(|i| self.entries[i].as_str())
            }
        }
    }

    /// Reset the browsing cursor (e.g. after sending or clearing input).
    pub fn reset(&mut self) {
        self.cursor = None;
        self.draft.clear();
    }

    /// Number of entries in memory.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the history is empty.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Walk up from `cwd` looking for a directory containing `.ergatai/`.
/// Falls back to `~/.ergatai/`.
fn resolve_history_path() -> PathBuf {
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join(".ergatai").join("chat_history.jsonl");
            if dir.join(".ergatai").is_dir() {
                return candidate;
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
    }
    // Fallback: ~/.ergatai/chat_history.jsonl
    if let Some(home) = home_dir() {
        let p = home.join(".ergatai").join("chat_history.jsonl");
        // Ensure parent exists.
        if let Some(parent) = p.parent() {
            let _ = fs::create_dir_all(parent);
        }
        return p;
    }
    // Absolute last resort — write into current dir.
    PathBuf::from(".ergatai_chat_history.jsonl")
}

/// Best-effort home directory resolution (no extra deps).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

/// Read up to `max` entries from a JSONL file (keeps the last `max` lines).
fn load_entries(path: &PathBuf, max: usize) -> Vec<String> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut all: Vec<String> = Vec::new();
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<HistoryRecord>(trimmed) {
            Ok(rec) if !rec.text.is_empty() => all.push(rec.text),
            _ => continue,
        }
    }
    // Keep only the last `max` entries.
    if all.len() > max {
        all.drain(..all.len() - max);
    }
    all
}

/// Append one record to the JSONL file. Creates parent dirs if needed.
fn append_record(path: &PathBuf, text: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let record = HistoryRecord {
        text: text.to_string(),
        ts: chrono::Utc::now().to_rfc3339(),
    };
    let line = match serde_json::to_string(&record) {
        Ok(l) => l,
        Err(_) => return,
    };
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ergatai_hist_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::create_dir_all(&dir);
        dir.join("chat_history.jsonl")
    }

    fn make_hist(path: PathBuf) -> InputHistory {
        InputHistory {
            entries: Vec::new(),
            path,
            cursor: None,
            draft: String::new(),
        }
    }

    #[test]
    fn test_add_and_prev_next() {
        let path = temp_path();
        let mut h = make_hist(path.clone());

        h.add("first");
        h.add("second");
        h.add("third");
        assert_eq!(h.len(), 3);

        // Start with draft "current input".
        assert_eq!(h.prev("current"), Some("third"));
        assert_eq!(h.prev(""), Some("second"));
        assert_eq!(h.prev(""), Some("first"));
        // At oldest — stays.
        assert_eq!(h.prev(""), Some("first"));

        // Cycle forward.
        assert_eq!(h.next(), Some("second"));
        assert_eq!(h.next(), Some("third"));
        assert_eq!(h.next(), Some("current")); // restored draft
        assert_eq!(h.next(), None); // cursor reset

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_skip_empty_and_duplicate() {
        let path = temp_path();
        let mut h = make_hist(path.clone());

        h.add("");
        h.add("   ");
        assert!(h.is_empty());

        h.add("hello");
        h.add("hello");
        assert_eq!(h.len(), 1);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_persistence() {
        let path = temp_path();
        {
            let mut h = make_hist(path.clone());
            h.add("alpha");
            h.add("beta");
        }
        // Reload from disk.
        let entries = load_entries(&path, MAX_ENTRIES);
        assert_eq!(entries, vec!["alpha".to_string(), "beta".to_string()]);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_reset_clears_cursor() {
        let path = temp_path();
        let mut h = make_hist(path.clone());
        h.add("one");
        h.add("two");

        assert_eq!(h.prev("draft"), Some("two"));
        h.reset();
        // After reset, prev starts again from newest.
        assert_eq!(h.prev("new draft"), Some("two"));

        let _ = fs::remove_file(&path);
    }
}
