//! CDD import: the `cdd.preview` -> `cdd.apply` handoff.
//!
//! Plan docs/17 3절 (D-17) names `cdd_apply` as the fourth responsibility split
//! out of the reference `apply_cdd` (Python `application.py` line 3060+).
//!
//! The two-step shape exists for a reason the plan is explicit about (4절):
//! preview and apply are separate user-visible steps, and a document can change
//! in between. So `cdd.preview` mints a `preview_id` that pins the exact bytes
//! it read, and `cdd.apply` refuses unless the file still hashes to the same
//! value. A file that changed under the user is reported, not silently
//! re-imported.
//!
//! ## What M2 completes, and what it does not
//!
//! Applying records the source file, the ECU and its variants, the DIDs, and
//! the six CDD simple data types. It does **not** yet build composite types, DTC
//! records, services or byte layouts. Those are reported as `unsupported` with a
//! reason, so a coverage figure is honest: "the file contains 19 DID elements and
//! we mapped 0" is a true statement, and a partially-imported document can never
//! be mistaken for a complete one.
//!
//! A DID member's `dtref` is resolved against the imported data types. A member
//! naming a composite type stays unresolved, and both facts are kept: the
//! reference text, so the link to the source survives, and a count, so the
//! missing link is visible.

use std::collections::HashMap;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::cdd::{self, CddPreview, DocumentKind};
use crate::storage::Storage;
use crate::{EngineError, session::Session};

/// How long an unapplied preview stays usable.
///
/// The window bounds how long the engine holds the original bytes of a file the
/// user has not yet committed to importing. It is long enough to cover a person
/// reading a coverage table, and short enough that a forgotten preview does not
/// pin megabytes indefinitely.
const PREVIEW_TTL_SECONDS: u64 = 600;
/// How many unapplied previews are retained. Older entries are dropped first.
const MAX_PENDING_PREVIEWS: usize = 16;

/// A preview the user has seen and may now apply.
///
/// Public only so it can be named in [`Previews::take`]'s signature; the type is
/// not part of the protocol and callers get the preview as JSON.
#[derive(Debug, Clone)]
pub struct PendingPreview {
    /// Content digest of the bytes that were read.
    sha256: String,
    path: String,
    document_kind: DocumentKind,
    ecu: String,
    ecu_qualifier: String,
    manufacturer: String,
    variants: Vec<String>,
    counts: cdd::CddCounts,
    /// The DIDs read, kept so apply inserts exactly what preview reported.
    dids: Vec<cdd::CddDid>,
    /// The simple data types read, kept for the same reason. `cdd.preview` does
    /// not put them on the wire -- a large document declares tens of thousands --
    /// but apply needs the very records the user saw summarised, and re-reading
    /// the file for them would defeat the digest check above.
    data_types: Vec<cdd::CddDataType>,
    /// Type elements the reader refused, grouped by element and reason. Reported
    /// so a coverage figure says what was skipped.
    unmapped_types: Vec<cdd::UnmappedTypes>,
    /// The original bytes, kept so apply stores exactly what the user approved
    /// rather than re-reading a file that may since have changed.
    content: Vec<u8>,
    issued_at: std::time::Instant,
}

impl PendingPreview {
    /// A reference to an owned buffer as a `&str`, valid for any path bytes.
    fn path(&self) -> &str {
        std::str::from_utf8(self.path.as_bytes()).unwrap_or_default()
    }
}

/// Mints and holds previews until they are applied or expire.
#[derive(Default)]
pub struct Previews {
    pending: HashMap<String, PendingPreview>,
}

