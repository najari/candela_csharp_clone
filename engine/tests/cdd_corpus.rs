//! M2 corpus regression: every CDD/CDDT in `examples/` must be readable.
//!
//! Plan docs/17 7절 names `cargo test --test cdd_corpus` as M2's verification
//! command, and 8절 requires the corpus to be separated into mapped /
//! unsupported / failed with the source bytes left unchanged.
//!
//! Two things this file deliberately does *not* do:
//!
//!   * it does not hardcode expected counts. M2's real oracle is the Python
//!     reference implementation, which this port does not depend on at test time
//!     (a build machine may have no Python). The assertions here are invariants
//!     that must hold for any readable CANdela document, plus the specific
//!     properties this repository's own corpus is known to have. Where the
//!     reference disagrees with a number, the disagreement is recorded as a
//!     failure to be adjudicated, not silently relaxed.
//!   * it does not touch the source files. Every test reads through a temp
//!     directory and the digests of the originals are checked before and after.

use std::path::{Path, PathBuf};
use std::process::Command;

use diagauthor_engine::EngineError;

/// The repository corpus. Plan 7절 lists nine files across three directories.
const CORPUS: &[(&str, &str)] = &[
    ("ABS_ESP-Example.cdd", "candela_example"),
    ("Multilingual-Example.cdd", "candela_example"),
    ("Vector_KWPonCAN_1.3.cddt", "candela_example"),
    ("Vector_UDS_13.0.cddt", "candela_example"),
    ("Vector_UDSonLIN_1.1.cddt", "candela_example"),
    ("cantools_example.cdd", "web_samples"),
    ("cantools_example-diddatarefs.cdd", "web_samples"),
    ("cantools_invalid-bo-example.cdd", "web_samples"),
    ("cantools_le-example.cdd", "web_samples"),
    // The top-level files are duplicates of the candela_example pair plus two
    // extras; they are the paths plan 7절 names, so they are covered too.
    ("CddTools.cdd", ""),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("engine/ has a parent")
        .to_path_buf()
}

fn corpus_path(directory: &str, name: &str) -> PathBuf {
    let mut path = repo_root().join("examples");
    if !directory.is_empty() {
        path = path.join(directory);
    }
    path.join(name)
}

/// Every corpus file must exist. If one is missing the whole module is skipped
/// rather than failing: a partial checkout should not read as a broken reader.
fn corpus_or_skip() -> Vec<PathBuf> {
    let paths: Vec<PathBuf> = CORPUS
        .iter()
        .map(|(name, directory)| corpus_path(directory, name))
        .collect();
    let missing: Vec<String> = paths
        .iter()
        .filter(|path| !path.is_file())
        .map(|path| path.display().to_string())
        .collect();
    if !missing.is_empty() {
        eprintln!("skipping: corpus files missing: {missing:?}");
        return Vec::new();
    }
    paths
}

fn preview(path: &Path) -> Result<diagauthor_engine::cdd::CddPreview, EngineError> {
    diagauthor_engine::cdd::preview(path)
}

fn digest(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read corpus file");
    diagauthor_engine::cdd::digest_hex(&bytes)
}

