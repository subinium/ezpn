//! Named copy buffers (#91).
//!
//! tmux exposes a stack of named buffers used for paste history,
//! vim-like registers, and parking yanks across panes. v0.5 ezpn only
//! had one ephemeral OSC-52 buffer. This module is the server-side
//! [`BufferStore`] those features hang off.
//!
//! ## Storage model
//! - A `BTreeMap<String, BufferEntry>` keyed on the buffer name.
//!   Iteration order is alphabetic — stable for `:list-buffers`.
//! - The default buffer is the empty string `""`. Yanks land there
//!   unless the caller passes an explicit name; `:paste-buffer` with
//!   no `-b` reads from it.
//! - Per-buffer cap: **16 MiB** ([`BufferStore::MAX_BYTES`]).
//!   `set_buffer` rejects oversize text with [`SetError::TooLarge`].
//! - Whole-store cap: **100 buffers** ([`BufferStore::MAX_BUFFERS`]).
//!   When the cap is hit, the **oldest** entry is evicted —
//!   `BufferEntry::created_at` is a monotonic counter so eviction is
//!   deterministic and never panics.
//!
//! ## Out of scope (#91)
//! - **Persistence across daemon restart** — the store lives in RAM
//!   only. Snapshot integration is a follow-up.
//! - **Wire-up to `:set-buffer` / `:paste-buffer` / `:list-buffers` /
//!   `:save-buffer` / `:delete-buffer`**. The command palette
//!   (`server/input_modes.rs::InputMode::CommandPalette`) is off-limits
//!   for this slice. The integration is documented on the host branch
//!   PR.
//! - **OSC 52 mirroring**. Yank still emits OSC 52 (subject to the
//!   paste-injection guard from #79); the integration point is
//!   [`crate::copy_mode::yank_to_buffer`], also added in this slice —
//!   it pushes to the default buffer in addition to the existing OSC 52
//!   push that lives in `server/input_modes.rs`.

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

/// One entry in the [`BufferStore`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferEntry {
    /// The buffer payload. UTF-8 enforced at the boundary
    /// ([`BufferStore::set`]) so consumers can read it as `&str`
    /// without re-checking.
    pub text: String,
    /// Monotonic insertion sequence number. Used by the LRU eviction
    /// policy when the buffer-count cap trips. Never wraps in practice
    /// (u64) but the wrap-around case is handled defensively in
    /// `next_seq`.
    pub created_at: u64,
}

/// Server-side store of named copy buffers.
#[derive(Debug, Default)]
pub struct BufferStore {
    buffers: BTreeMap<String, BufferEntry>,
    next_seq: u64,
}

impl BufferStore {
    /// Maximum total number of buffers retained at once. Excess pushes
    /// to the **default** (unnamed) buffer evict the oldest entry; named
    /// `:set-buffer NAME` calls also evict if the cap is hit and the
    /// name is new.
    pub const MAX_BUFFERS: usize = 100;
    /// Per-buffer cap. Anything larger is rejected with
    /// [`SetError::TooLarge`] so a runaway yank cannot OOM the daemon.
    pub const MAX_BYTES: usize = 16 * 1024 * 1024;
    /// Reserved name for the default LIFO buffer. Yanks land here when
    /// no explicit `-b NAME` is given.
    pub const DEFAULT_NAME: &'static str = "";

    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a buffer. Existing names keep their slot
    /// (no eviction); new names trigger oldest-first eviction once
    /// the count cap is hit.
    pub fn set(
        &mut self,
        name: impl Into<String>,
        text: impl Into<String>,
    ) -> Result<(), SetError> {
        let text = text.into();
        if text.len() > Self::MAX_BYTES {
            return Err(SetError::TooLarge {
                name: name.into(),
                size: text.len(),
                cap: Self::MAX_BYTES,
            });
        }
        let name = name.into();
        let seq = self.next_seq();

        let is_new = !self.buffers.contains_key(&name);
        if is_new && self.buffers.len() >= Self::MAX_BUFFERS {
            self.evict_oldest();
        }
        self.buffers.insert(
            name,
            BufferEntry {
                text,
                created_at: seq,
            },
        );
        Ok(())
    }

