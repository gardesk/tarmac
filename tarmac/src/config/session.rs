//! Session persistence — remember which workspace each window lived on so a
//! daemon restart (or `reload`-restart) restores the layout instead of piling
//! every pre-existing window onto the active workspace.
//!
//! Window ids do not survive an app restart, and macOS on Tahoe poisons the
//! Space-enumeration APIs (see `ers/CLAUDE.md`), so we cannot key on the wid or
//! ask the OS where a window "was". We key instead on a stable app+title
//! identity captured at snapshot time and match it back at discovery.
//!
//! Matching is deliberately forgiving: an exact `(bundle_id, title)` match wins,
//! but a window whose title has drifted since last session (terminal cwd,
//! browser tab) still lands on the right workspace via a `bundle_id`-only
//! fallback. Entries are *consumed* on match so N windows of the same app
//! distribute across their N saved slots rather than all snapping to the first.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One remembered window placement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEntry {
    pub bundle_id: String,
    pub title: String,
    /// `WorkspaceId::Display` form — `"3"`, `"W"`, or `"special:scratch"`.
    pub workspace: String,
}

/// The persisted layout: an ordered list of placements.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub entries: Vec<SessionEntry>,
}

impl Session {
    /// Default on-disk location: `~/.config/tarmac/session.json`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::home_dir().map(|home| {
            home.join(".config")
                .join("tarmac")
                .join("session.json")
        })
    }

    /// Load a session file. A missing or unparseable file is not an error —
    /// it just means "no remembered layout", so we return an empty session.
    pub fn load(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        match serde_json::from_slice(&bytes) {
            Ok(session) => session,
            Err(e) => {
                tracing::warn!(path = %path.display(), err = %e, "ignoring unreadable session file");
                Self::default()
            }
        }
    }

    /// Atomically write the session to disk (temp file + rename) so a crash
    /// mid-write can never leave a truncated file that would then be silently
    /// discarded on the next load.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, path)
    }

    /// Consume the best-matching entry for a window and return its workspace
    /// string. Exact `(bundle_id, title)` first, then `bundle_id`-only. A
    /// window with no bundle id (e.g. an unsigned helper) never matches, so it
    /// falls through to the caller's default placement. Returns `None` when
    /// nothing matches.
    pub fn take_match(&mut self, bundle_id: &str, title: &str) -> Option<String> {
        if bundle_id.is_empty() {
            return None;
        }
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.bundle_id == bundle_id && e.title == title)
        {
            return Some(self.entries.remove(pos).workspace);
        }
        if let Some(pos) = self.entries.iter().position(|e| e.bundle_id == bundle_id) {
            return Some(self.entries.remove(pos).workspace);
        }
        None
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(bundle: &str, title: &str, ws: &str) -> SessionEntry {
        SessionEntry {
            bundle_id: bundle.to_string(),
            title: title.to_string(),
            workspace: ws.to_string(),
        }
    }

    #[test]
    fn exact_match_wins_over_bundle_fallback() {
        let mut s = Session {
            entries: vec![
                entry("com.brave.Browser", "Gmail", "1"),
                entry("com.brave.Browser", "GitHub", "3"),
            ],
        };
        assert_eq!(s.take_match("com.brave.Browser", "GitHub").as_deref(), Some("3"));
        // The exact one was consumed; the other remains.
        assert_eq!(s.entries.len(), 1);
        assert_eq!(s.entries[0].title, "Gmail");
    }

    #[test]
    fn title_drift_falls_back_to_bundle_and_consumes() {
        let mut s = Session {
            entries: vec![entry("com.mitchellh.ghostty", "~/proj", "3")],
        };
        // Title changed since last session; bundle-only fallback still routes it.
        assert_eq!(
            s.take_match("com.mitchellh.ghostty", "~/other").as_deref(),
            Some("3")
        );
        assert!(s.is_empty());
    }

    #[test]
    fn duplicate_apps_distribute_across_saved_slots() {
        let mut s = Session {
            entries: vec![
                entry("com.mitchellh.ghostty", "a", "1"),
                entry("com.mitchellh.ghostty", "b", "2"),
            ],
        };
        // Two live windows, neither title matching — each consumes one slot.
        let first = s.take_match("com.mitchellh.ghostty", "x");
        let second = s.take_match("com.mitchellh.ghostty", "y");
        let third = s.take_match("com.mitchellh.ghostty", "z");
        assert_eq!(first.as_deref(), Some("1"));
        assert_eq!(second.as_deref(), Some("2"));
        assert_eq!(third, None);
    }

    #[test]
    fn empty_bundle_never_matches() {
        let mut s = Session {
            entries: vec![entry("", "helper", "1")],
        };
        assert_eq!(s.take_match("", "helper"), None);
    }

    #[test]
    fn no_match_returns_none_and_preserves_entries() {
        let mut s = Session {
            entries: vec![entry("com.apple.Terminal", "shell", "2")],
        };
        assert_eq!(s.take_match("com.unknown.App", "x"), None);
        assert_eq!(s.entries.len(), 1);
    }

    #[test]
    fn roundtrips_through_json() {
        let s = Session {
            entries: vec![
                entry("com.brave.Browser", "Gmail", "1"),
                entry("com.mitchellh.ghostty", "~/proj", "special:scratch"),
            ],
        };
        let json = serde_json::to_vec(&s).unwrap();
        let back: Session = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.entries, s.entries);
    }

    #[test]
    fn load_missing_file_is_empty() {
        let s = Session::load(Path::new("/nonexistent/tarmac/session.json"));
        assert!(s.is_empty());
    }

    #[test]
    fn save_then_load_roundtrips_on_disk() {
        let dir = std::env::temp_dir().join(format!("tarmac-session-test-{}", std::process::id()));
        let path = dir.join("session.json");
        let s = Session {
            entries: vec![entry("com.brave.Browser", "Gmail", "4")],
        };
        s.save(&path).unwrap();
        let back = Session::load(&path);
        assert_eq!(back.entries, s.entries);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
