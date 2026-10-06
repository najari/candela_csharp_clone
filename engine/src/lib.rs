//! diagauthor engine: single-threaded JSON Lines request dispatcher.
//!
//! The engine is a child process on purpose. The C# GUI owns a `serve` child,
//! writes one request object per line to its stdin and reads one response object
//! per line from its stdout; stderr carries logs only.
//!
//! Concurrency model: there is none, by design. The `serve` loop owns exactly
//! one `Engine` and processes requests strictly in order, so every handler can
//! take `&mut self` without a mutex. Do not add an async runtime or shared
//! lock -- the protocol is synchronous request/response.
//!
//! Module layout follows the plan's D-17 split of the Python reference's single
//! `application.py` class into four responsibilities:
//!
//! | module | responsibility |
//! |---|---|
//! | [`session`] | which project is open, revision, dirty flag, save receipt |
//! | [`command`] | mutating and lifecycle ops (project create/open/close/save/snapshot) |
//! | [`query`] | read-only ops (tree, paged object list) |
//! | [`cdd_apply`] | CDD import application; arrives with M2, exists as a seam now |
//!
//! [`storage`] is the SQLite layer, [`workspace`] the working-copy layout and
//! crash-recovery scan, and [`protocol`] the wire contract; none of the three is
//! a port target because the Python reference had no equivalent split.

pub mod cdd;
pub mod cdd_apply;
pub mod command;
pub mod protocol;
pub mod query;
pub mod session;
pub mod storage;
pub mod workspace;

use protocol::PROTOCOL_VERSION;
use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;
use session::Session;

/// Every way an engine request can fail. Each variant renders as a stable,
/// human-readable message for the GUI's error pane.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("missing or invalid argument: {0}")]
    InvalidArgs(String),
    #[error("unknown op '{0}'")]
    UnknownOp(String),
    #[error("project file already exists: {} (pass \"overwrite\":true to replace it)", .0.display())]
    ProjectExists(std::path::PathBuf),
    #[error("io error at {}: {detail}", path.display())]
    Io { path: std::path::PathBuf, detail: String },
    /// A path that is not SQLite at all, or that fails `PRAGMA quick_check`.
    /// The UI must be able to tell this apart from a foreign-but-valid file
    /// (`NotAProject`), because recovery differs: corruption quarantines the
    /// file, a foreign file is simply not ours.
    #[error("corrupt project at {path}: {detail}")]
    Corrupt { path: String, detail: String },
    /// A valid SQLite file whose `application_id` is not ours.
    #[error("not a DiagAuthor project at {path}: application_id {found:#x}, expected {expected:#x}")]
    NotAProject { path: String, found: i64, expected: i64 },
    /// A project from a newer engine. Never written to (plan 5절).
    #[error("unsupported project schema version: found {found}, this engine reads up to {expected}")]
    UnsupportedSchema { found: i64, expected: i64 },
    /// A `preview_id` that was never issued, or has expired out of the pending
    /// set. The plan's error model has no dedicated code for this; it is a
    /// caller mistake rather than a document problem.
    #[error("unknown or expired preview id '{0}'; run cdd.preview again")]
    UnknownPreview(String),
    /// The source file changed, or disappeared, between `cdd.preview` and
    /// `cdd.apply`. The user approved specific bytes; those are no longer there.
    #[error("source file changed since preview: {path} (expected sha256 {expected})")]
    FileChanged { path: String, expected: String },
    /// A CDD/CDDT file above the hard read size ceiling.
    #[error("CDD file too large: {} is {size} bytes, limit {limit}", path.display())]
    TooLarge { path: PathBuf, size: u64, limit: u64 },
    /// A CDD/CDDT file above the hard total-element ceiling. Bounds total scan
    /// work: a streaming reader keeps O(depth) state, so a shallow but enormous
    /// file could otherwise spin the scanner indefinitely.
    #[error("CDD file has too many elements: {path}, limit {limit}")]
    NodeLimit { path: String, limit: u64 },
    /// A single CDD/CDDT text node above the hard ceiling. Refusing one enormous
    /// node is cheaper and clearer than buffering it.
    #[error("CDD text node too large in {}: {size} bytes, limit {limit}", path.display())]
    TextTooLarge { path: PathBuf, size: u64, limit: u64 },
    /// A CDD/CDDT file nested deeper than the hard element ceiling.
    #[error("CDD element nesting too deep in {}: limit {limit}", path.display())]
    DepthExceeded { path: PathBuf, limit: usize },
    /// Any CDD/CDDT parse or structural problem, with the path it occurred in.
    #[error("CDD parse error at {path}: {detail}")]
    CddParse { path: String, detail: String },
    /// A schema constraint rejected a write. Carried separately so an import can
    /// recognise "this one object is bad" and continue, instead of losing the
    /// whole document.
    #[error("schema constraint violated: {0}")]
    SqliteConstraint(String),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("no project is open; call project.create or project.open first")]
    NoProject,
    /// A feature the engine recognises but does not implement yet. The plan's
    /// error model has `UNSUPPORTED_FEATURE` for exactly this: a known gap is
    /// not the same as a bad request, and must not read as success.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