impl Previews {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read `path`, hold the bytes, and return the preview plus its id.
    pub fn issue(&mut self, path: &std::path::Path) -> Result<(String, CddPreview), EngineError> {
        let preview = cdd::preview(path)?;
        // Re-read is avoided: cdd::preview already read the bytes, so the digest
        // it reports describes exactly these bytes.
        let content = std::fs::read(path).map_err(|e| EngineError::Io {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        self.expire();
        self.evict_oldest_if_full();

        let id = Uuid::new_v4().to_string();
        self.pending.insert(
            id.clone(),
            PendingPreview {
                sha256: preview.sha256.clone(),
                path: preview.path.clone(),
                document_kind: preview.document_kind,
                ecu: preview.ecu.clone(),
                ecu_qualifier: preview.ecu_qualifier.clone(),
                manufacturer: preview.manufacturer.clone(),
                variants: preview.variants.clone(),
                counts: preview.counts,
                dids: preview.dids.clone(),
                data_types: preview.data_types.clone(),
                unmapped_types: preview.unmapped_types.clone(),
                content,
                issued_at: std::time::Instant::now(),
            },
        );
        Ok((id, preview))
    }

    /// Take a preview, removing it from the pending set.
    ///
    /// Single-use by construction: `take` removes, so a spent id is
    /// `UnknownPreview` on the second call rather than importing twice.
    pub(crate) fn take(&mut self, id: &str) -> Result<PendingPreview, EngineError> {
        self.expire();
        self.pending
            .remove(id)
            .ok_or_else(|| EngineError::UnknownPreview(id.to_string()))
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    fn expire(&mut self) {
        self.pending
            .retain(|_, preview| preview.issued_at.elapsed().as_secs() < PREVIEW_TTL_SECONDS);
    }

    fn evict_oldest_if_full(&mut self) {
        while self.pending.len() >= MAX_PENDING_PREVIEWS {
            let Some(oldest) = self
                .pending
                .iter()
                .min_by_key(|(_, preview)| preview.issued_at)
                .map(|(id, _)| id.clone())
            else {
                return;
            };
            self.pending.remove(&oldest);
        }
    }
}

/// `cdd.apply` -- apply a previously issued `cdd.preview`.
pub fn apply(
    session: &mut Session,
    previews: &mut Previews,
    args: &Value,
) -> Result<Value, EngineError> {
    let preview_id = args
        .get("preview_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            EngineError::InvalidArgs("cdd.apply requires a non-empty 'preview_id'".into())
        })?;
    let pending = previews.take(preview_id)?;

    // The user approved specific bytes. If the file changed or vanished, those
    // are no longer what is on disk, and importing anyway would put something in
    // the project that nobody saw.
    match std::fs::read(&pending.path) {
        Ok(current) if cdd::digest_hex(&current) != pending.sha256 => {
            return Err(EngineError::FileChanged {
                path: pending.path.clone(),
                expected: pending.sha256,
            });
        }
        Ok(_) => {}
        Err(_) => {
            return Err(EngineError::FileChanged {
                path: pending.path.clone(),
                expected: pending.sha256,
            });
        }
    }

    // A preview of an unsupported document must never be applied, even if the
    // user asks: there is no import to perform, and pretending otherwise would
    // create an empty project that looks like a successful import.
    if pending.document_kind == DocumentKind::Unknown {
        return Err(EngineError::Unsupported(format!(
            "{} is not a supported CDD(inst) or CDDT(tmpl) document; nothing to import",
            pending.path()
        )));
    }

    // A CANdela document has at least one variant; the plan makes the base
    // variant the starting point for a new project. If the scan found none,
    // refuse rather than invent one -- an empty ECU is not a document.
    if pending.variants.is_empty() {
        return Err(EngineError::CddParse {
            path: pending.path.clone(),
            detail: "document declares no <VAR> element, so there is no variant to import".into(),
        });
    }

    let result = {
        // The session's own connection is the single writer (D-7). Borrow it
        // for the duration of the apply so every insert happens under the
        // session's view and the connection is never staled underneath us.
        let storage = session.storage_mut()?;
        apply_to_project(storage, &pending)?
    };
    session.touch();
    Ok(result)
}

/// Insert the source file, the ECU, and its variants, then report coverage.
fn apply_to_project(storage: &mut Storage, pending: &PendingPreview) -> Result<Value, EngineError> {
    let kind = match pending.document_kind {
        DocumentKind::Cddt => "cddt",
        _ => "cdd",
    };
    let source_id = storage.record_source_file(
        pending.path(),
        kind,
        &pending.sha256,
        &pending.content,
    )?;
    let ecu_id =
        storage.insert_ecu(&pending.ecu, &pending.ecu_qualifier, &pending.manufacturer)?;
    storage.map_source_node(ecu_id, source_id, "/CANDELA/ECUDOC/ECU", "ECU")?;

    let mut variant_ids = Vec::new();
    for (index, name) in pending.variants.iter().enumerate() {
        let variant_id = storage.insert_variant(ecu_id, name, index == 0)?;
        storage.map_source_node(
            variant_id,
            source_id,
            &format!("/CANDELA/ECUDOC/ECU/VAR[{index}]"),
            "VAR",
        )?;
        variant_ids.push(variant_id);
    }

    // Data types are imported before the DIDs, because a DID member's `dtref`
    // points at a type by the id the document gave it, and resolving that needs
    // the type to already be a row.
    let mut mapped_types: u64 = 0;
    let mut unmapped_type_count: u64 = 0;
    for (index, data_type) in pending.data_types.iter().enumerate() {
        match storage.insert_data_type(ecu_id, data_type) {
            Ok(type_id) => {
                mapped_types += 1;
                storage.map_source_node(
                    type_id,
                    source_id,
                    &format!(
                        "/CANDELA/ECUDOC/DATATYPES/{}[{index}]",
                        data_type.kind.element()
                    ),
                    data_type.kind.element(),
                )?;
            }
            Err(EngineError::SqliteConstraint(message)) => {
                // Two elements sharing a document-local id is a document defect.
                // The import continues: failing here would lose every DID in the
                // file over one duplicated type.
                unmapped_type_count += 1;
                storage.add_import_issue(
                    "warning",
                    "CDD-TYPE-DUPLICATE-SOURCE-REF",
                    None,
                    None,
                    &format!(
                        "data type '{}' repeats document id '{}' and was not imported \
                         again: {message}",
                        data_type.name, data_type.id
                    ),
                )?;
            }
            Err(error) => return Err(error),
        }
    }
    let type_ids = storage.data_type_ids_by_source_ref(ecu_id)?;

    // DIDs are mapped. Every variant in a CDD lists the same document-wide DID
    // set, so a DID belongs to the ECU, and `variant_dids` links each variant to
    // it -- which is what lets a later stage show "this variant hides that DID".
    let mut mapped_dids: u64 = 0;
    let mut unmapped_dids: u64 = 0;
    // Members whose `dtref` named a type this importer did not map. Counted and
    // reported, never silently dropped: a DID member shown with no type would
    // read as a member that has no type, which is a different claim.
    let mut unresolved_members: u64 = 0;
    for (index, did) in pending.dids.iter().enumerate() {
        // `n` is the DID number. A DID with no readable `n` cannot be
        // identified, and inventing one would collide with a real DID and
        // answer the wrong request -- so it is counted as unmapped instead.
        let Some(identifier) = did.identifier else {
            unmapped_dids += 1;
            storage.add_import_issue(
                "warning",
                "CDD-DID-NO-IDENTIFIER",
                None,
                None,
                &format!(
                    "DID '{}' (qualifier '{}') declares no readable 'n' identifier, \
                     so it cannot be identified and was not imported",
                    did.name, did.qualifier
                ),
            )?;
            continue;
        };
        // A duplicate identifier inside one ECU is a document defect. The import
        // continues and the loss is reported, because failing the whole document
        // over one bad DID loses everything that was correct.
        match storage.insert_did(ecu_id, &did.name, identifier) {
            Ok(did_id) => {
                storage.set_did_qualifier(did_id, &did.qualifier)?;
                import_did_detail(
                    storage,
                    source_id,
                    did_id,
                    did,
                    index,
                    &variant_ids,
                    &type_ids,
                    &mut unresolved_members,
                )?;
                mapped_dids += 1;
            }
            Err(EngineError::SqliteConstraint(message)) => {
                unmapped_dids += 1;
                storage.add_import_issue(
                    "warning",
                    "CDD-DID-DUPLICATE-IDENTIFIER",
                    None,
                    None,
                    &format!(
                        "DID 0x{identifier:04X} ('{}') repeats an identifier already imported \
                         for this ECU and was not imported again: {message}",
                        did.name
                    ),
                )?;
            }
            Err(error) => return Err(error),
        }
    }

    // Everything not yet mapped is reported, not silently dropped. M2's later
    // work teaches the engine to map these; until then the counts must say so.
    let mut unsupported = Vec::new();
    let mut unsupported_total: u64 = unmapped_dids + unmapped_type_count;
    if unmapped_dids > 0 {
        unsupported.push(json!({
            "kind": "did",
            "count": unmapped_dids,
            "reason": "no identifier could be read from the element",
        }));
    }
    if unmapped_type_count > 0 {
        unsupported.push(json!({
            "kind": "data_type",
            "count": unmapped_type_count,
            "reason": "the element repeats a document id another type already used",
        }));
    }
    // A type element the reader refused, with the reason it shares. One issue per
    // group rather than per element: a 3500-element group would bury the report
    // in near-identical rows and still say the same thing.
    for group in &pending.unmapped_types {
        let issue = storage.add_import_issue(
            "info",
            "CDD-TYPE-NOT-MAPPED",
            None,
            None,
            &format!(
                "{} <{}> element(s) were read but not imported: {} (document ids: {})",
                group.count,
                group.element,
                group.reason,
                group.examples.join(", ")
            ),
        )?;
        unsupported_total += group.count;
        unsupported.push(json!({
            "kind": group.element,
            "count": group.count,
            "reason": group.reason,
            "examples": group.examples,
            "issue_id": issue,
        }));
    }
    if unresolved_members > 0 {
        // Deliberately not a row in `coverage.detail`. The member itself is
        // mapped -- its row exists, with the reference text kept -- and the type
        // it names is already counted above among the unmapped elements. Adding
        // it here would count one lost type twice and make the totals disagree.
        storage.add_import_issue(
            "info",
            "CDD-MEMBER-TYPE-UNRESOLVED",
            None,
            None,
            &format!(
                "{unresolved_members} DID member(s) name a data type this import did not \
                 map -- a composite type, most likely. The reference is kept, and the \
                 member's type shows as unresolved until that type is imported"
            ),
        )?;
    }
    let counts = pending.counts;
    for (label, count) in [
        ("dtc", counts.dtc),
        ("class", counts.class),
        ("instance", counts.instance),
        ("service", counts.service),
    ] {
        if count == 0 {
            continue;
        }
        let issue = storage.add_import_issue(
            "info",
            &format!("CDD-{label}-NOT-MAPPED"),
            None,
            None,
            &format!(
                "{count} {label} element(s) were read but not imported; service and DTC \
                 import lands with the second half of M2"
            ),
        )?;
        unsupported_total += count;
        unsupported.push(json!({
            "kind": label,
            "count": count,
            "issue_id": issue,
        }));
    }

    let mapped = 1 + pending.variants.len() as u64 + mapped_types + mapped_dids;
    let total = mapped + unsupported_total;

    Ok(json!({
        "source_file_id": source_id,
        "ecu_id": ecu_id,
        "ecu": pending.ecu,
        "variants": variant_ids
            .iter()
            .copied()
            .map(Value::from)
            .collect::<Vec<Value>>(),
        "data_types_mapped": mapped_types,
        "dids_mapped": mapped_dids,
        "did_members_unresolved": unresolved_members,
        "coverage": {
            "mapped": mapped,
            "unsupported": unsupported_total,
            "failed": 0,
            "total_scanned_elements": total,
            "detail": unsupported,
        },
    }))
}

/// Record one imported DID: its source node, its payload members, and its
/// presence in every variant.
#[allow(clippy::too_many_arguments)]
fn import_did_detail(
    storage: &Storage,
    source_id: i64,
    did_id: i64,
    did: &cdd::CddDid,
    index: usize,
    variant_ids: &[i64],
    type_ids: &HashMap<String, i64>,
    unresolved_members: &mut u64,
) -> Result<(), EngineError> {
    storage.map_source_node(
        did_id,
        source_id,
        &format!("/CANDELA/ECUDOC/ECU/DIDS/DID[{index}]"),
        "DID",
    )?;
    for (position, member) in did.members.iter().enumerate() {
        // A member's `dtref` resolves against the document-local ids. It is
        // recorded either way: the text keeps the link to the source even when
        // the type it names is a composite this importer does not model.
        let resolved = type_ids.get(&member.data_type_ref).copied();
        if resolved.is_none() && !member.data_type_ref.is_empty() {
            *unresolved_members += 1;
        }
        storage.insert_did_member(
            did_id,
            position as i64,
            &member.name,
            &member.qualifier,
            &member.data_type_ref,
            resolved,
        )?;
    }
    // A CDD lists the same DID set on every variant, so each variant is linked to
    // the DID. This is what later lets the UI show "this variant hides that DID"
    // once a variant's DID subset is edited.
    for variant_id in variant_ids {
        storage.link_variant_did(*variant_id, did_id)?;
    }
    Ok(())
}


