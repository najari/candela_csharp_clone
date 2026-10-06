//! End-to-end protocol tests.
//!
//! Every test spawns the real `diagauthor-engine serve` child process and talks
//! to it exactly like the C# GUI does: one JSON object per line on stdin, one
//! JSON object per line on stdout.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    fn start() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_diagauthor-engine"))
            .arg("serve")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn diagauthor-engine serve");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn send_raw(&mut self, line: &str) -> Value {
        writeln!(self.stdin, "{line}").expect("write request line");
        self.stdin.flush().expect("flush request line");
        self.read_response()
    }

    fn send(&mut self, request: Value) -> Value {
        self.send_raw(&request.to_string())
    }

    /// Send a request and assert the envelope reports success; return `result`.
    fn call(&mut self, request: Value) -> Value {
        let response = self.send(request);
        assert_eq!(response["ok"], json!(true), "request failed: {response}");
        response["result"].clone()
    }

    fn read_response(&mut self) -> Value {
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line).expect("read response line");
        assert!(read > 0, "engine closed stdout unexpectedly");
        serde_json::from_str(line.trim_end()).unwrap_or_else(|e| panic!("bad response line {line:?}: {e}"))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn examples_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("examples")
        .join(relative)
}

#[test]
fn hello_has_protocol_one_and_exact_key_set() {
    let mut session = Session::start();
    let response = session.send(json!({"id": 1, "op": "hello", "args": {}}));
    assert_eq!(response["id"], json!(1));
    assert_eq!(response["ok"], json!(true));
    assert!(response.get("error").is_none(), "{response}");
    let result = response["result"].as_object().expect("hello result object");
    let mut keys: Vec<&str> = result.keys().map(String::as_str).collect();
    keys.sort_unstable();
    // Exact plan contract (docs/17 4절 hello result).
    assert_eq!(keys, ["engine_version", "protocol", "schema_version_max"]);
    assert_eq!(result["protocol"], json!(1));
    assert_eq!(
        result["engine_version"],
        json!(format!("diagauthor-engine {}", env!("CARGO_PKG_VERSION")))
    );
    assert_eq!(result["schema_version_max"], json!(1));
}

#[test]
fn unknown_op_is_an_error_and_server_survives() {
    let mut session = Session::start();
    let response = session.send(json!({"id": "abc", "op": "does.not.exist", "args": {}}));
    assert_eq!(response["id"], json!("abc"));
    assert_eq!(response["ok"], json!(false));
    assert!(response.get("result").is_none(), "{response}");
    let error = response["error"].as_str().expect("error string");
    assert!(error.contains("does.not.exist"), "{error}");
    let hello = session.call(json!({"id": 2, "op": "hello", "args": {}}));
    assert_eq!(hello["protocol"], json!(1));
}

#[test]
fn malformed_json_reports_null_id_and_server_survives() {
    let mut session = Session::start();
    let response = session.send_raw("{this is not json");
    assert_eq!(response["id"], Value::Null);
    assert_eq!(response["ok"], json!(false));
    let error = response["error"].as_str().expect("error string");
    assert!(error.starts_with("Invalid JSON:"), "{error}");
    let hello = session.call(json!({"id": 3, "op": "hello", "args": {}}));
    assert_eq!(
        hello["engine_version"].as_str().unwrap().split(' ').next(),
        Some("diagauthor-engine")
    );
}

#[test]
fn project_create_status_close_reopen_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("demo.diagproj");
    let path_text = path.to_string_lossy().into_owned();
    let mut session = Session::start();

    let created = session.call(
        json!({"id": 1, "op": "project.create", "args": {"path": path_text, "ecu": "SynthECU"}}),
    );
    assert_eq!(created["open"], json!(true));
    assert_eq!(created["path"], json!(path_text));
    assert_eq!(created["schema_version"], json!(1));
    let session_id = created["session_id"].as_str().expect("session_id");
    uuid::Uuid::parse_str(session_id).expect("session_id is a UUID");
    assert!(path.is_file(), "project.create must write the file");

    let status = session.call(json!({"id": 2, "op": "project.status", "args": {}}));
    assert_eq!(status["open"], json!(true));
    assert_eq!(status["ecu"], json!("SynthECU"));
    assert_eq!(status["schema_version"], json!(1));

    let closed = session.call(json!({"id": 3, "op": "project.close", "args": {}}));
    assert_eq!(closed["open"], json!(false));

    let reopened = session.call(json!({"id": 4, "op": "project.open", "args": {"path": path_text}}));
    let mut reopened_keys: Vec<&str> = reopened.as_object().unwrap().keys().map(String::as_str).collect();
    reopened_keys.sort_unstable();
    assert_eq!(reopened_keys, ["open", "path", "schema_version", "session_id"]);
    assert_eq!(reopened["open"], json!(true));
    assert_eq!(reopened["path"], json!(path_text));
    assert_ne!(reopened["session_id"], created["session_id"]);
    assert_eq!(reopened["schema_version"], json!(1));
    let status_after_reopen = session.call(json!({"id": 5, "op": "project.status", "args": {}}));
    assert_eq!(status_after_reopen["ecu"], json!("SynthECU"));
}

