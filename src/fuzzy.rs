//! Fuzzy command palette engine (issue #86).
//!
//! Native fuzzy match across five sources — sessions, panes, tabs, the
//! frozen v1 action vocabulary, and recent dispatched commands. Designed
//! to be source-agnostic: callers feed `Entry` values into a `FuzzyIndex`
//! and ask for the top-N matches against the current query buffer.
//!
//! Renderer wiring (the bottom-anchored 8-row overlay, Up/Down navigation,
//! Tab completion) lives in render.rs and server.rs — kept off-limits per
//! the task brief. This module is the pure backend.
//!
//! ## Scoring
//!
//! Wraps `nucleo-matcher` (`nucleo_matcher::Matcher`) — same engine helix
//! uses. We pin the version because rerank stability is part of the #86
//! contract: a surprise rerank between releases breaks user muscle memory.
//!
//! ## History
//!
//! Recent commands persist to `$XDG_STATE_HOME/ezpn/history.toml` with a
//! 200-entry cap. Most-recent-first; duplicates collapse to a single
//! entry at the head.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Source of an entry in the unified candidate list.
///
/// The renderer uses this to badge each row (`session: …`, `pane: …`)
/// and to dispatch differently on `Enter` — selecting a session attaches,
/// selecting a pane focuses, selecting a command parses through
/// `commands::parse`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Session,
    Pane,
    Tab,
    Command,
    Recent,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Pane => "pane",
            Self::Tab => "tab",
            Self::Command => "command",
            Self::Recent => "recent",
        }
    }
}

/// One candidate row. `display` is what the user sees; `payload` is what
/// the dispatcher receives on `Enter` (often equal to `display` but free
/// to differ for multi-word entries like `"pane: nvim @ work/main"`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: EntryKind,
    pub display: String,
    pub payload: String,
    /// Optional priority bias added to the nucleo score. Recent entries
    /// get +500 to surface them above identical-name commands.
    pub bias: i32,
}

impl Entry {
    pub fn new(kind: EntryKind, display: impl Into<String>) -> Self {
        let display = display.into();
        Self {
            kind,
            payload: display.clone(),
            display,
            bias: 0,
        }
    }

    pub fn with_payload(mut self, payload: impl Into<String>) -> Self {
        self.payload = payload.into();
        self
    }

    pub fn with_bias(mut self, bias: i32) -> Self {
        self.bias = bias;
        self
    }
}

/// One match: index into the candidate list plus the (biased) score.
/// Higher scores rank higher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub index: usize,
    pub score: i32,
}

/// Fuzzy index over a snapshot of candidates. Cheap to construct;
/// callers rebuild on every keystroke if the source set changes.
pub struct FuzzyIndex {
    entries: Vec<Entry>,
    matcher: Matcher,
}

impl FuzzyIndex {
    pub fn new(entries: Vec<Entry>) -> Self {
        Self {
            entries,
            matcher: Matcher::default(),
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Score every entry against `query`, return top `limit` by score.
    /// Empty query returns every entry in declaration order with score 0.
    pub fn search(&mut self, query: &str, limit: usize) -> Vec<Match> {
        if query.is_empty() {
            return self
                .entries
                .iter()
                .enumerate()
                .take(limit)
                .map(|(i, e)| Match {
                    index: i,
                    score: e.bias,
                })
                .collect();
        }
        // `Pattern::parse` keeps the query as a single atom (no AND splits).
        // Smart-case is the right default: lower-case query is case-insensitive.
        let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);

        let mut scored: Vec<Match> = Vec::with_capacity(self.entries.len());
        let mut buf = Vec::new();
        for (i, entry) in self.entries.iter().enumerate() {
            buf.clear();
            let utf32 = nucleo_matcher::Utf32Str::new(&entry.display, &mut buf);
            if let Some(score) = pattern.score(utf32, &mut self.matcher) {
                let total = score as i32 + entry.bias;
                scored.push(Match {
                    index: i,
                    score: total,
                });
            }
        }
        // Higher score first; on ties, preserve declaration order.
        scored.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
        scored.truncate(limit);
        scored
    }
}

// ─── Recent-command history ───────────────────────────────

/// Cap on the on-disk history file. Matches the issue spec.
pub const HISTORY_CAP: usize = 200;
/// How many recent entries to surface in the palette by default.
pub const HISTORY_VIEW_CAP: usize = 20;

/// Persisted history file shape.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct History {
    /// Most-recent-first command list. No duplicates.
    #[serde(default)]
    pub commands: Vec<String>,
}

