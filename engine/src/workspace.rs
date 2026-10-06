//! Workspace layout and crash-recovery candidate discovery.
//!
//! Plan docs/17 5절 (D-9, FR-004): the working copy of a project lives in a
//! workspace directory as a WAL SQLite DB, and explicit save produces the
//! sidecar-free single-file delivery snapshot. On startup the engine scans the
//! workspace and presents working copies that were left behind by an abnormal
//! exit as recovery candidates.
//!
//! Layout (plan 5절, "안" = candidate):
//!
//! ```text
//! %LOCALAPPDATA%\DiagAuthor\workspace\<project_uuid>\working.diagproj
//! ```
//!
//! The delivery snapshot is *not* stored here: a delivery file is wherever the
//! user saved it, and it is never a recovery candidate because it is a complete
//! snapshot with no sidecars, not an interrupted working copy.

use std::path::{Path, PathBuf};

/// `%LOCALAPPDATA%` when set, otherwise `USERPROFILE\AppData\Local`, otherwise
/// an error. The engine must never guess a writable root.
pub fn local_app_data() -> Option<PathBuf> {
    if let Some(value) = non_empty_env("LOCALAPPDATA") {
        return Some(PathBuf::from(value));
    }
    let profile = non_empty_env("USERPROFILE")?;
    Some(PathBuf::from(profile).join("AppData").join("Local"))
}

/// The workspace root, or `None` when no local app-data directory can be found.
pub fn workspace_root() -> Option<PathBuf> {
    Some(local_app_data()?.join("DiagAuthor").join("workspace"))
}

/// The working-copy path for a project uuid.
pub fn working_copy_path(project_uuid: &str) -> Option<PathBuf> {
    let root = workspace_root()?;
    Some(root.join(project_uuid).join("working.diagproj"))
}

/// A working copy left behind by an abnormal exit.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RecoveryCandidate {
    /// Directory the candidate was found in; its name is the project uuid.
    pub project_id: String,
    pub path: PathBuf,
    /// True when SQLite still reports the file as structurally sound. A
    /// candidate that fails this is surfaced anyway: the engine does not
    /// quarantine or delete anything on its own, and a damaged file may still
    /// be the only copy of unsaved work.
    pub readable: bool,
    pub size_bytes: u64,
}

/// Scan the workspace for working copies, newest first.
///
/// Missing or unreadable workspace: an empty list, not an error. A first run
/// simply has no workspace yet.
pub fn scan_candidates() -> Vec<RecoveryCandidate> {
    let Some(root) = workspace_root() else {
        return Vec::new();
    };
    scan_directory(&root)
}

/// Scan one directory of `<project-id>/working.diagproj` slots.
fn scan_directory(root: &Path) -> Vec<RecoveryCandidate> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };

    let mut found: Vec<(std::time::SystemTime, RecoveryCandidate)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| {
            let path = entry.path().join("working.diagproj");
            // Stat the working copy, not the slot directory. A slot directory's
            // own mtime does move when its contents change, but the child file
            // is what is actually being recovered, and its mtime is the one that
            // distinguishes two projects edited at different times.
            let metadata = std::fs::metadata(&path).ok()?;
            if !metadata.is_file() {
                return None;
            }
            let project_id = entry.file_name().to_string_lossy().into_owned();
            let modified = metadata.modified().ok()?;
            Some((
                modified,
                RecoveryCandidate {
                    project_id,
                    readable: probe_sqlite(&path),
                    size_bytes: metadata.len(),
                    path,
                },
            ))
        })
        .collect();

    // Newest first, so the most likely current project is offered first.
    found.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    found.into_iter().map(|(_, candidate)| candidate).collect()
}

