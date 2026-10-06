//! CLI entry point: `serve`, `info`, `migrate <path>`, `cdd <path>`, `--version`.
//!
//! `serve` is a JSON Lines loop over stdin/stdout. stdout carries protocol
//! lines only; every log or diagnostic goes to stderr. Each request is wrapped
//! in `catch_unwind` so a panic in one handler becomes an error response and
//! the loop survives to answer the next request.
//!
//! `cdd` exists so a corpus file can be inspected without the GUI, and so the
//! integration tests can exercise the same path a user would.

use diagauthor_engine::{Engine, cdd, migrate_project, protocol};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        None | Some("serve") => serve(),
        Some("info") => info(),
        Some("migrate") => match args.get(1) {
            Some(path) => migrate(Path::new(path)),
            None => usage("migrate requires a project path"),
        },
        Some("cdd") => match args.get(1) {
            Some(path) => cdd_preview(
                Path::new(path),
                matches!(args.get(2).map(String::as_str), Some("--data-types")),
            ),
            None => usage("cdd requires a CDD/CDDT path"),
        },
        Some("--version") => {
            println!(
                "{} {} / SQLite {}",
                protocol::ENGINE_NAME,
                protocol::ENGINE_VERSION,
                rusqlite::version()
            );
            0
        }
        Some(other) => usage(&format!("unknown command '{other}'")),
    };
    std::process::exit(code);
}

fn usage(detail: &str) -> i32 {
    eprintln!("{detail}");
    eprintln!(
        "usage: diagauthor-engine [serve | info | migrate <path> | cdd <path> [--data-types] | --version]"
    );
    2
}

fn info() -> i32 {
    let payload = json!({
        "engine": protocol::ENGINE_NAME,
        "version": protocol::ENGINE_VERSION,
        "protocol": protocol::PROTOCOL_VERSION,
        "schema_version": protocol::SCHEMA_VERSION,
        "sqlite": rusqlite::version(),
    });
    println!("{payload}");
    0
}

fn migrate(path: &Path) -> i32 {
    match migrate_project(path) {
        Ok(report) => {
            println!("{report}");
            0
        }
        Err(error) => {
            eprintln!("migrate failed: {error}");
            2
        }
    }
}

/// Print a CDD/CDDT preview as JSON. A file that reads but is not a supported
/// CANdela document still exits 0 with `"supported": false` -- the caller asked
/// what is in the file, and "this is not one of ours" is an answer. Only a
/// failure to read at all is an error.
///
/// `with_data_types` adds the parsed simple types. They are left out of the
/// default output because they are not part of the `cdd.preview` response: a
/// large OEM document declares tens of thousands of them, and a preview that
/// carried every record would grow with the file.
fn cdd_preview(path: &Path, with_data_types: bool) -> i32 {
    match cdd::preview(path) {
        Ok(preview) => {
            let mut payload = serde_json::to_value(&preview).expect("preview serializes");
            if with_data_types
                && let Some(object) = payload.as_object_mut()
            {
                object.insert(
                    "data_types".to_string(),
                    serde_json::to_value(&preview.data_types).expect("types serialize"),
                );
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).expect("preview serializes")
            );
            0
        }
        Err(error) => {
            eprintln!("cdd preview failed: {error}");
            2
        }
    }
}

/// Run the JSON Lines loop until stdin closes (or stdout goes away).
fn serve() -> i32 {
    let mut engine = Engine::default();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut input = stdin.lock();
    let mut output = stdout.lock();
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match input.read_until(b'\n', &mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                eprintln!("stdin error: {error}");
                break;
            }
        }
        while matches!(buffer.last(), Some(b'\n' | b'\r')) {
            buffer.pop();
        }
        if buffer.is_empty() {
            continue;
        }
        let response = match std::str::from_utf8(&buffer) {
            Ok(line) => handle_line(&mut engine, line),
            Err(error) => protocol::Response::err(
                Value::Null,
                format!("Invalid UTF-8 in request: {error}"),
            ),
        };
        let encoded = match serde_json::to_string(&response) {
            Ok(encoded) => encoded,
            Err(error) => {
                eprintln!("failed to encode response: {error}");
                break;
            }
        };
        let wrote = output
            .write_all(encoded.as_bytes())
            .and_then(|()| output.write_all(b"\n"))
            .and_then(|()| output.flush());
        if wrote.is_err() {
            eprintln!("stdout closed; stopping serve loop");
            break;
        }
    }
    0
}

fn handle_line(engine: &mut Engine, line: &str) -> protocol::Response {
    let request: Value = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return protocol::Response::err(Value::Null, format!("Invalid JSON: {error}"));
        }
    };
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| engine.request(&request)));
    match outcome {
        Ok(Ok(result)) => protocol::Response::ok(id, result),
        Ok(Err(error)) => protocol::Response::err(id, error.to_string()),
        Err(_) => protocol::Response::err(id, "Internal engine error"),
    }
}