impl History {
    /// Push a freshly dispatched command to the front. Idempotent on the
    /// most-recent entry (typing the same command twice doesn't grow
    /// history); existing duplicates further back are collapsed up.
    pub fn push(&mut self, command: impl Into<String>) {
        let cmd = command.into();
        if cmd.trim().is_empty() {
            return;
        }
        // Remove existing copies (collapses duplicates anywhere in the list).
        self.commands.retain(|c| c != &cmd);
        self.commands.insert(0, cmd);
        if self.commands.len() > HISTORY_CAP {
            self.commands.truncate(HISTORY_CAP);
        }
    }

    /// View the top-N commands as palette entries. `Recent` entries get a
    /// +500 bias so they outrank identical-substring matches against the
    /// command vocabulary.
    pub fn as_entries(&self) -> Vec<Entry> {
        self.commands
            .iter()
            .take(HISTORY_VIEW_CAP)
            .map(|c| Entry::new(EntryKind::Recent, c.clone()).with_bias(500))
            .collect()
    }

    pub fn load(path: &std::path::Path) -> Self {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            // Missing or unreadable file is a normal first-run condition.
            Err(_) => return Self::default(),
        };
        toml::from_str(&raw).unwrap_or_default()
    }

    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = toml::to_string(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, body)
    }
}

/// Default on-disk path: `$XDG_STATE_HOME/ezpn/history.toml` with the
/// usual `~/.local/state/ezpn/...` fallback.
pub fn history_path() -> PathBuf {
    let dir = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let mut home = std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/tmp"));
            home.push(".local");
            home.push("state");
            home
        });
    dir.join("ezpn").join("history.toml")
}