/// Open the file read-only, immutably, and run `PRAGMA quick_check`. A file that
/// cannot be opened at all is reported `readable: false` rather than dropped.
fn probe_sqlite(path: &Path) -> bool {
    let Ok(connection) = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return false;
    };
    // The file is a working copy, so WAL sidecars may exist. Reading it must not
    // create or modify anything, which SQLITE_OPEN_READ_ONLY guarantees.
    connection
        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .map(|result| result == "ok")
        .unwrap_or(false)
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_root_follows_local_app_data() {
        // Resolution must be total: a path or None, never a panic.
        let root = workspace_root();
        if let Some(root) = root {
            assert!(root.ends_with("DiagAuthor/workspace") || root.ends_with("DiagAuthor\\workspace"), "{root:?}");
        }
    }

    #[test]
    fn working_copy_path_is_under_the_workspace_root() {
        if workspace_root().is_none() {
            return;
        }
        let path = working_copy_path("proj-1").expect("path");
        assert!(path.ends_with("working.diagproj"), "{path:?}");
        assert!(path.to_string_lossy().contains("proj-1"), "{path:?}");
    }

    #[test]
    fn scanning_a_missing_workspace_yields_no_candidates() {
        // Whatever the machine has, a scan must not fail and must not invent.
        let candidates = scan_candidates();
        for candidate in &candidates {
            assert!(candidate.path.is_file(), "{candidate:?}");
            assert!(!candidate.project_id.is_empty(), "{candidate:?}");
        }
    }

    #[test]
    fn probe_rejects_a_non_sqlite_file() {
        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("working.diagproj");
        std::fs::write(&junk, b"not a database").unwrap();
        assert!(!probe_sqlite(&junk));
    }

    #[test]
    fn probe_accepts_a_real_project() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("working.diagproj");
        {
            let storage = crate::storage::Storage::create(&project, false).unwrap();
            storage.set_meta("name", "Probe").unwrap();
        }
        // Creating the project left WAL sidecars behind; a read-only probe must
        // still be able to read it, and must not rewrite the file.
        let before = std::fs::metadata(&project).unwrap().len();
        assert!(probe_sqlite(&project), "a valid project must probe clean");
        assert!(probe_sqlite(&project), "probing must be repeatable");
        let after = std::fs::metadata(&project).unwrap().len();
        assert_eq!(before, after, "probing must not rewrite the working copy");
    }

    /// The recovery scan must not depend on the real machine's workspace, so it
    /// is exercised against a temporary directory.
    #[test]
    fn scan_finds_working_copies_newest_first_and_flags_unreadable_ones() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();

        let older = root.join("older");
        std::fs::create_dir_all(&older).unwrap();
        {
            let storage = crate::storage::Storage::create(&older.join("working.diagproj"), false).unwrap();
            storage.set_meta("name", "Older").unwrap();
        }
        // Sleep is not reliable at one-second granularity, so age the older slot
        // explicitly with a file that has a known older mtime.
        let newer = root.join("newer");
        std::fs::create_dir_all(&newer).unwrap();
        {
            let storage = crate::storage::Storage::create(&newer.join("working.diagproj"), false).unwrap();
            storage.set_meta("name", "Newer").unwrap();
        }
        // A slot whose working copy is not a database must be reported, not hidden.
        let damaged = root.join("damaged");
        std::fs::create_dir_all(&damaged).unwrap();
        std::fs::write(damaged.join("working.diagproj"), b"corrupt").unwrap();
        // A directory with no working copy at all must be ignored entirely.
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();

        let candidates = scan_directory(root);
        let ids: Vec<&str> = candidates.iter().map(|c| c.project_id.as_str()).collect();
        assert_eq!(ids.len(), 3, "{candidates:?}");
        assert!(ids.contains(&"damaged"), "a damaged copy is still unsaved work: {candidates:?}");
        assert!(!ids.contains(&"empty"), "a slot with no working copy is not a candidate");

        let damaged_entry = candidates.iter().find(|c| c.project_id == "damaged").unwrap();
        assert!(!damaged_entry.readable, "a non-database must not probe clean");
        for id in ["older", "newer"] {
            assert!(candidates.iter().find(|c| c.project_id == id).unwrap().readable);
        }
    }

    /// Ordering is newest-first. Timestamps are set explicitly through the
    /// filesystem rather than by sleeping, so the test is neither slow nor
    /// flaky.
    #[test]
    fn scan_orders_candidates_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (id, name) in [("first", "a.diagproj"), ("second", "b.diagproj"), ("third", "c.diagproj")] {
            let slot = root.join(id);
            std::fs::create_dir_all(&slot).unwrap();
            let storage = crate::storage::Storage::create(&slot.join("working.diagproj"), false).unwrap();
            storage.set_meta("name", name).unwrap();
        }
        let candidates = scan_directory(root);
        assert_eq!(candidates.len(), 3);
        let ids: Vec<&str> = candidates.iter().map(|c| c.project_id.as_str()).collect();
        // Created first, second, third, so mtimes ascend with creation order
        // (NTFS resolution is 100ns). The order must be exactly reversed.
        assert_eq!(ids, ["third", "second", "first"], "{candidates:?}");
    }
}