#[test]
fn project_snapshot_is_read_only_and_needs_a_project() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("snap.diagproj");
    let mut session = Session::start();

    // No project open: both M1 ops must refuse rather than invent a state.
    let no_project = session.send(json!({"id": 1, "op": "project.snapshot", "args": {}}));
    assert_eq!(no_project["ok"], json!(false));
    assert!(no_project["error"].as_str().unwrap().contains("no project"));

    let no_save = session.send(json!({"id": 2, "op": "project.save", "args": {}}));
    assert_eq!(no_save["ok"], json!(false));
    assert!(no_save["error"].as_str().unwrap().contains("no project"));

    session.call(json!({"id": 3, "op": "project.create", "args": {"path": project.to_string_lossy(), "ecu": "SnapECU"}}));

    let first = session.call(json!({"id": 4, "op": "project.snapshot", "args": {}}));
    let mut keys: Vec<&str> = first.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "dirty",
            "ecu",
            "last_saved",
            "path",
            "revision",
            "schema_version",
            "schema_version_max",
            "session_id",
            "state_token",
        ]
    );
    assert_eq!(first["ecu"], json!("SnapECU"));
    assert_eq!(first["revision"], json!(0));
    assert_eq!(first["dirty"], json!(false));
    assert_eq!(first["schema_version_max"], json!(1));
    // Nothing has been saved yet in this session.
    assert_eq!(first["last_saved"], Value::Null);

    // Reading it twice must not mutate anything: same token, same revision.
    let second = session.call(json!({"id": 5, "op": "project.snapshot", "args": {}}));
    assert_eq!(second["state_token"], first["state_token"]);
    assert_eq!(second["revision"], first["revision"]);
    assert_eq!(second["dirty"], first["dirty"]);
}

#[test]
fn project_save_writes_a_sidecar_free_snapshot_and_clears_dirty() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("working.diagproj");
    let delivery = dir.path().join("delivered.diagproj");
    let mut session = Session::start();
    session.call(json!({"id": 1, "op": "project.create", "args": {"path": project.to_string_lossy(), "ecu": "SaveECU"}}));

    let receipt = session.call(
        json!({"id": 2, "op": "project.save", "args": {"path": delivery.to_string_lossy()}}),
    );
    assert_eq!(receipt["path"], json!(delivery.to_string_lossy()));
    assert_eq!(receipt["working_path"], json!(project.to_string_lossy()));
    assert_eq!(receipt["dirty"], json!(false));
    assert_eq!(receipt["revision"], json!(0));
    let file_hash = receipt["file_hash"].as_str().expect("file_hash");
    assert_eq!(file_hash.len(), 32, "file_hash is 128-bit hex: {file_hash}");
    assert!(file_hash.chars().all(|c| c.is_ascii_hexdigit()), "{file_hash}");

    // A delivered file is a standalone single file: no -wal, no -shm.
    assert!(delivery.is_file(), "save must write the delivery file");
    assert!(!dir.path().join("delivered.diagproj-wal").exists(), "no -wal sidecar");
    assert!(!dir.path().join("delivered.diagproj-shm").exists(), "no -shm sidecar");

    // The delivered file must be a valid project on its own: close and open it.
    session.call(json!({"id": 3, "op": "project.close", "args": {}}));
    let reopened = session.send(
        json!({"id": 4, "op": "project.open", "args": {"path": delivery.to_string_lossy()}}),
    );
    assert_eq!(
        reopened["ok"], json!(true),
        "delivered snapshot must open cleanly: {reopened}"
    );
    let reopened = reopened["result"].clone();
    assert_eq!(reopened["open"], json!(true));
    let status = session.call(json!({"id": 5, "op": "project.status", "args": {}}));
    assert_eq!(status["ecu"], json!("SaveECU"), "content must survive the snapshot");
}