pub struct Engine {
    session: Session,
    previews: cdd_apply::Previews,
    protocol: u32,
    started: Instant,
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            session: Session::new(),
            previews: cdd_apply::Previews::new(),
            protocol: PROTOCOL_VERSION,
            started: Instant::now(),
        }
    }
}

impl Engine {
    pub fn started(&self) -> Instant {
        self.started
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Dispatch one request object. The `id` is echoed by the serve loop, not
    /// here; handlers only produce the `result` payload.
    pub fn request(&mut self, request: &Value) -> Result<Value, EngineError> {
        let op = request
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| EngineError::InvalidArgs("request is missing the string field 'op'".into()))?;
        let args = request.get("args").cloned().unwrap_or_else(|| serde_json::json!({}));
        match op {
            "hello" => Ok(command::hello(self.protocol)),
            "project.create" => command::project_create(&mut self.session, &args),
            "project.open" => command::project_open(&mut self.session, &args),
            "project.close" => command::project_close(&mut self.session),
            "project.status" => command::project_status(&self.session),
            "project.save" => command::project_save(&mut self.session, &args),
            "project.snapshot" => command::project_snapshot(&self.session),
            "tree.get" => query::tree(&self.session),
            "objects.query" => query::objects(&self.session, &args),
            "cdd.preview" => {
                let path = protocol::require_path(
                    args.get("path").and_then(Value::as_str).unwrap_or_default(),
                    "cdd.preview",
                )?;
                let (preview_id, preview) = self.previews.issue(&path)?;
                let mut result = serde_json::to_value(preview)?;
                // The id is issued alongside the preview so the caller can
                // apply exactly the bytes it was shown.
                if let Some(object) = result.as_object_mut() {
                    object.insert("preview_id".into(), Value::from(preview_id));
                }
                Ok(result)
            }
            "cdd.apply" => cdd_apply::apply(&mut self.session, &mut self.previews, &args),
            "validation.run" => command::validation_run(&self.session),
            // FR-004: present working copies left behind by an abnormal exit so
            // the UI can offer recovery. Read-only, and a missing workspace is
            // an empty list rather than an error.
            "recovery.list" => Ok(serde_json::json!({
                "workspace": workspace::workspace_root().map(|p| p.display().to_string()),
                "candidates": workspace::scan_candidates(),
            })),
            other => Err(EngineError::UnknownOp(other.to_string())),
        }
    }
}

