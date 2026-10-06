//! Read-only query ops: the navigation tree and the paged object list.
//!
//! Plan docs/17 3절 (D-17) names `query` as one of the four responsibilities
//! carved out of the Python reference's single `application.py` class. These
//! handlers are the query half. They never write, so they can be called on
//! every UI refresh.
//!
//! Pagination note: the UI review's decision D4 asks for one unpaged list so
//! that shift-range selection, Tab traversal, multi-row entry and the new-number
//! rule all see the whole list. The engine still clamps `limit`
//! ([`protocol::QUERY_LIMIT_MAX`]) because the client is free to page; the shell
//! decides whether to request a page or everything. That decision belongs to the
//! caller, not here.

use serde_json::{Value, json};

use crate::EngineError;
use crate::protocol::{self, ObjectsQueryArgs};
use crate::session::Session;
use crate::storage;

/// `tree.get` — the project tree, or an empty tree when no project is open.
pub fn tree(session: &Session) -> Result<Value, EngineError> {
    match session.storage() {
        Some(storage) => storage.tree(),
        None => Ok(json!({ "nodes": [] })),
    }
}

/// `objects.query` — a filtered, clamped page of one object kind.
pub fn objects(session: &Session, args: &Value) -> Result<Value, EngineError> {
    let args: ObjectsQueryArgs = protocol::parse_args(args)?;
    let kind = args.kind.trim().to_ascii_lowercase();
    storage::validate_kind(&kind)?;
    let limit = args.limit.clamp(protocol::QUERY_LIMIT_MIN, protocol::QUERY_LIMIT_MAX);
    let offset = args.offset.max(0);
    match session.storage() {
        Some(storage) => storage.object_rows(&kind, &args.filter, limit, offset),
        None => Ok(json!({ "rows": [], "total": 0 })),
    }
}