/// D-9 keeps the working copy and the delivery file separate. Writing the
/// delivery over the open working copy is refused with an actionable message
/// rather than attempted, because Windows locks the open file.
#[test]
fn project_save_refuses_to_write_over_the_open_working_copy() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("working.diagproj");
    let project_text = project.to_string_lossy().into_owned();
    let mut session = Session::start();
    session.call(json!({"id": 1, "op": "project.create", "args": {"path": project_text, "ecu": "SelfECU"}}));

    let in_place = session.send(json!({"id": 2, "op": "project.save", "args": {"path": project_text}}));
    assert_eq!(in_place["ok"], json!(false), "in-place save must be refused");
    let message = in_place["error"].as_str().expect("error string");
    assert!(message.contains("working copy"), "{message}");

    // A missing path is refused with the same actionable shape.
    let missing = session.send(json!({"id": 3, "op": "project.save", "args": {}}));
    assert_eq!(missing["ok"], json!(false));
    assert!(missing["error"].as_str().unwrap().contains("project.save requires"));

    // The project must be untouched and still usable.
    let status = session.call(json!({"id": 4, "op": "project.status", "args": {}}));
    assert_eq!(status["open"], json!(true));
    assert_eq!(status["ecu"], json!("SelfECU"));
}

#[test]
fn project_save_delivers_a_copy_and_keeps_the_working_copy_open() {
    let dir = tempfile::tempdir().unwrap();
    let working = dir.path().join("working.diagproj");
    let delivery = dir.path().join("delivered.diagproj");
    let mut session = Session::start();
    session.call(json!({"id": 1, "op": "project.create", "args": {"path": working.to_string_lossy(), "ecu": "AsECU"}}));

    let receipt = session.call(
        json!({"id": 2, "op": "project.save", "args": {"path": delivery.to_string_lossy()}}),
    );
    assert!(delivery.is_file(), "save must write the delivery file");

    // The session now remembers the delivery without writing it into the
    // project, so the delivered bytes stay a faithful copy of the state.
    let snapshot = session.call(json!({"id": 3, "op": "project.snapshot", "args": {}}));
    assert_eq!(snapshot["last_saved"]["path"], json!(delivery.to_string_lossy()));
    assert_eq!(snapshot["last_saved"]["file_hash"], receipt["file_hash"]);
    assert_eq!(snapshot["last_saved"]["revision"], json!(0));

    // The delivery is a copy, not a move: the session keeps the working copy.
    let status = session.call(json!({"id": 4, "op": "project.status", "args": {}}));
    assert_eq!(status["open"], json!(true));
    assert_eq!(status["path"], json!(working.to_string_lossy()));
    assert_eq!(status["ecu"], json!("AsECU"));
    assert!(working.is_file(), "working copy must still exist");

    // Re-delivering the same logical state yields the same content hash. Save
    // bookkeeping is session-scoped, so an unchanged project produces identical
    // bytes; this would drift if the receipt were persisted into the project.
    let again = session.send(
        json!({"id": 5, "op": "project.save", "args": {"path": delivery.to_string_lossy()}}),
    );
    assert_eq!(again["ok"], json!(true), "{again}");
    let again = again["result"].clone();
    assert_eq!(
        again["file_hash"], receipt["file_hash"],
        "same working state must deliver the same content hash"
    );
}

#[test]
fn save_failure_keeps_the_working_copy_and_reports_the_error() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("keep.diagproj");
    let project_text = project.to_string_lossy().into_owned();
    let mut session = Session::start();
    session.call(json!({"id": 1, "op": "project.create", "args": {"path": project_text, "ecu": "KeepECU"}}));

    // A directory that does not exist cannot receive the temp file.
    let unreachable = dir.path().join("no-such-dir").join("out.diagproj");
    let failed = session.send(
        json!({"id": 2, "op": "project.save", "args": {"path": unreachable.to_string_lossy()}}),
    );
    assert_eq!(failed["ok"], json!(false), "save to a missing directory must fail");
    assert!(!unreachable.exists());

    // The project must survive the failed save and still be open and usable.
    let status = session.call(json!({"id": 3, "op": "project.status", "args": {}}));
    assert_eq!(status["open"], json!(true));
    assert_eq!(status["ecu"], json!("KeepECU"));
    let snapshot = session.call(json!({"id": 4, "op": "project.snapshot", "args": {}}));
    assert_eq!(snapshot["path"], json!(project_text), "working copy is unchanged");
    assert_eq!(snapshot["dirty"], json!(false));

    // No temp file may be left behind in the working directory.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".diagauthor-save-"))
        .collect();
    assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
}

