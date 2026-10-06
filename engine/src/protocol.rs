//! JSON Lines protocol envelope and typed argument bags.
//!
//! Wire contract (one JSON object per line, UTF-8, stdout only):
//!
//! ```text
//! {"id":<id>,"op":"<name>","args":{...}}
//! {"id":<id>,"ok":true,"result":{...}}
//! {"id":<id>,"ok":false,"error":"<message>"}
//! ```
//!
//! All result keys are `snake_case`. Every argument struct is
//! `#[serde(default)]` so a missing field never panics the engine.

use crate::EngineError;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

/// Protocol revision carried by `hello`.
pub const PROTOCOL_VERSION: u32 = 1;
pub const ENGINE_NAME: &str = "diagauthor-engine";
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// SQLite project schema revision implemented by this engine.
pub const SCHEMA_VERSION: i64 = 1;

pub const QUERY_LIMIT_DEFAULT: i64 = 200;
pub const QUERY_LIMIT_MIN: i64 = 1;
pub const QUERY_LIMIT_MAX: i64 = 1000;

/// An incoming request line. `id` may be any JSON value; it is echoed verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub id: Value,
    pub op: String,
    #[serde(default = "empty_object")]
    pub args: Value,
}

/// An outgoing response line. `result` and `error` are mutually exclusive and
/// the absent one is omitted from the JSON object entirely.
#[derive(Debug, Clone, Serialize)]
pub struct Response {
    pub id: Value,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: Value, result: Value) -> Self {
        Self {
            id,
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Value, error: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

/// `project.create` / `project.open` arguments.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct ProjectPathArgs {
    pub path: String,
    /// Only `project.create` uses this: replace an existing non-empty file.
    pub overwrite: bool,
    /// Optional display name; defaults to the file stem.
    pub name: String,
    /// Optional initial ECU name for a freshly created project.
    pub ecu: String,
}

/// `objects.query` arguments.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct ObjectsQueryArgs {
    pub kind: String,
    pub filter: String,
    pub limit: i64,
    pub offset: i64,
}

impl Default for ObjectsQueryArgs {
    fn default() -> Self {
        Self {
            kind: String::new(),
            filter: String::new(),
            limit: QUERY_LIMIT_DEFAULT,
            offset: 0,
        }
    }
}

/// `cdd.preview` arguments.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct CddPreviewArgs {
    pub path: String,
}

/// `project.save` arguments (plan docs/17 4절 "선택 path(다른 이름 저장)").
///
/// `path` is the delivery file and must differ from the open working copy.
/// D-9 keeps the WAL working copy and the `journal_mode=DELETE` delivery
/// snapshot as two different files, and on Windows an open file cannot be
/// renamed over, so in-place self-replacement is refused rather than attempted.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
pub struct ProjectSaveArgs {
    pub path: String,
}

/// Reject an empty or whitespace-only path argument with an actionable message.
pub fn require_path(raw: &str, op: &str) -> Result<std::path::PathBuf, EngineError> {
    if raw.trim().is_empty() {
        return Err(EngineError::InvalidArgs(format!("{op} requires a non-empty 'path'")));
    }
    Ok(std::path::PathBuf::from(raw))
}

/// Deserialize an args bag into a typed struct. A null or missing bag behaves
/// like `{}`, so container-level `#[serde(default)]` fills every field.
pub fn parse_args<T: DeserializeOwned>(args: &Value) -> Result<T, EngineError> {
    let value = if args.is_null() { empty_object() } else { args.clone() };
    serde_json::from_value(value).map_err(|e| EngineError::InvalidArgs(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ok_response_wire_format_is_exact() {
        let line = serde_json::to_string(&Response::ok(json!(7), json!({"a": 1}))).unwrap();
        assert_eq!(line, r#"{"id":7,"ok":true,"result":{"a":1}}"#);
        let parsed: Value = serde_json::from_str(&line).unwrap();
        let keys: Vec<&str> = parsed.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["id", "ok", "result"]);
    }

    #[test]
    fn error_response_wire_format_is_exact() {
        let line = serde_json::to_string(&Response::err(Value::Null, "boom")).unwrap();
        assert_eq!(line, r#"{"id":null,"ok":false,"error":"boom"}"#);
        let parsed: Value = serde_json::from_str(&line).unwrap();
        let keys: Vec<&str> = parsed.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["error", "id", "ok"]);
        assert!(parsed.get("result").is_none());
    }

    #[test]
    fn query_args_defaults_are_applied() {
        let args: ObjectsQueryArgs = parse_args(&json!({"kind": "did"})).unwrap();
        assert_eq!(args.kind, "did");
        assert_eq!(args.filter, "");
        assert_eq!(args.limit, QUERY_LIMIT_DEFAULT);
        assert_eq!(args.offset, 0);
    }

    #[test]
    fn null_args_parse_as_all_defaults() {
        let args: ProjectPathArgs = parse_args(&Value::Null).unwrap();
        assert!(args.path.is_empty());
        assert!(!args.overwrite);
    }

    #[test]
    fn wrong_argument_type_is_rejected() {
        let error = parse_args::<ObjectsQueryArgs>(&json!({"limit": "many"})).unwrap_err();
        assert!(matches!(error, EngineError::InvalidArgs(_)), "{error}");
    }

    #[test]
    fn project_save_args_default_to_omitted_path() {
        let args: ProjectSaveArgs = parse_args(&json!({})).unwrap();
        assert!(args.path.is_empty());
        let args: ProjectSaveArgs = parse_args(&json!({"path": "a.diagproj"})).unwrap();
        assert_eq!(args.path, "a.diagproj");
    }

    /// The delivery path is mandatory and must not be the open working copy, so
    /// the engine rejects a missing path with a usable message rather than
    /// writing somewhere the caller did not ask for.
    #[test]
    fn project_save_requires_a_path() {
        assert!(require_path("", "project.save").is_err());
        assert!(require_path("   ", "project.save").is_err());
        assert!(require_path("out.diagproj", "project.save").is_ok());
    }
}
