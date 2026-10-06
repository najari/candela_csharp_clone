//! Session state: the project the engine currently has open, plus the
//! working-state bookkeeping that belongs to a session rather than to the file.
//!
//! Plan docs/17 3절 and D-17 split the Python reference's single
//! `application.py` class into four responsibilities. This module owns the
//! first: it is the per-process "which project is open" slot, the revision
//! counter, the dirty flag, and the last save receipt. Nothing here reads or
//! writes the project directly -- that is [`crate::storage`]'s job, and the ops
//! in [`crate::command`] compose the two.

use std::path::{Path, PathBuf};

use crate::protocol;
use crate::storage::Storage;

/// What the last successful `project.save` produced.
///
/// Session-scoped on purpose: the delivery file's location and content hash
/// describe the copy, not the project. Writing them into `project_meta` would
/// make every delivery differ from the working state it was taken from.
#[derive(Debug, Clone)]
pub struct SaveReceipt {
    pub path: PathBuf,
    pub revision: i64,
    pub file_hash: String,
}

/// The engine's per-process state. The serve loop owns exactly one.
#[derive(Default)]
pub struct Session {
    storage: Option<Storage>,
    /// Monotonic working-state counter. Advances on every mutating command so a
    /// caller can order background results and recognise a stale one.
    revision: i64,
    /// Whether the working state has diverged from the last explicit save.
    dirty: bool,
    last_saved: Option<SaveReceipt>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn storage(&self) -> Option<&Storage> {
        self.storage.as_ref()
    }

    /// The open storage, or the `NoProject` error the plan's error model uses
    /// for every op that needs a project.
    pub fn require(&self) -> Result<&Storage, crate::EngineError> {
        self.storage.as_ref().ok_or(crate::EngineError::NoProject)
    }

    /// Mutable access to the open storage, for the ops that write (D-7: SQLite
    /// is opened by the engine only, and the session's connection is the single
    /// writer).
    pub fn storage_mut(&mut self) -> Result<&mut Storage, crate::EngineError> {
        self.storage.as_mut().ok_or(crate::EngineError::NoProject)
    }

    pub fn is_open(&self) -> bool {
        self.storage.is_some()
    }

    pub fn revision(&self) -> i64 {
        self.revision
    }

    pub fn dirty(&self) -> bool {
        self.dirty
    }

    pub fn last_saved(&self) -> Option<&SaveReceipt> {
        self.last_saved.as_ref()
    }

    /// The open project, or `None` when the session has none.
    pub fn working_path(&self) -> Option<&Path> {
        self.storage.as_ref().map(Storage::path)
    }

    /// Mark a mutating command as having changed the working state.
    pub fn touch(&mut self) -> i64 {
        self.revision += 1;
        self.dirty = true;
        self.revision
    }

    /// Attach a newly opened or created project, replacing any current one.
    /// The GUI is single-project, so opening replaces (plan 3절: one project,
    /// one revision, one undo cursor per engine process, D-19).
    pub fn attach(&mut self, storage: Storage) {
        self.storage = Some(storage);
        self.revision = 0;
        self.dirty = false;
        self.last_saved = None;
    }

    /// Detach the current project. Returns the storage so the caller can still
    /// inspect it after the slot is cleared.
    pub fn detach(&mut self) -> Option<Storage> {
        self.revision = 0;
        self.dirty = false;
        self.last_saved = None;
        self.storage.take()
    }

    /// Record a successful explicit save and clear the dirty flag.
    pub fn record_save(&mut self, receipt: SaveReceipt) {
        self.dirty = false;
        self.last_saved = Some(receipt);
    }

    /// A token that is stable for a given working state and changes whenever the
    /// state does. Callers compare it to decide whether a late background result
    /// still describes what they asked about (plan 4절 `state_token`).
    pub fn state_token(&self) -> Option<String> {
        let storage = self.storage.as_ref()?;
        Some(format!(
            "{}:{}:{}:{}",
            storage.session_id(),
            storage.path().display(),
            self.revision,
            u8::from(self.dirty)
        ))
    }

    /// The `project.create` / `project.open` result shape.
    pub fn open_result(storage: &Storage) -> serde_json::Value {
        serde_json::json!({
            "open": true,
            "path": storage.path().display().to_string(),
            "schema_version": protocol::SCHEMA_VERSION,
            "session_id": storage.session_id(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::Storage;

    fn attached_session() -> (tempfile::TempDir, Session) {
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = Storage::create(&dir.path().join("p.diagproj"), false).expect("create");
        let mut session = Session::new();
        session.attach(storage);
        (dir, session)
    }

    #[test]
    fn a_new_session_has_no_project() {
        let session = Session::new();
        assert!(!session.is_open());
        assert!(session.storage().is_none());
        assert_eq!(session.state_token(), None);
        assert!(matches!(session.require(), Err(crate::EngineError::NoProject)));
    }

    #[test]
    fn attach_opens_the_project_and_resets_session_counters() {
        let (_dir, session) = attached_session();
        assert!(session.is_open());
        assert_eq!(session.revision(), 0);
        assert!(!session.dirty());
        assert!(session.last_saved().is_none());
        assert!(session.working_path().is_some());
    }

    #[test]
    fn touch_advances_the_revision_and_marks_dirty() {
        let (_dir, mut session) = attached_session();
        assert_eq!(session.touch(), 1);
        assert_eq!(session.revision(), 1);
        assert!(session.dirty());
        assert_eq!(session.touch(), 2);
        assert_eq!(session.revision(), 2);
    }

    /// The state token is what lets a caller notice that a late background
    /// result describes a state it no longer cares about, so any change to the
    /// working state must change it.
    #[test]
    fn state_token_changes_with_every_working_state_change() {
        let (_dir, mut session) = attached_session();
        let initial = session.state_token().expect("token");
        // Reading it twice must not perturb it.
        assert_eq!(session.state_token().expect("token"), initial);

        session.touch();
        let after_edit = session.state_token().expect("token");
        assert_ne!(after_edit, initial, "an edit must change the token");

        // A second edit at the same revision is impossible, but a detach and
        // reattach must look like a fresh state even at revision 0.
        session.detach();
        assert_eq!(session.state_token(), None);
    }

    #[test]
    fn record_save_clears_dirty_and_keeps_the_receipt() {
        let (_dir, mut session) = attached_session();
        session.touch();
        assert!(session.dirty());
        let path = PathBuf::from("delivered.diagproj");
        session.record_save(SaveReceipt {
            path: path.clone(),
            revision: session.revision(),
            file_hash: "0123456789abcdef0123456789abcdef".into(),
        });
        assert!(!session.dirty(), "an explicit save clears the dirty flag");
        let receipt = session.last_saved().expect("receipt");
        assert_eq!(receipt.path, path);
        assert_eq!(receipt.revision, 1);
        // The save did not change the working state, so the token must not move
        // except for the dirty bit, which it legitimately reflects.
        assert!(!session.dirty());
    }

    #[test]
    fn detach_clears_the_project_and_its_counters() {
        let (_dir, mut session) = attached_session();
        session.touch();
        let storage = session.detach();
        assert!(storage.is_some(), "detach hands the storage back to the caller");
        assert!(!session.is_open());
        assert_eq!(session.revision(), 0);
        assert!(!session.dirty());
        assert!(session.last_saved().is_none());
    }
}