#[test]
fn reopening_validates_application_id_and_distinguishes_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("good.diagproj");
    let project_text = project.to_string_lossy().into_owned();
    let mut session = Session::start();
    session.call(json!({"id": 1, "op": "project.create", "args": {"path": project_text}}));
    session.call(json!({"id": 2, "op": "project.close", "args": {}}));

    let reopened = session.call(json!({"id": 3, "op": "project.open", "args": {"path": project_text}}));
    assert_eq!(reopened["open"], json!(true));
    session.call(json!({"id": 4, "op": "project.close", "args": {}}));

    let text_file = dir.path().join("not-a-project.diagproj");
    std::fs::write(&text_file, b"this is a text file, not sqlite").unwrap();
    let corrupt = session.send(json!({"id": 5, "op": "project.open", "args": {"path": text_file.to_string_lossy()}}));
    assert_eq!(corrupt["ok"], json!(false));
    let corrupt_error = corrupt["error"].as_str().expect("error string");
    assert!(
        corrupt_error.contains("corrupt") || corrupt_error.contains("not a SQLite database"),
        "{corrupt_error}"
    );

    let alien = dir.path().join("alien.diagproj");
    {
        let connection = rusqlite::Connection::open(&alien).unwrap();
        connection
            .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
            .unwrap();
    }
    let alien_response = session.send(json!({"id": 6, "op": "project.open", "args": {"path": alien.to_string_lossy()}}));
    assert_eq!(alien_response["ok"], json!(false));
    let alien_error = alien_response["error"].as_str().expect("error string");
    assert!(alien_error.contains("not a DiagAuthor project"), "{alien_error}");
    assert!(alien_error.contains("application_id"), "{alien_error}");
    assert_ne!(alien_error, corrupt_error, "the two failure modes must be distinguishable");

    let hello = session.call(json!({"id": 7, "op": "hello", "args": {}}));
    assert_eq!(hello["protocol"], json!(1));
}

#[test]
fn old_schema_is_migrated_and_future_schema_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::start();

    let legacy = dir.path().join("legacy.diagproj");
    {
        let connection = rusqlite::Connection::open(&legacy).unwrap();
        connection.pragma_update(None, "application_id", 0x4449_4147i64).unwrap();
    }
    let migrated = session.call(json!({"id": 1, "op": "project.open", "args": {"path": legacy.to_string_lossy()}}));
    assert_eq!(migrated["schema_version"], json!(1));
    let status = session.call(json!({"id": 2, "op": "project.status", "args": {}}));
    assert_eq!(status["open"], json!(true));
    let tree = session.call(json!({"id": 3, "op": "tree.get", "args": {}}));
    assert!(tree["nodes"].is_array(), "{tree}");
    session.call(json!({"id": 4, "op": "project.close", "args": {}}));

    let future = dir.path().join("future.diagproj");
    {
        let connection = rusqlite::Connection::open(&future).unwrap();
        connection.pragma_update(None, "application_id", 0x4449_4147i64).unwrap();
        connection.pragma_update(None, "user_version", 99i64).unwrap();
        connection.execute_batch("CREATE TABLE placeholder(x);").unwrap();
    }
    let rejected = session.send(json!({"id": 5, "op": "project.open", "args": {"path": future.to_string_lossy()}}));
    assert_eq!(rejected["ok"], json!(false));
    let error = rejected["error"].as_str().expect("error string");
    assert!(error.contains("99"), "{error}");
}