    /// Fetch the buffer named `name`. `None` if the slot is empty.
    pub fn get(&self, name: &str) -> Option<&BufferEntry> {
        self.buffers.get(name)
    }

    /// Fetch the default buffer (`""`). Convenience wrapper for the
    /// `:paste-buffer` no-arg path.
    pub fn default_buffer(&self) -> Option<&BufferEntry> {
        self.get(Self::DEFAULT_NAME)
    }

    /// Remove a buffer. Returns the evicted entry, or `None` if the
    /// name was unknown.
    pub fn delete(&mut self, name: &str) -> Option<BufferEntry> {
        self.buffers.remove(name)
    }

    /// All buffers in alphabetical order. Used by `:list-buffers`
    /// (and the fuzzy picker built on top of it once #86 lands).
    pub fn list(&self) -> impl Iterator<Item = (&str, &BufferEntry)> {
        self.buffers.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Number of buffers currently stored.
    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    /// True iff the store is empty.
    pub fn is_empty(&self) -> bool {
        self.buffers.is_empty()
    }

    /// Save a buffer to disk with mode `0600`. Used by `:save-buffer`.
    /// We refuse to clobber existing files unless the caller passes
    /// `truncate = true` — the safer default for a clipboard write.
    pub fn save(&self, name: &str, path: &Path, truncate: bool) -> Result<usize, SaveError> {
        let entry = self.get(name).ok_or_else(|| SaveError::NoSuchBuffer {
            name: name.to_string(),
        })?;
        let mut opts = OpenOptions::new();
        opts.write(true).create(true);
        if truncate {
            opts.truncate(true);
        } else {
            opts.create_new(true);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Mode 0600 — never world- or group-readable. Matches the
            // socket permissions hardened in #57.
            opts.mode(0o600);
        }
        let mut f = opts.open(path).map_err(|e| SaveError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        f.write_all(entry.text.as_bytes())
            .map_err(|e| SaveError::Io {
                path: path.to_path_buf(),
                source: e,
            })?;
        Ok(entry.text.len())
    }

    fn next_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        // Defensive wrap — in practice we never hit u64::MAX yanks but
        // we still keep the counter well-defined.
        self.next_seq = self.next_seq.wrapping_add(1);
        seq
    }

    /// Drop the entry with the smallest `created_at`. We scan the map
    /// (it is bounded by `MAX_BUFFERS = 100` so the cost is trivial)
    /// rather than maintain a side index — keeps eviction always
    /// correct after a `set` that overwrites a name.
    fn evict_oldest(&mut self) {
        let oldest = self
            .buffers
            .iter()
            .min_by_key(|(_, v)| v.created_at)
            .map(|(k, _)| k.clone());
        if let Some(k) = oldest {
            self.buffers.remove(&k);
        }
    }
}

/// Errors from [`BufferStore::set`].
#[derive(Debug)]
pub enum SetError {
    /// Payload exceeded the per-buffer cap ([`BufferStore::MAX_BYTES`]).
    TooLarge {
        name: String,
        size: usize,
        cap: usize,
    },
}

impl std::fmt::Display for SetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { name, size, cap } => write!(
                f,
                "buffer '{name}' rejected: {size} bytes exceeds {cap} byte cap"
            ),
        }
    }
}

impl std::error::Error for SetError {}

/// Errors from [`BufferStore::save`].
#[derive(Debug)]
pub enum SaveError {
    NoSuchBuffer {
        name: String,
    },
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchBuffer { name } => write!(f, "no buffer named '{name}'"),
            Self::Io { path, source } => write!(f, "writing {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for SaveError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_default_buffer() {
        let mut s = BufferStore::new();
        s.set(BufferStore::DEFAULT_NAME, "hello").unwrap();
        assert_eq!(s.default_buffer().unwrap().text, "hello");
    }

