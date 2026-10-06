//! Mutating and lifecycle commands: project create/open/close/save/status/
//! snapshot, plus the CDD preview entry point.
//!
//! Plan docs/17 3절 (D-17) names `command` as one of the four responsibilities
//! split out of the Python reference's single `application.py` class. The
//! dispatcher in [`crate::lib`] owns routing; the handlers here own semantics.
//!
//! Every handler takes `&mut Session` only for the ones that change session
//! state, and `&Session` for the read-only ones. The engine is single-threaded
//! by design (D-3), so no locking appears anywhere in this module.

use std::path::Path;

use serde_json::{Value, json};

use crate::EngineError;
use crate::protocol::{self, CddPreviewArgs, ProjectPathArgs, ProjectSaveArgs};
use crate::session::{SaveReceipt, Session};
use crate::storage::Storage;
use crate::{cdd, storage::hash_file};

/// `hello` — the protocol handshake the client gates on.
///
/// Key set is the plan contract (docs/17 4절, PORT-RISK-05):
///   * `protocol` -- envelope revision, integer
///   * `engine_version` -- "<engine name> <semver>", human diagnostic value
///   * `schema_version_max` -- highest project `schema_version` this engine can
///     open. A file above this must be rejected with `UNSUPPORTED_VERSION`, so
///     the client gates on this and never on the engine's current version.
pub fn hello(protocol_version: u32) -> Value {
    json!({
        "protocol": protocol_version,
        "engine_version": format!("{} {}", protocol::ENGINE_NAME, protocol::ENGINE_VERSION),
        "schema_version_max": protocol::SCHEMA_VERSION,
    })
}

pub fn project_create(session: &mut Session, args: &Value) -> Result<Value, EngineError> {
    let args: ProjectPathArgs = protocol::parse_args(args)?;
    let path = protocol::require_path(&args.path, "project.create")?;
    let mut storage = Storage::create(&path, args.overwrite)?;
    let name = if args.name.trim().is_empty() {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        args.name.clone()
    };
    storage.set_meta("name", &name)?;
    if !args.ecu.trim().is_empty() {
        storage.set_primary_ecu(args.ecu.trim())?;
    }
    let result = Session::open_result(&storage);
    session.attach(storage);
    Ok(result)
}

pub fn project_open(session: &mut Session, args: &Value) -> Result<Value, EngineError> {
    let args: ProjectPathArgs = protocol::parse_args(args)?;
    let path = protocol::require_path(&args.path, "project.open")?;
    let storage = Storage::open(&path)?;
    let result = Session::open_result(&storage);
    // Opening replaces any currently open project; the GUI is single-project.
    session.attach(storage);
    Ok(result)
}

pub fn project_close(session: &mut Session) -> Result<Value, EngineError> {
    session.detach();
    Ok(json!({ "open": false }))
}

pub fn project_status(session: &Session) -> Result<Value, EngineError> {
    let Some(storage) = session.storage() else {
        return Ok(json!({
            "open": false,
            "path": Value::Null,
            "ecu": "",
            "schema_version": protocol::SCHEMA_VERSION,
        }));
    };
    Ok(json!({
        "open": true,
        "path": storage.path().display().to_string(),
        "ecu": storage.primary_ecu()?.unwrap_or_default(),
        "schema_version": protocol::SCHEMA_VERSION,
    }))
}

/// `project.save` -- publish a standalone single-file delivery snapshot.
///
/// Plan docs/17 4절 puts this at M1. Semantics per 5절 (D-9):
///   * `path` is the delivery file and is required
///   * it must differ from the open working copy: on Windows the working file
///     is locked by this connection, so renaming a snapshot over it fails with a
///     sharing violation. D-9 already keeps the WAL working copy and the
///     DELETE-mode delivery file distinct, so in-place self-replacement is
///     refused instead of attempted.
///   * the delivered file has no `-wal`/`-shm` sidecars and is
///     integrity-checked before the atomic rename
///   * the working copy stays open and keeps its WAL state; a save is a delivery
///     copy, not a move
pub fn project_save(session: &mut Session, args: &Value) -> Result<Value, EngineError> {
    let args: ProjectSaveArgs = protocol::parse_args(args)?;
    let storage = session.require()?;
    let destination = protocol::require_path(&args.path, "project.save")?;
    let working = storage.path().to_path_buf();
    if same_file(&destination, &working) {
        return Err(EngineError::InvalidArgs(format!(
            "project.save cannot write over the open working copy ({}); pass a different delivery path",
            working.display()
        )));
    }
    let revision = session.revision();
    let state_token = session
        .state_token()
        .ok_or(EngineError::NoProject)?;

    storage.save_as(&destination)?;

    let file_hash = hash_file(&destination)?;
    session.record_save(SaveReceipt {
        path: destination.clone(),
        revision,
        file_hash: file_hash.clone(),
    });

    Ok(json!({
        "path": destination.display().to_string(),
        "working_path": working.display().to_string(),
        "revision": revision,
        "state_token": state_token,
        "file_hash": file_hash,
        "dirty": session.dirty(),
    }))
}

/// `project.snapshot` -- read-only liveness and state descriptor.
///
/// Plan docs/17 4절 puts this at M1. It never writes, so it is safe to call on
/// every UI refresh. It errors when no project is open, which is how a caller
/// distinguishes "no project" from "an empty project".
pub fn project_snapshot(session: &Session) -> Result<Value, EngineError> {
    let storage = session.require()?;
    Ok(json!({
        "session_id": storage.session_id(),
        "path": storage.path().display().to_string(),
        "ecu": storage.primary_ecu()?.unwrap_or_default(),
        "revision": session.revision(),
        "schema_version": storage.schema_version()?,
        "schema_version_max": protocol::SCHEMA_VERSION,
        "dirty": session.dirty(),
        "state_token": session.state_token().unwrap_or_default(),
        "last_saved": match session.last_saved() {
            Some(receipt) => json!({
                "path": receipt.path.display().to_string(),
                "revision": receipt.revision,
                "file_hash": receipt.file_hash,
            }),
            None => Value::Null,
        },
    }))
}

/// `cdd.preview` without id minting, for callers that only want to inspect a
/// file. The dispatcher uses [`crate::cdd_apply::Previews::issue`] instead,
/// because that also holds the bytes an apply would need.
pub fn cdd_preview(args: &Value) -> Result<Value, EngineError> {
    let args: CddPreviewArgs = protocol::parse_args(args)?;
    let path = protocol::require_path(&args.path, "cdd.preview")?;
    let preview = cdd::preview(&path)?;
    Ok(serde_json::to_value(preview)?)
}

/// `validation.run` -- M1 skeleton.
///
/// The result shape is final: `result.issues` is the list the plan's error model
/// requires, and unsupported or malformed import findings are returned as data
/// inside a successful response rather than as exceptions. The rule set itself
/// arrives with M5.
pub fn validation_run(_session: &Session) -> Result<Value, EngineError> {
    Ok(json!({ "issues": [] }))
}

/// Compare two paths for identity, tolerating a non-existent destination (which
/// only needs the lexical form) and an existing one (compared by canonical path,
/// so a differently spelled but identical path is still recognised).
fn same_file(destination: &Path, working: &Path) -> bool {
    if destination == working {
        return true;
    }
    match (destination.canonicalize(), working.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