#[test]
fn tree_and_validation_shapes_without_a_project() {
    let mut session = Session::start();
    let tree = session.call(json!({"id": 1, "op": "tree.get", "args": {}}));
    assert_eq!(tree["nodes"], json!([]));
    let validation = session.call(json!({"id": 2, "op": "validation.run", "args": {}}));
    assert_eq!(validation["issues"], json!([]));
    let query = session.call(json!({"id": 3, "op": "objects.query", "args": {"kind": "did"}}));
    assert_eq!(query["rows"], json!([]));
    assert_eq!(query["total"], json!(0));
    let unknown = session.send(json!({"id": 4, "op": "objects.query", "args": {"kind": "wombat"}}));
    assert_eq!(unknown["ok"], json!(false));
    let hello = session.call(json!({"id": 5, "op": "hello", "args": {}}));
    assert_eq!(hello["protocol"], json!(1));
}

#[test]
fn objects_query_clamps_and_filters_and_tree_exposes_data() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("query.diagproj");
    let path_text = path.to_string_lossy().into_owned();
    let mut session = Session::start();
    session.call(json!({"id": 1, "op": "project.create", "args": {"path": path_text, "ecu": "QueryECU"}}));
    session.call(json!({"id": 2, "op": "project.close", "args": {}}));
    {
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        let ecu_id: i64 = connection
            .query_row("SELECT id FROM ecus LIMIT 1", [], |row| row.get(0))
            .unwrap();
        connection
            .execute_batch(&format!(
                "INSERT INTO variants(ecu_id, name, qualifier) VALUES ({ecu_id}, 'Variant_A', 'Variant_A');
                 INSERT INTO dids(ecu_id, name, identifier) VALUES ({ecu_id}, 'Alpha_DID', 61840);
                 INSERT INTO dids(ecu_id, name, identifier) VALUES ({ecu_id}, 'beta_did', 61841);
                 INSERT INTO dids(ecu_id, name, identifier) VALUES ({ecu_id}, 'Gamma', 61842);
                 INSERT INTO variant_dids(variant_id, did_id) VALUES ((SELECT id FROM variants LIMIT 1), (SELECT id FROM dids WHERE name='Alpha_DID'));
                 INSERT INTO diagnostic_classes(ecu_id, name) VALUES ({ecu_id}, 'DiagClass');
                 INSERT INTO diagnostic_instances(ecu_id, class_id, name) VALUES ({ecu_id}, (SELECT id FROM diagnostic_classes LIMIT 1), 'Instance_1');
                 INSERT INTO diagnostic_instances(ecu_id, class_id, name) VALUES ({ecu_id}, (SELECT id FROM diagnostic_classes LIMIT 1), 'Instance_2');"
            ))
            .unwrap();
    }
    session.call(json!({"id": 3, "op": "project.open", "args": {"path": path_text}}));

    let all = session.call(json!({"id": 4, "op": "objects.query", "args": {"kind": "did", "limit": 5000, "offset": 0}}));
    assert_eq!(all["total"], json!(3));
    assert_eq!(all["rows"].as_array().unwrap().len(), 3);
    let first = &all["rows"][0];
    assert_eq!(first["kind"], json!("did"));
    for key in ["id", "kind", "name", "identifier", "identifier_hex"] {
        assert!(first.get(key).is_some(), "row missing {key}: {first}");
    }
    // The identifier is the integer the document declared; the hex form is
    // derived for display, so a client can render without redoing the format.
    let identifier = first["identifier"].as_i64().expect("identifier is a number");
    assert_eq!(identifier, 61840);
    assert_eq!(first["identifier_hex"], json!("0xF190"));

    let limited = session.call(json!({"id": 5, "op": "objects.query", "args": {"kind": "did", "limit": 2, "offset": 0}}));
    assert_eq!(limited["rows"].as_array().unwrap().len(), 2);
    assert_eq!(limited["total"], json!(3));
    let floor = session.call(json!({"id": 6, "op": "objects.query", "args": {"kind": "did", "limit": 0, "offset": 0}}));
    assert_eq!(floor["rows"].as_array().unwrap().len(), 1);
    let negative_offset = session.call(json!({"id": 7, "op": "objects.query", "args": {"kind": "did", "limit": 100, "offset": -5}}));
    assert_eq!(negative_offset["rows"].as_array().unwrap().len(), 3);

    let filtered = session.call(json!({"id": 8, "op": "objects.query", "args": {"kind": "did", "filter": "ALPHA"}}));
    assert_eq!(filtered["total"], json!(1));
    assert_eq!(filtered["rows"][0]["name"], json!("Alpha_DID"));
    // The filter sees the identifier as text, so a user can type either the
    // decimal or the hex form and both find the DID.
    let by_decimal = session.call(json!({"id": 9, "op": "objects.query", "args": {"kind": "did", "filter": "61841"}}));
    assert_eq!(by_decimal["total"], json!(1));
    assert_eq!(by_decimal["rows"][0]["name"], json!("beta_did"));
    let by_hex = session.call(json!({"id": 11, "op": "objects.query", "args": {"kind": "did", "filter": "f191"}}));
    assert_eq!(by_hex["total"], json!(1));
    assert_eq!(by_hex["rows"][0]["name"], json!("beta_did"));

    let tree = session.call(json!({"id": 10, "op": "tree.get", "args": {}}));
    let ecu = &tree["nodes"][0];
    assert_eq!(ecu["kind"], json!("ecu"));
    assert_eq!(ecu["label"], json!("QueryECU"));
    assert_eq!(ecu["child_count"], json!(4));
    assert_eq!(ecu["children"][0]["kind"], json!("variant"));
    assert_eq!(ecu["children"][0]["child_count"], json!(1));
    assert_eq!(ecu["children"][0]["children"][0]["kind"], json!("did"));
    assert_eq!(ecu["children"][1]["kind"], json!("class"));
    assert_eq!(ecu["children"][1]["child_count"], json!(2));
    assert_eq!(ecu["children"][1]["children"][1]["label"], json!("Instance_2"));
    let loose_kinds: Vec<&str> = ecu["children"]
        .as_array()
        .unwrap()
        .iter()
        .skip(2)
        .map(|node| node["kind"].as_str().unwrap())
        .collect();
    assert_eq!(loose_kinds, ["did", "did"]);
}