/// Create the file if missing, otherwise open and migrate it. Used by the
/// `migrate` CLI; reports which versions were applied.
pub fn migrate_project(path: &std::path::Path) -> Result<Value, EngineError> {
    if !path.exists() {
        let storage = storage::Storage::create(path, false)?;
        return Ok(serde_json::json!({
            "path": storage.path().display().to_string(),
            "schema_version": protocol::SCHEMA_VERSION,
            "applied": [protocol::SCHEMA_VERSION],
            "created": true,
        }));
    }
    let mut storage = storage::Storage::open(path)?;
    let applied = storage.migrate()?;
    Ok(serde_json::json!({
        "path": storage.path().display().to_string(),
        "schema_version": storage.schema_version()?,
        "applied": applied,
        "created": false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(engine: &mut Engine, value: Value) -> Result<Value, EngineError> {
        engine.request(&value)
    }

    /// A corpus file from the repository `examples/` directory.
    fn examples_file(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("engine/ has a parent")
            .join("examples")
            .join(name)
    }

    #[test]
    fn hello_returns_protocol_one_and_exact_key_set() {
        let mut engine = Engine::default();
        let result = request(&mut engine, json!({"id": 1, "op": "hello", "args": {}})).unwrap();
        let object = result.as_object().unwrap();
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        // Exact plan contract: docs/17 4절 hello result.
        assert_eq!(keys, ["engine_version", "protocol", "schema_version_max"]);
        assert_eq!(result["protocol"], json!(protocol::PROTOCOL_VERSION));
        assert_eq!(
            result["engine_version"],
            json!(format!("{} {}", protocol::ENGINE_NAME, protocol::ENGINE_VERSION))
        );
        assert_eq!(result["schema_version_max"], json!(protocol::SCHEMA_VERSION));
        assert!(engine.started().elapsed().as_secs() < 60);
    }

    /// The client gates project opening on `schema_version_max`, so the field
    /// must be the engine's own readable maximum and not a hardcoded 1.
    #[test]
    fn schema_version_max_tracks_the_engine_schema_revision() {
        assert_eq!(protocol::SCHEMA_VERSION, 1);
        let mut engine = Engine::default();
        let hello = request(&mut engine, json!({"id": 1, "op": "hello", "args": {}})).unwrap();
        assert_eq!(hello["schema_version_max"], json!(protocol::SCHEMA_VERSION));
    }

    #[test]
    fn unknown_op_errors_but_engine_keeps_working() {
        let mut engine = Engine::default();
        let error = request(&mut engine, json!({"id": 1, "op": "does.not.exist", "args": {}}))
            .unwrap_err();
        assert!(matches!(error, EngineError::UnknownOp(ref op) if op == "does.not.exist"), "{error}");
        let hello = request(&mut engine, json!({"id": 2, "op": "hello", "args": {}})).unwrap();
        assert_eq!(hello["protocol"], json!(1));
    }

    #[test]
    fn missing_op_is_invalid_arguments() {
        let mut engine = Engine::default();
        let error = request(&mut engine, json!({"id": 1, "args": {}})).unwrap_err();
        assert!(matches!(error, EngineError::InvalidArgs(_)), "{error}");
    }

    /// A preview id that was never issued, or has expired, must be refused.
    /// Applying an unknown preview would otherwise let a caller invent an import.
    #[test]
    fn cdd_apply_refuses_an_unknown_preview_id() {
        let mut engine = Engine::default();
        let error = request(
            &mut engine,
            json!({"id": 1, "op": "cdd.apply", "args": {"preview_id": "not-issued"}}),
        )
        .unwrap_err();
        assert!(matches!(error, EngineError::UnknownPreview(ref id) if id == "not-issued"), "{error}");

        let missing = request(
            &mut engine,
            json!({"id": 2, "op": "cdd.apply", "args": {"preview_id": "   "}}),
        )
        .unwrap_err();
        assert!(matches!(missing, EngineError::InvalidArgs(_)), "{missing}");
    }

    /// A preview of an unsupported document must never produce a project, even
    /// though the file itself reads without error.
    #[test]
    fn cdd_apply_refuses_an_unsupported_document() {
        let path = examples_file("Synthetic.cdd");
        if !path.is_file() {
            return;
        }
        let mut engine = Engine::default();
        let project = std::env::temp_dir().join("diagauthor-refuse-apply.diagproj");
        let _ = std::fs::remove_file(&project);
        request(
            &mut engine,
            json!({"id": 1, "op": "project.create", "args": {"path": project.to_string_lossy()}}),
        )
        .unwrap();

        let issued = request(
            &mut engine,
            json!({"id": 2, "op": "cdd.preview", "args": {"path": path.to_string_lossy()}}),
        )
        .unwrap();
        let preview_id = issued["preview_id"].as_str().expect("preview_id").to_string();

        let error = request(
            &mut engine,
            json!({"id": 3, "op": "cdd.apply", "args": {"preview_id": preview_id}}),
        )
        .unwrap_err();
        assert!(matches!(error, EngineError::Unsupported(_)), "{error}");

        // The project must be untouched: an ECU row would mean a partial import.
        let tree = request(&mut engine, json!({"id": 4, "op": "tree.get", "args": {}})).unwrap();
        assert_eq!(tree["nodes"], json!([]), "{tree}");
        let _ = std::fs::remove_file(&project);
    }

    /// The dispatcher must reach every op the plan's M1~M3 table names, and must
    /// not silently accept a name it does not implement.
    #[test]
    fn dispatch_covers_every_declared_op_and_rejects_the_rest() {
        let mut engine = Engine::default();
        let declared = [
            "hello",
            "project.create",
            "project.open",
            "project.close",
            "project.status",
            "project.save",
            "project.snapshot",
            "tree.get",
            "objects.query",
            "cdd.preview",
            "cdd.apply",
            "validation.run",
        ];
        for (index, op) in declared.iter().enumerate() {
            // A minimal path is required by the path-taking ops; a refusal for
            // that reason still proves the op is routed rather than unknown.
            let response = request(
                &mut engine,
                json!({"id": index, "op": op, "args": {"path": ""}}),
            );
            match response {
                Ok(_) => {}
                Err(EngineError::UnknownOp(other)) => {
                    panic!("declared op {op} is not routed (fell through to UnknownOp {other})")
                }
                Err(_) => {}
            }
        }
        assert!(matches!(
            request(&mut engine, json!({"id": 99, "op": "project.destroy", "args": {}})),
            Err(EngineError::UnknownOp(_))
        ));
    }
}