    #[test]
    fn named_buffer_round_trip() {
        let mut s = BufferStore::new();
        s.set("foo", "alpha").unwrap();
        s.set("bar", "beta").unwrap();
        assert_eq!(s.get("foo").unwrap().text, "alpha");
        assert_eq!(s.get("bar").unwrap().text, "beta");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn list_returns_alphabetic_order() {
        let mut s = BufferStore::new();
        s.set("zeta", "z").unwrap();
        s.set("alpha", "a").unwrap();
        s.set("mu", "m").unwrap();
        let names: Vec<&str> = s.list().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn delete_removes_named_buffer() {
        let mut s = BufferStore::new();
        s.set("foo", "x").unwrap();
        let evicted = s.delete("foo").unwrap();
        assert_eq!(evicted.text, "x");
        assert!(s.get("foo").is_none());
        assert!(s.delete("missing").is_none());
    }

    #[test]
    fn replace_existing_buffer_preserves_count() {
        let mut s = BufferStore::new();
        s.set("foo", "first").unwrap();
        s.set("foo", "second").unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!(s.get("foo").unwrap().text, "second");
    }

    #[test]
    fn oversize_payload_is_rejected() {
        let mut s = BufferStore::new();
        let big = "a".repeat(BufferStore::MAX_BYTES + 1);
        let err = s.set("foo", big).unwrap_err();
        match err {
            SetError::TooLarge { name, size, cap } => {
                assert_eq!(name, "foo");
                assert_eq!(size, BufferStore::MAX_BYTES + 1);
                assert_eq!(cap, BufferStore::MAX_BYTES);
            }
        }
        // Store stays untouched.
        assert!(s.is_empty());
    }

    #[test]
    fn payload_at_cap_is_accepted() {
        let mut s = BufferStore::new();
        let exact = "x".repeat(BufferStore::MAX_BYTES);
        s.set("foo", exact).expect("at-cap payload accepted");
        assert_eq!(s.get("foo").unwrap().text.len(), BufferStore::MAX_BYTES);
    }

    #[test]
    fn evicts_oldest_when_buffer_count_cap_hit() {
        let mut s = BufferStore::new();
        for i in 0..BufferStore::MAX_BUFFERS {
            s.set(format!("buf{i:03}"), format!("v{i}")).unwrap();
        }
        assert_eq!(s.len(), BufferStore::MAX_BUFFERS);
        // Pushing one more must drop `buf000` (oldest seq).
        s.set("overflow", "new").unwrap();
        assert_eq!(s.len(), BufferStore::MAX_BUFFERS);
        assert!(s.get("buf000").is_none());
        assert_eq!(s.get("overflow").unwrap().text, "new");
        // The next-oldest buf001 should still be there.
        assert!(s.get("buf001").is_some());
    }

    #[test]
    fn replacing_named_buffer_at_cap_does_not_evict() {
        let mut s = BufferStore::new();
        for i in 0..BufferStore::MAX_BUFFERS {
            s.set(format!("buf{i:03}"), format!("v{i}")).unwrap();
        }
        // Re-setting an existing name keeps the count constant — no
        // eviction needed.
        s.set("buf050", "updated").unwrap();
        assert_eq!(s.len(), BufferStore::MAX_BUFFERS);
        assert!(s.get("buf000").is_some());
        assert_eq!(s.get("buf050").unwrap().text, "updated");
    }

    #[test]
    fn save_writes_file_with_0600_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("clip.txt");
        let mut s = BufferStore::new();
        s.set("foo", "payload").unwrap();
        let n = s.save("foo", &path, true).unwrap();
        assert_eq!(n, "payload".len());
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body, "payload");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "file must be private (0600)");
        }
    }

    #[test]
    fn save_unknown_buffer_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.txt");
        let s = BufferStore::new();
        let err = s.save("nope", &path, true).unwrap_err();
        match err {
            SaveError::NoSuchBuffer { name } => assert_eq!(name, "nope"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn save_refuses_to_clobber_when_truncate_false() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("preexisting.txt");
        std::fs::write(&path, "do not overwrite").unwrap();
        let mut s = BufferStore::new();
        s.set("foo", "new").unwrap();
        let err = s.save("foo", &path, false).unwrap_err();
        assert!(matches!(err, SaveError::Io { .. }));
        // Original content untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "do not overwrite");
    }
}