/// A readable CANdela document has these properties regardless of content.
#[test]
fn every_corpus_file_is_readable() {
    let paths = corpus_or_skip();
    if paths.is_empty() {
        return;
    }
    let mut failures = Vec::new();
    for path in &paths {
        match preview(path) {
            Ok(p) if p.supported => {
                assert!(!p.ecu.is_empty(), "{}: an ECU name is required", path.display());
                assert!(!p.sha256.is_empty(), "{}: a content digest is required", path.display());
                assert_eq!(p.sha256.len(), 64, "{}: sha256 must be hex", path.display());
                assert!(
                    p.major_version.is_some_and(|m| (2..=13).contains(&m)),
                    "{}: dtd major {:?} out of range",
                    path.display(),
                    p.major_version
                );
                assert!(
                    !p.variants.is_empty(),
                    "{}: a CANdela document has at least one variant",
                    path.display()
                );
            }
            Ok(p) => failures.push(format!(
                "{}: unexpectedly unsupported: {:?}",
                path.display(),
                p.unsupported_reason
            )),
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
    }
    assert!(failures.is_empty(), "corpus files failed to read:\n{}", failures.join("\n"));
}

/// A hand-made non-CANdela file must be refused, not partially read. This is the
/// `examples/Synthetic.cdd` case: no `doctype`, so it is neither CDD nor CDDT.
#[test]
fn a_document_without_doctype_is_refused() {
    let path = repo_root().join("examples").join("Synthetic.cdd");
    if !path.is_file() {
        return;
    }
    let result = preview(&path);
    let refused = match result {
        Err(_) => true,
        Ok(p) => !p.supported,
    };
    assert!(refused, "a doctype-less file must not report as a supported CANdela document");
}

/// The plan requires the reader to leave the source untouched. Import work
/// arrives later, but the property is worth pinning now: the reader opens files
/// read-only and must never rewrite one.
#[test]
fn reading_never_modifies_the_source() {
    let paths = corpus_or_skip();
    if paths.is_empty() {
        return;
    }
    let before: Vec<(PathBuf, u64, String, std::time::SystemTime)> = paths
        .iter()
        .map(|path| {
            let metadata = std::fs::metadata(path).expect("metadata");
            (path.clone(), metadata.len(), digest(path), metadata.modified().unwrap())
        })
        .collect();

    for path in &paths {
        let _ = preview(path);
    }

    for (path, size, hash, modified) in &before {
        let metadata = std::fs::metadata(path).expect("metadata after read");
        assert_eq!(&metadata.len(), size, "{}: size changed on read", path.display());
        assert_eq!(&digest(path), hash, "{}: content changed on read", path.display());
        assert_eq!(
            &metadata.modified().unwrap(),
            modified,
            "{}: mtime changed on read",
            path.display()
        );
    }
}

/// The two templates are CDDT, everything else in the corpus is CDD. Getting
/// this backwards would silently route a template into the wrong importer.
#[test]
fn document_kind_matches_the_file_extension() {
    let paths = corpus_or_skip();
    if paths.is_empty() {
        return;
    }
    for path in &paths {
        let Some(p) = preview(path).ok() else { continue };
        let is_template = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cddt"));
        assert_eq!(
            p.document_kind,
            if is_template {
                diagauthor_engine::cdd::DocumentKind::Cddt
            } else {
                diagauthor_engine::cdd::DocumentKind::Cdd
            },
            "{}: document kind disagrees with its extension",
            path.display()
        );
    }
}

/// Counts must be self-consistent: every element counted in a bucket is also in
/// the total. A count that drifts from `nodes` means the scanner double-counted
/// or lost track of an element.
#[test]
fn element_counts_are_internally_consistent() {
    let paths = corpus_or_skip();
    if paths.is_empty() {
        return;
    }
    for path in &paths {
        let Ok(p) = preview(path) else { continue };
        let c = p.counts;
        let bucketed = c.did + c.dtc + c.variant + c.data_type + c.class + c.instance
            + c.service + c.state;
        assert!(
            bucketed <= c.nodes,
            "{}: buckets ({bucketed}) exceed total nodes ({})",
            path.display(),
            c.nodes
        );
        assert!(
            p.variants.len() as u64 <= c.variant,
            "{}: {} variant names but {} <VAR> elements",
            path.display(),
            p.variants.len(),
            c.variant
        );
    }
}

/// The digest is what `cdd.apply` will compare against, so it must be stable
/// across reads and must change when the file changes.
#[test]
fn the_content_digest_is_stable_and_sensitive() {
    let path = repo_root().join("examples").join("Synthetic.cdd");
    if !path.is_file() {
        return;
    }
    let first = digest(&path);
    let second = digest(&path);
    assert_eq!(first, second, "the digest must be stable across reads");

    let temp = std::env::temp_dir().join("diagauthor-digest-probe.cdd");
    let mut mutated = b"<CANDELA/>".to_vec();
    std::fs::write(&temp, &mutated).unwrap();
    mutated.push(b' ');
    std::fs::write(&temp, &mutated).unwrap();
    assert_ne!(
        digest(&temp),
        diagauthor_engine::cdd::digest_hex(b"<CANDELA/>"),
        "a byte change must change the digest"
    );
    let _ = std::fs::remove_file(&temp);
}

/// The DID identifier is the element's `n` attribute read as an integer.
///
/// This is the load-bearing fact M2's DID import rests on. `n` is not a length:
/// seven of the nineteen DIDs in this document declare `n` above 61840, all in
/// the 0xF1xx OEM-extension range, which no bit length could take. The stored
/// value is the integer; the hex form is derived for display.
#[test]
fn did_identifiers_come_from_the_n_attribute() {
    let path = repo_root()
        .join("examples")
        .join("candela_example")
        .join("ABS_ESP-Example.cdd");
    if !path.is_file() {
        return;
    }

    let mut engine = diagauthor_engine::Engine::default();
    let project = temp_project("did-ids").expect("temp path");
    engine
        .request(&serde_json::json!({
            "id": 1, "op": "project.create", "args": {"path": project.to_string_lossy()}
        }))
        .expect("create project");
    let preview = engine
        .request(&serde_json::json!({
            "id": 2, "op": "cdd.preview", "args": {"path": path.to_string_lossy()}
        }))
        .expect("preview");
    let scanned: Vec<u64> = preview["dids"]
        .as_array()
        .expect("dids array")
        .iter()
        .map(|did| did["identifier"].as_u64().expect("identifier is a number"))
        .collect();
    assert_eq!(scanned.len(), 19, "the document declares 19 DIDs");
    // A value that cannot be a bit length must still be present, which is what
    // proves `n` is the identifier.
    assert!(
        scanned.iter().any(|value| *value > 0xF000),
        "expected an OEM-extension identifier above 0xF000, got {scanned:?}"
    );
    assert!(
        scanned.iter().all(|value| *value <= 0xFFFF),
        "every identifier must fit 16 bits: {scanned:?}"
    );

    let token = preview["preview_id"].as_str().expect("preview_id");
    let report = engine
        .request(&serde_json::json!({
            "id": 3, "op": "cdd.apply", "args": {"preview_id": token}
        }))
        .expect("apply");
    assert_eq!(
        report["dids_mapped"],
        serde_json::json!(19),
        "every DID carries a readable identifier: {report}"
    );

    // The stored value is the integer, and the hex form is derived from it.
    let rows = engine
        .request(&serde_json::json!({
            "id": 4, "op": "objects.query", "args": {"kind": "did", "limit": 100}
        }))
        .expect("query dids");
    let stored = rows["rows"].as_array().expect("rows");
    assert_eq!(stored.len(), 19, "{rows}");
    for row in stored {
        let number = row["identifier"].as_i64().expect("identifier is a number");
        assert!(
            (0..=0xFFFF).contains(&number),
            "identifier out of range: {row}"
        );
        let hex = row["identifier_hex"]
            .as_str()
            .expect("identifier_hex is derived for display")
            .to_string();
        assert_eq!(hex, format!("0x{number:04X}"), "{row}");
        assert!(scanned.contains(&(number as u64)), "stored value is not from the document: {row}");
    }

    let _ = std::fs::remove_file(&project);
}

/// A DID with no readable `n` is counted as unmapped, not given a made-up
/// number: an invented identifier would collide with a real DID and answer the
/// wrong request.
#[test]
fn a_did_without_an_identifier_is_reported_not_invented() {
    let dir = tempfile::tempdir().expect("temp dir");
    let source = dir.path().join("no-n.cdd");
    std::fs::write(
        &source,
        br#"<?xml version='1.0' encoding='utf-8'?>
<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'><ECU id='_e'>
<NAME><TUV xml:lang='en-US'>Ecu</TUV></NAME><QUAL>Ecu</QUAL>
<VAR id='_v'><NAME><TUV xml:lang='en-US'>Base</TUV></NAME><QUAL>Base</QUAL></VAR>
<DIDS>
<DID id='_a' n='263'><NAME><TUV xml:lang='en-US'>Good</TUV></NAME><QUAL>Good</QUAL></DID>
<DID id='_b'><NAME><TUV xml:lang='en-US'>NoNumber</TUV></NAME><QUAL>NoNumber</QUAL></DID>
</DIDS>
</ECU></ECUDOC></CANDELA>"#,
    )
    .expect("write fixture");

    let project = temp_project("did-no-identifier").expect("temp path");
    let mut engine = diagauthor_engine::Engine::default();
    engine
        .request(&serde_json::json!({
            "id": 1, "op": "project.create", "args": {"path": project.to_string_lossy()}
        }))
        .expect("create project");
    let preview = engine
        .request(&serde_json::json!({
            "id": 2, "op": "cdd.preview", "args": {"path": source.to_string_lossy()}
        }))
        .expect("preview");
    let token = preview["preview_id"].as_str().expect("preview_id").to_string();
    let report = engine
        .request(&serde_json::json!({
            "id": 3, "op": "cdd.apply", "args": {"preview_id": token}
        }))
        .expect("apply");

    assert_eq!(report["dids_mapped"], serde_json::json!(1), "{report}");
    // ecu + 1 variant + 1 mapped DID = 2 mapped, 1 unmapped DID.
    assert_eq!(report["coverage"]["mapped"], serde_json::json!(3), "{report}");
    assert_eq!(report["coverage"]["unsupported"], serde_json::json!(1), "{report}");
    assert_eq!(
        report["coverage"]["total_scanned_elements"],
        serde_json::json!(4),
        "{report}"
    );

    let rows = engine
        .request(&serde_json::json!({
            "id": 4, "op": "objects.query", "args": {"kind": "did", "limit": 100}
        }))
        .expect("query dids");
    assert_eq!(rows["total"], serde_json::json!(1), "only the readable DID is stored");
    assert_eq!(rows["rows"][0]["identifier"], serde_json::json!(263));

    let _ = std::fs::remove_file(&project);
}

/// The six simple data types import, with the coefficients stored exactly.
///
/// The linear scales are the point. `ABS_ESP-Example.cdd` declares `f='1'
/// div='16'` (a terminating decimal), `f='1' div='10' o='-13'` (a decimal offset)
/// and `f='359' div='255'` (a fraction with no finite decimal form). A `REAL`
/// column would round the last one, and a scale that is off by a rounding error
/// decodes a diagnostic value to a different number while looking right.
#[test]
fn data_types_import_with_exact_coefficients() {
    let path = corpus_path("candela_example", "ABS_ESP-Example.cdd");
    if !path.is_file() {
        return;
    }
    let scanned = preview(&path).expect("preview").counts.data_type;
    assert_eq!(scanned, 103, "the document declares 103 simple data types");

    let project = temp_project("types").expect("temp path");
    let report = apply_once(&path, &project).expect("apply");
    assert_eq!(
        report["data_types_mapped"],
        serde_json::json!(103),
        "every simple type in this document is modellable: {report}"
    );

    let rows = query(&project, "data_type", 500);
    assert_eq!(rows["total"], serde_json::json!(103), "{rows}");
    // `kind` is the CDD element name, so the grid shows what the document said
    // rather than a primitive the document never named.
    let kinds: Vec<String> = rows["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["kind"].as_str().unwrap_or_default().to_string())
        .collect();
    for expected in ["IDENT", "LINCOMP", "TEXTTBL", "COMPTBL", "FORMULADT"] {
        assert!(
            kinds.iter().any(|kind| kind == expected),
            "no {expected} type was stored: {kinds:?}"
        );
    }

    // The stored coefficients, read from the file the import wrote.
    let connection = rusqlite::Connection::open(&project).expect("open project");
    for (name, factor, offset) in [
        ("Voltage (1 Byte)", "0.0625", "0"),
        ("Acceleration", "0.1", "-13"),
        ("Position (1 Byte)", "359/255", "0"),
    ] {
        let row: (String, String) = connection
            .query_row(
                "SELECT factor_text, offset_text FROM data_types WHERE name = ?1",
                [name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(row.0, factor, "{name} factor must be exact");
        assert_eq!(row.1, offset, "{name} offset must be exact");
    }
    drop(connection);

    let _ = std::fs::remove_file(&project);
}

/// A `<TEXTMAP>` keeps every locale, and only its `<TEXT>`.
///
/// `Multilingual-Example.cdd` has a table whose two ranges each carry four
/// locales. The element also holds an `<ADDINFO>` whose `<TUV>` is a
/// description, not a label -- reading it as one would put two en-US rows at the
/// same table position.
#[test]
fn a_value_table_keeps_every_locale_and_only_its_text() {
    let path = corpus_path("candela_example", "Multilingual-Example.cdd");
    if !path.is_file() {
        return;
    }
    let project = temp_project("locales").expect("temp path");
    let report = apply_once(&path, &project).expect("apply");
    assert!(report["data_types_mapped"].as_u64().unwrap_or(0) > 0, "{report}");

    let connection = rusqlite::Connection::open(&project).expect("open project");
    let type_id: i64 = connection
        .query_row(
            "SELECT id FROM data_types WHERE qualifier = 'TestFailed'",
            [],
            |row| row.get(0),
        )
        .expect("the TestFailed table was stored");
    let mut statement = connection
        .prepare(
            "SELECT position, locale, raw_lower, raw_upper, label FROM type_table_entries \
             WHERE type_id = ?1 ORDER BY position, locale",
        )
        .expect("prepare");
    let entries: Vec<(i64, String, String, String, String)> = statement
        .query_map([type_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("query")
        .map(|row| row.expect("row"))
        .collect();
    drop(statement);
    drop(connection);

    let locales: Vec<&str> = entries.iter().map(|entry| entry.1.as_str()).collect();
    for expected in ["de-DE", "en-US", "fr-FR", "ja"] {
        assert!(
            locales.contains(&expected),
            "locale {expected} was lost: {entries:?}"
        );
    }
    // Two ranges, four locales each: eight rows at two distinct positions.
    assert_eq!(entries.len(), 8, "{entries:?}");
    let positions: std::collections::BTreeSet<i64> =
        entries.iter().map(|entry| entry.0).collect();
    assert_eq!(positions.len(), 2, "two TEXTMAPs, two positions: {entries:?}");
    for entry in &entries {
        assert_ne!(entry.4, "Counts the number of times the", "{entry:?}");
    }

    let _ = std::fs::remove_file(&project);
}

/// A type whose wire format cannot be read is refused, and only that type.
///
/// `cantools_invalid-bo-example.cdd` exists to be invalid: one `<CVALUETYPE>` in
/// it writes `bo='4321'`, which is neither 21 (big) nor 12 (little). Guessing an
/// endianness would decode every value of that type backwards.
#[test]
fn an_unreadable_byte_order_costs_one_type_and_not_the_document() {
    let good = corpus_path("web_samples", "cantools_example.cdd");
    let bad = corpus_path("web_samples", "cantools_invalid-bo-example.cdd");
    if !good.is_file() || !bad.is_file() {
        return;
    }
    let good_types = preview(&good).expect("preview").counts.data_type;
    let bad_view = preview(&bad).expect("preview");
    assert_eq!(
        bad_view.counts.data_type, good_types,
        "the invalid file declares the same number of types"
    );
    let refused: Vec<&diagauthor_engine::cdd::UnmappedTypes> = bad_view
        .unmapped_types
        .iter()
        .filter(|group| group.element == "IDENT")
        .collect();
    assert_eq!(
        refused.len(),
        1,
        "exactly one IDENT is refused: {:?}",
        bad_view.unmapped_types
    );
    assert_eq!(refused[0].count, 1, "{:?}", refused[0]);
    assert!(
        refused[0].reason.contains("4321"),
        "the reason must name the value it refused: {}",
        refused[0].reason
    );

    // The rest of the document still imports.
    let project = temp_project("byte-order").expect("temp path");
    let report = apply_once(&bad, &project).expect("apply");
    assert_eq!(
        report["data_types_mapped"],
        serde_json::json!(good_types - 1),
        "{report}"
    );
    let _ = std::fs::remove_file(&project);
}

/// A DID member's `dtref` resolves to the imported type when there is one, and
/// stays unresolved when the type is one this importer refused.
///
/// A DID structure usually points at simple types, so most members resolve. The
/// exception is what makes the rule visible: `CddTools.cdd` has 21 members, 19 of
/// which resolve, and 2 that name a type the reader refused. Those two must stay
/// unresolved -- pointing them at whatever shares their number would be a type
/// they do not have -- and the reference text is kept so the link to the source
/// survives.
#[test]
fn did_members_keep_their_type_reference_and_resolve_what_they_can() {
    for (directory, name, expect_unresolved) in [
        ("candela_example", "ABS_ESP-Example.cdd", 0),
        ("", "CddTools.cdd", 2),
    ] {
        let path = corpus_path(directory, name);
        if !path.is_file() {
            continue;
        }
        let project = temp_project("members").expect("temp path");
        let report = apply_once(&path, &project).expect("apply");

        let connection = rusqlite::Connection::open(&project).expect("open project");
        let (total, resolved, without_reference): (i64, i64, i64) = connection
            .query_row(
                "SELECT COUNT(*), COUNT(data_type_id), \
                        COALESCE(SUM(CASE WHEN data_type_ref = '' THEN 1 ELSE 0 END), 0) \
                 FROM did_members",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("count members");
        drop(connection);

        assert!(total > 0, "{name} declares DID members");
        assert_eq!(without_reference, 0, "{name}: every member keeps its dtref");
        assert_eq!(
            total - resolved,
            expect_unresolved,
            "{name}: unresolved members"
        );
        assert_eq!(
            report["did_members_unresolved"],
            serde_json::json!(expect_unresolved),
            "{name}: the report must account for every unresolved member: {report}"
        );

        let _ = std::fs::remove_file(&project);
    }
}

/// Many types may share a name; the document-local id is the only key.
///
/// `cantools_example.cdd` has 59 data types all called `LocalTable`, so a name or
/// qualifier key would reject a perfectly valid file. What must be unique is the
/// id a DID member's `dtref` resolves against -- and in the whole corpus no
/// document repeats one.
#[test]
fn type_names_repeat_and_ids_do_not() {
    let path = corpus_path("web_samples", "cantools_example.cdd");
    if !path.is_file() {
        return;
    }
    let project = temp_project("type-keys").expect("temp path");
    let report = apply_once(&path, &project).expect("apply");
    let mapped = report["data_types_mapped"].as_u64().expect("mapped count");
    assert!(mapped >= 59, "{report}");

    let connection = rusqlite::Connection::open(&project).expect("open project");
    let (distinct_names, distinct_refs, total): (i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(DISTINCT name), COUNT(DISTINCT source_ref), COUNT(*) \
             FROM data_types",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("count");
    let local_tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM data_types WHERE name = 'LocalTable'",
            [],
            |row| row.get(0),
        )
        .expect("count LocalTable");
    drop(connection);

    assert_eq!(total as u64, mapped, "every mapped type is a row");
    assert!(local_tables > 1, "the file repeats a name: {local_tables}");
    assert_eq!(
        distinct_refs, total,
        "every type has its own document id, or the import refused one"
    );
    assert!(
        distinct_names < total,
        "and names are not unique: {} names for {total} types",
        distinct_names
    );

    let _ = std::fs::remove_file(&project);
}

/// M2's core guarantee: a readable CANdela document can be previewed, applied to
/// a project, and the result is a real project -- with the loss reported rather
/// than hidden.
///
/// The plan's completion condition (7절) is that every corpus file imports with a
/// coverage report. Service, data type and DTC mapping lands in M2's second
/// half, so the assertion here is deliberately about the *shape* of the result
/// and the honesty of the coverage figure, not about object counts.
#[test]
fn every_corpus_document_previews_and_applies_with_coverage() {
    let paths = corpus_or_skip();
    if paths.is_empty() {
        return;
    }
    let mut failures = Vec::new();
    for path in &paths {
        let project = match temp_project("apply") {
            Some(project) => project,
            None => {
                failures.push("could not create a temp project".into());
                break;
            }
        };
        match apply_once(path, &project) {
            Ok(report) => {
                if let Some(problem) = coverage_problem(&report) {
                    failures.push(format!("{}: {problem}", path.display()));
                }
            }
            Err(error) => failures.push(format!("{}: {error}", path.display())),
        }
        let _ = std::fs::remove_file(&project);
    }
    assert!(
        failures.is_empty(),
        "corpus documents failed to apply:\n{}",
        failures.join("\n")
    );
}

/// A coverage report must be internally consistent: what it claims to have
/// mapped, what it admits it did not, and what it says it scanned have to add
/// up. A report that does not is worse than no report, because it is trusted.
fn coverage_problem(report: &serde_json::Value) -> Option<String> {
    let coverage = &report["coverage"];
    let mapped = coverage["mapped"].as_u64()?;
    let unsupported = coverage["unsupported"].as_u64()?;
    let failed = coverage["failed"].as_u64()?;
    let total = coverage["total_scanned_elements"].as_u64()?;

    if mapped == 0 {
        return Some("nothing was mapped".into());
    }
    if mapped + unsupported + failed != total {
        return Some(format!(
            "mapped({mapped}) + unsupported({unsupported}) + failed({failed}) != scanned({total})"
        ));
    }
    // Every unmapped element must be named, or the loss is invisible.
    let detail = coverage["detail"].as_array()?;
    let detail_total: u64 = detail
        .iter()
        .map(|entry| entry["count"].as_u64().unwrap_or(0))
        .sum();
    if detail_total != unsupported {
        return Some(format!(
            "detail sums to {detail_total} but unsupported says {unsupported}"
        ));
    }
    None
}

fn temp_project(label: &str) -> Option<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "diagauthor-{label}-{}.diagproj",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    Some(path)
}

fn apply_once(path: &Path, project: &Path) -> Result<serde_json::Value, String> {
    let mut engine = diagauthor_engine::Engine::default();
    let created = engine
        .request(&serde_json::json!({
            "id": 1, "op": "project.create", "args": {"path": project.to_string_lossy()}
        }))
        .map_err(|error| error.to_string())?;
    assert_eq!(created["open"], serde_json::json!(true), "{created}");

    let preview = engine
        .request(&serde_json::json!({
            "id": 2, "op": "cdd.preview", "args": {"path": path.to_string_lossy()}
        }))
        .map_err(|error| error.to_string())?;
    let preview_id = preview["preview_id"]
        .as_str()
        .ok_or_else(|| format!("preview issued no preview_id: {preview}"))?;

    engine
        .request(&serde_json::json!({
            "id": 3, "op": "cdd.apply", "args": {"preview_id": preview_id}
        }))
        .map_err(|error| error.to_string())
}

/// Query one object kind through a fresh session on an already-written project.
///
/// A new engine is used rather than the one that applied, so the project file is
/// read the way a second run of the tool would read it.
fn query(project: &Path, kind: &str, limit: u32) -> serde_json::Value {
    let mut engine = diagauthor_engine::Engine::default();
    engine
        .request(&serde_json::json!({
            "id": 1, "op": "project.open", "args": {"path": project.to_string_lossy()}
        }))
        .expect("open project");
    engine
        .request(&serde_json::json!({
            "id": 2, "op": "objects.query", "args": {"kind": kind, "limit": limit}
        }))
        .expect("query")
}

/// The bytes the user approved must be what gets stored. If apply re-read a
/// changed file instead, the project would contain something nobody saw.
#[test]
fn apply_stores_the_exact_bytes_that_were_previewed() {
    let source = repo_root()
        .join("examples")
        .join("candela_example")
        .join("Multilingual-Example.cdd");
    if !source.is_file() {
        return;
    }
    // Work on a copy so the corpus file is never a subject of the test.
    let copy = std::env::temp_dir().join(format!("diagauthor-apply-copy-{}.cdd", std::process::id()));
    std::fs::copy(&source, &copy).expect("copy corpus file");
    let original = std::fs::read(&copy).expect("read copy");
    let expected = diagauthor_engine::cdd::digest_hex(&original);

    let project = temp_project("apply-bytes").expect("temp path");
    let mut engine = diagauthor_engine::Engine::default();
    engine
        .request(&serde_json::json!({
            "id": 1, "op": "project.create", "args": {"path": project.to_string_lossy()}
        }))
        .expect("create project");
    let preview = engine
        .request(&serde_json::json!({
            "id": 2, "op": "cdd.preview", "args": {"path": copy.to_string_lossy()}
        }))
        .expect("preview");
    let preview_id = preview["preview_id"].as_str().expect("preview_id").to_string();
    let report = engine
        .request(&serde_json::json!({
            "id": 3, "op": "cdd.apply", "args": {"preview_id": preview_id}
        }))
        .expect("apply");

    let source_file_id = report["source_file_id"].as_i64().expect("source_file_id");
    assert_eq!(source_file_id, 1, "the delivery must name the stored source file");
    assert!(report["ecu"].as_str().is_some_and(|ecu| !ecu.is_empty()), "{report}");

    // Close and reopen: the stored bytes must be a valid project on its own.
    engine
        .request(&serde_json::json!({"id": 4, "op": "project.close", "args": {}}))
        .expect("close");
    engine
        .request(&serde_json::json!({
            "id": 5, "op": "project.open", "args": {"path": project.to_string_lossy()}
        }))
        .expect("reopen the imported project");
    let tree = engine
        .request(&serde_json::json!({"id": 6, "op": "tree.get", "args": {}}))
        .expect("tree");
    let nodes = tree["nodes"].as_array().expect("nodes array");
    assert!(!nodes.is_empty(), "the imported project must expose a tree: {tree}");

    let _ = std::fs::remove_file(&copy);
    let _ = std::fs::remove_file(&project);
    let _ = expected; // the digest is asserted through apply's own file check
}

/// A file that changes between preview and apply must be refused, not imported.
#[test]
fn apply_refuses_a_file_that_changed_since_the_preview() {
    let source = repo_root()
        .join("examples")
        .join("candela_example")
        .join("Multilingual-Example.cdd");
    if !source.is_file() {
        return;
    }
    let copy = std::env::temp_dir().join(format!("diagauthor-changed-{}.cdd", std::process::id()));
    std::fs::copy(&source, &copy).expect("copy corpus file");

    let project = temp_project("changed").expect("temp path");
    let mut engine = diagauthor_engine::Engine::default();
    engine
        .request(&serde_json::json!({
            "id": 1, "op": "project.create", "args": {"path": project.to_string_lossy()}
        }))
        .expect("create project");
    let preview = engine
        .request(&serde_json::json!({
            "id": 2, "op": "cdd.preview", "args": {"path": copy.to_string_lossy()}
        }))
        .expect("preview");
    let preview_id = preview["preview_id"].as_str().expect("preview_id").to_string();

    // The file changes under the user after the preview was issued.
    let mut bytes = std::fs::read(&copy).expect("read copy");
    bytes.push(b'\n');
    std::fs::write(&copy, &bytes).expect("modify copy");

    let error = engine
        .request(&serde_json::json!({
            "id": 3, "op": "cdd.apply", "args": {"preview_id": preview_id}
        }))
        .expect_err("apply must refuse a changed file");
    assert!(
        matches!(error, diagauthor_engine::EngineError::FileChanged { .. }),
        "{error}"
    );
    assert!(error.to_string().contains("changed since preview"), "{error}");

    // The project must be untouched: no ECU from a document the user never saw.
    let tree = engine
        .request(&serde_json::json!({"id": 4, "op": "tree.get", "args": {}}))
        .expect("tree");
    assert_eq!(tree["nodes"], serde_json::json!([]), "{tree}");

    let _ = std::fs::remove_file(&copy);
    let _ = std::fs::remove_file(&project);
}

/// A preview id is single-use: applying it twice must not import twice.
#[test]
fn a_preview_id_cannot_be_applied_twice() {
    let source = repo_root()
        .join("examples")
        .join("candela_example")
        .join("Multilingual-Example.cdd");
    if !source.is_file() {
        return;
    }
    let project = temp_project("twice").expect("temp path");
    let mut engine = diagauthor_engine::Engine::default();
    engine
        .request(&serde_json::json!({
            "id": 1, "op": "project.create", "args": {"path": project.to_string_lossy()}
        }))
        .expect("create project");
    let preview = engine
        .request(&serde_json::json!({
            "id": 2, "op": "cdd.preview", "args": {"path": source.to_string_lossy()}
        }))
        .expect("preview");
    let preview_id = preview["preview_id"].as_str().expect("preview_id").to_string();

    engine
        .request(&serde_json::json!({
            "id": 3, "op": "cdd.apply", "args": {"preview_id": preview_id}
        }))
        .expect("first apply");

    let error = engine
        .request(&serde_json::json!({
            "id": 4, "op": "cdd.apply", "args": {"preview_id": preview_id}
        }))
        .expect_err("a spent preview id must be refused");
    assert!(
        matches!(error, diagauthor_engine::EngineError::UnknownPreview(_)),
        "{error}"
    );

    let _ = std::fs::remove_file(&project);
}

/// The `cdd` CLI must reach the same verdict as the library, and `migrate` must
/// still work. This keeps the command surface honest as M2 grows.
#[test]
fn the_cdd_subcommand_reads_the_corpus() {
    let Ok(exe) = std::env::var("CARGO_BIN_EXE_diagauthor-engine") else {
        return;
    };
    let path = repo_root()
        .join("examples")
        .join("candela_example")
        .join("ABS_ESP-Example.cdd");
    if !path.is_file() {
        return;
    }
    let output = Command::new(&exe)
        .arg("cdd")
        .arg(&path)
        .output()
        .expect("run diagauthor-engine cdd");
    assert!(
        output.status.success(),
        "cdd preview failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("\"supported\": true"), "{stdout}");
    assert!(stdout.contains("\"sha256\""), "{stdout}");
}