// ─── Tests ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(s: &str) -> Entry {
        Entry::new(EntryKind::Command, s)
    }

    #[test]
    fn empty_query_returns_entries_in_order() {
        let mut idx = FuzzyIndex::new(vec![cmd("alpha"), cmd("beta"), cmd("gamma")]);
        let m = idx.search("", 10);
        assert_eq!(m.len(), 3);
        assert_eq!(m[0].index, 0);
        assert_eq!(m[1].index, 1);
        assert_eq!(m[2].index, 2);
    }

    #[test]
    fn ranks_kill_pane_above_unrelated_for_kil_query() {
        // From the #86 acceptance criteria: typing `kil` ranks `kill-pane`
        // above unrelated entries.
        let mut idx = FuzzyIndex::new(vec![
            cmd("split-window -h"),
            cmd("split-window -v"),
            cmd("new-window"),
            cmd("kill-pane"),
            cmd("kill-window"),
            cmd("rename-session"),
        ]);
        let m = idx.search("kil", 10);
        assert!(!m.is_empty(), "expected at least one match");
        let top = &idx.entries()[m[0].index];
        assert!(
            top.display.starts_with("kill"),
            "top match for 'kil' should start with 'kill', got {:?}",
            top.display
        );
    }

    #[test]
    fn limit_truncates_results() {
        let entries: Vec<Entry> = (0..50).map(|i| cmd(&format!("cmd-{i}"))).collect();
        let mut idx = FuzzyIndex::new(entries);
        let m = idx.search("cmd", 5);
        assert_eq!(m.len(), 5);
    }

    #[test]
    fn recent_bias_outranks_command_with_same_substring() {
        // A `Recent` "kill-pane" should beat a `Command` "kill-pane" when
        // both score equally on the query "kill".
        let mut idx = FuzzyIndex::new(vec![
            cmd("kill-pane"),
            Entry::new(EntryKind::Recent, "kill-pane").with_bias(500),
        ]);
        let m = idx.search("kill", 10);
        assert!(m.len() >= 2);
        // The biased Recent entry must come first.
        assert_eq!(idx.entries()[m[0].index].kind, EntryKind::Recent);
    }

    #[test]
    fn smart_case_lower_query_is_case_insensitive() {
        let mut idx = FuzzyIndex::new(vec![cmd("Kill-Pane"), cmd("rename-Session")]);
        let m = idx.search("kill", 10);
        assert!(!m.is_empty());
        assert!(idx.entries()[m[0].index].display.starts_with("Kill"));
    }

    #[test]
    fn no_match_returns_empty() {
        let mut idx = FuzzyIndex::new(vec![cmd("alpha"), cmd("beta")]);
        let m = idx.search("zzz", 10);
        assert!(m.is_empty());
    }

    // ─── History ─────────────────────────────────────────

    #[test]
    fn history_push_collapses_duplicates() {
        let mut h = History::default();
        h.push("split-window -h");
        h.push("kill-pane");
        h.push("split-window -h"); // duplicate -> moves to head
        assert_eq!(h.commands, vec!["split-window -h", "kill-pane"]);
    }

    #[test]
    fn history_caps_at_200() {
        let mut h = History::default();
        for i in 0..250 {
            h.push(format!("cmd-{i}"));
        }
        assert_eq!(h.commands.len(), HISTORY_CAP);
        // Most-recent-first: `cmd-249` is at the front.
        assert_eq!(h.commands[0], "cmd-249");
    }

    #[test]
    fn history_skips_blank() {
        let mut h = History::default();
        h.push("");
        h.push("   ");
        assert!(h.commands.is_empty());
    }

    #[test]
    fn history_persists_across_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("history.toml");
        let mut h = History::default();
        h.push("kill-pane");
        h.push("split-window");
        h.save(&path).unwrap();

        let loaded = History::load(&path);
        assert_eq!(loaded.commands, vec!["split-window", "kill-pane"]);
    }

    #[test]
    fn history_load_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let loaded = History::load(&path);
        assert!(loaded.commands.is_empty());
    }

    #[test]
    fn history_as_entries_caps_at_view_limit() {
        let mut h = History::default();
        for i in 0..50 {
            h.push(format!("cmd-{i}"));
        }
        assert_eq!(h.as_entries().len(), HISTORY_VIEW_CAP);
        assert_eq!(h.as_entries()[0].kind, EntryKind::Recent);
        assert_eq!(h.as_entries()[0].bias, 500);
    }

    // ─── Stress / FPS smoke ──────────────────────────────

    #[test]
    fn one_thousand_entries_search_meets_60fps_budget() {
        // 60 FPS budget = 16ms / frame. Searching 1k entries on every
        // keystroke must be well under that.
        //
        // The bound is enforced only in release mode — debug builds run
        // ~30x slower because of bounds-check overhead and would make this
        // test flaky on CI. Release-mode timings on a modern laptop sit
        // around ~300µs per search (~50x headroom).
        let entries: Vec<Entry> = (0..1000)
            .map(|i| cmd(&format!("pane: nvim-{i} @ session/main")))
            .collect();
        let mut idx = FuzzyIndex::new(entries);
        // Warm caches once.
        let _ = idx.search("nvim", 8);
        let start = std::time::Instant::now();
        const ITERS: u32 = 100;
        for _ in 0..ITERS {
            let _ = idx.search("nvim", 8);
        }
        let elapsed = start.elapsed();
        let per_iter = elapsed / ITERS;
        eprintln!(
            "fuzzy bench: 1000 entries x 100 iters = {elapsed:?} \
             ({:?} per search)",
            per_iter
        );

        #[cfg(not(debug_assertions))]
        assert!(
            per_iter.as_millis() < 16,
            "1k-entry search took {per_iter:?} — over the 16ms (60 FPS) frame budget"
        );

        // In debug mode, just smoke-test that the search completes — the
        // 60 FPS contract is only meaningful in release.
        #[cfg(debug_assertions)]
        {
            let _ = per_iter;
        }
    }
}