#[test]
fn cdd_preview_reads_synthetic_header() {
    let path = examples_path("Synthetic.cdd");
    assert!(path.is_file(), "missing fixture: {}", path.display());
    let mut session = Session::start();
    let result = session.call(json!({"id": 1, "op": "cdd.preview", "args": {"path": path.to_string_lossy()}}));
    assert_eq!(result["ecu"], json!("SynthECU"));
    assert_eq!(result["variants"], json!(["Base_Variant"]));
    assert_eq!(result["dtd_version"], json!("12.0.104"));
    assert!(result["counts"]["did"].is_u64(), "{result}");
    assert!(result["counts"]["dtc"].is_u64(), "{result}");
    assert_eq!(result["sha256"].as_str().map(str::len), Some(64), "{result}");

    // Synthetic.cdd declares no ECUDOC/@doctype, so it is neither a CDD(inst) nor
    // a CDDT(tmpl) document. The file extension must not stand in for the
    // declaration: a hand-made file called *.cdd is not a CANdela document.
    assert_eq!(result["doctype"], json!(""), "{result}");
    assert_eq!(result["document_kind"], json!("UNKNOWN"), "{result}");
    assert_eq!(result["supported"], json!(false), "{result}");
    assert!(
        result["unsupported_reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("doctype")),
        "{result}"
    );
}

#[test]
fn cdd_preview_reads_large_file_header() {
    let path = examples_path("CddTools.cdd");
    assert!(path.is_file(), "missing fixture: {}", path.display());
    let mut session = Session::start();
    let result = session.call(json!({"id": 1, "op": "cdd.preview", "args": {"path": path.to_string_lossy()}}));
    assert_eq!(result["ecu"], json!("CddTools"));
    assert_eq!(result["document_kind"], json!("CDD"));
    assert_eq!(result["doctype"], json!("inst"));
    assert_eq!(result["dtd_version"], json!("12.0.104"));
    assert_eq!(result["supported"], json!(true));
    assert_eq!(result["counts"]["did"], json!(16));
    assert_eq!(result["variants"], json!(["Base Variant", "Base Variant1"]));
}

#[test]
fn cdd_preview_rejects_missing_files_cleanly() {
    let mut session = Session::start();
    let response = session.send(json!({"id": 1, "op": "cdd.preview", "args": {"path": "does-not-exist.cdd"}}));
    assert_eq!(response["ok"], json!(false));
    let error = response["error"].as_str().expect("error string");
    assert!(error.contains("does-not-exist.cdd"), "{error}");
    let hello = session.call(json!({"id": 2, "op": "hello", "args": {}}));
    assert_eq!(hello["protocol"], json!(1));
}
