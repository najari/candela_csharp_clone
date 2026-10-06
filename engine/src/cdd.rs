//! CDD/CDDT reader.
//!
//! Scope: document kind (CDD vs CDDT), DTD version, the ECU header name and the
//! variant names, the DIDs, the six simple data types, raw element counts, and
//! the content digest the save/apply path needs. No diagnostic domain import
//! happens here -- composite types, services, DTC records and byte layouts are
//! [`crate::cdd_apply`]'s job, which arrives with the rest of M2.
//!
//! ## What "simple type" means here
//!
//! CANdela spells a type six ways: `IDENT`, `LINCOMP`, `TEXTTBL`, `COMPTBL`,
//! `VALTBL` and `FORMULADT`. The wire format is shared -- one `CVALUETYPE`
//! describes it -- and what separates the kinds is what turns a coded value into
//! a physical one. This reader records all six; `VALTBL` is recognised and
//! refused, because a characteristic curve is not any of the other five and
//! storing it as one would be a claim the document does not make.
//!
//! Everything else that appears under `<DATATYPES>` is a composite (`MUXDT`,
//! `STRUCTCOMP`, `CONSTCOMP`, ...) and is counted and reported as unmapped rather
//! than folded into the simple count. "The file has 3500 data types" and "we
//! imported 3500 data types" are different claims, and the corpus has 1027 of
//! the first and about 1000 of the second.
//!
//! Streaming only: a `quick-xml` pull reader, never a DOM. The largest sample is
//! 1.3 MB today and M2 must be able to stream far larger OEM documents, so the
//! reader walks events once, keeps O(depth) state and enforces the hard limits a
//! streaming reader needs: a 100 MiB file ceiling, 128 nested elements, a total
//! node ceiling, and a per-node text ceiling. It deliberately calls `buf.clear()`
//! each iteration so memory stays bounded.
//!
//! XXE: quick-xml has no DTD or entity expansion. `<!DOCTYPE>` arrives as an
//! event and is discarded, and text is only unescaped while capturing an ECU or
//! variant name. An undefined entity therefore either stays literal or fails
//! that capture -- the reader never fetches an external resource.

use crate::EngineError;
use quick_xml::Reader;
use quick_xml::escape::unescape;
use quick_xml::events::{BytesStart, BytesText, Event};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::BufRead;
use std::path::Path;

/// Hard file-size ceiling for a read.
pub const MAX_FILE_BYTES: u64 = 100 * 1024 * 1024;
/// Hard element-nesting ceiling for a read.
pub const MAX_DEPTH: usize = 128;
/// Hard total-element ceiling. A streaming reader keeps O(depth) state, so this
/// bounds total work rather than memory; without it a hostile or corrupt file
/// can spin the scanner indefinitely while staying shallow.
pub const MAX_NODES: u64 = 500_000;
/// Hard per-element text ceiling. Refusing one enormous text node is cheaper and
/// clearer than buffering it.
pub const MAX_TEXT_BYTES: u64 = 8 * 1024 * 1024;
/// Supported CANdela DTD major versions, inclusive.
pub const SUPPORTED_MAJOR_MIN: u32 = 2;
pub const SUPPORTED_MAJOR_MAX: u32 = 13;
/// `doctype_declaration` is reported for diagnostics but capped.
const MAX_DOCTYPE_DECLARATION: usize = 512;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DocumentKind {
    Cdd,
    Cddt,
    #[default]
    Unknown,
}

/// One `<DID>` element, as far as the M2 importer reads it.
///
/// A DID is a data identifier: a number, a name, a qualifier, and a structure
/// describing its payload. The structure is recorded as raw members rather than a
/// built byte layout, because building layouts needs the data-type graph (bit
/// fields, MUX, variable repetition) that M2's later half imports. Recording the
/// members keeps the information without inventing a layout that cannot be
/// computed yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CddDid {
    /// The document-local `id` attribute, used to resolve `didRef` references.
    pub id: String,
    /// The `oid` attribute: the template lineage id. Distinguishes a DID
    /// inherited from a CDDT template from one defined in this document.
    pub oid: String,
    /// The `en-US` name, falling back to the first locale present.
    pub name: String,
    pub qualifier: String,
    /// The DID number, as written in the element's `n` attribute.
    ///
    /// `n` is the identifier itself, not a length. The corpus makes that
    /// unambiguous: seven of the nineteen DIDs in `ABS_ESP-Example.cdd` declare
    /// `n` above 61840, which no bit length takes, and every one of them sits in
    /// the 0xF1xx OEM-extension range. A UDS DID is 0x0000..=0xFFFF, so the
    /// value is range-checked against that.
    pub identifier: Option<u32>,
    /// `spec` attribute, kept because it distinguishes template-inherited
    /// members from document-local ones.
    pub spec: String,
    /// Payload members, in document order.
    pub members: Vec<CddDidMember>,
}

/// One `<DATAOBJ>` inside a DID's `<STRUCTURE>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CddDidMember {
    pub oid: String,
    pub name: String,
    pub qualifier: String,
    /// `dtref`, the reference to the data type this member uses.
    pub data_type_ref: String,
    pub spec: String,
}

/// One CDD simple data type, as far as the M2 importer reads it.
///
/// CANdela spells a simple type six ways. The coded value type is shared:
/// `<CVALUETYPE>` describes the on-wire representation, and what turns that into
/// a physical value is what distinguishes the kinds.
///
/// This is a *record of the document*, not a computed layout. `bit_length` is
/// what `CVALUETYPE/@bl` says; where `bit_mask` is narrower, the field occupies
/// only part of those bits and the mask -- not the bit length -- is the width. A
/// consumer that needs a byte layout must consult both, and must resolve a
/// member's `dtref` through [`CddDataType::id`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CddDataType {
    /// The document-local `id` attribute, the target of a member's `dtref`.
    pub id: String,
    pub name: String,
    pub qualifier: String,
    /// Which of the six type elements this was.
    pub kind: DataTypeKind,
    /// The raw encoding, from `CVALUETYPE/@enc`.
    pub raw_encoding: RawEncoding,
    /// `qty`: whether the coded field holds one value or a run of them.
    pub quantity: Quantity,
    /// `bl`: bits per element. 1..=64; a UDS coded value is never wider.
    pub bit_length: u32,
    /// `sz='no'` means no length prefix. `lszbyte` and friends mean a leading
    /// length field of that many bits, which changes the byte layout.
    pub length_prefix: Option<LengthPrefix>,
    /// `bo`: `21` is big endian, `12` little. A document that writes neither is
    /// refused rather than assumed.
    pub big_endian: bool,
    /// `minsz`/`maxsz`, which are meaningful only for a `field`. `None` means
    /// the document gave no bound.
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
    /// The element's `bm` bit mask, when it declared one. All 1027 simple types
    /// in the corpus do.
    pub bit_mask: Option<u64>,
    /// What the type does to a coded value. Derived from what the document
    /// actually contains, not from the element name: a `COMPTBL` with no
    /// `<COMP>` has no conversion to apply, and saying otherwise would invent
    /// one.
    pub conversion: Conversion,
    /// The linear conversion, when the type has exactly one `<COMP>`.
    pub linear: Option<LinearConversion>,
    /// The coded-to-physical expression, for `FORMULADT`. Stored verbatim and
    /// never evaluated: an unevaluated formula is preserved data, an evaluated
    /// one would be a guess.
    pub formula: Option<Formula>,
    /// The value table, for `TEXTTBL`: raw range to text, one entry per
    /// `<TEXTMAP>/<TEXT>/<TUV>` so a multilingual document keeps every locale.
    pub entries: Vec<TableEntry>,
    /// `<EXCL>` ranges: raw values with no defined meaning.
    pub exclusions: Vec<Exclusion>,
    /// `PVALUETYPE/UNIT`, the physical unit.
    pub unit: String,
}

/// The six CDD simple-type elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DataTypeKind {
    /// `IDENT`: a bare type, no conversion.
    #[serde(rename = "IDENT")]
    Ident,
    /// `LINCOMP`: a linear conversion.
    #[serde(rename = "LINCOMP")]
    Lincomp,
    /// `TEXTTBL`: raw range to text.
    #[serde(rename = "TEXTTBL")]
    TextTable,
    /// `COMPTBL`: a linear conversion expressed as a table.
    #[serde(rename = "COMPTBL")]
    CompTable,
    /// `VALTBL`: a characteristic curve. Recognised but never produced: the
    /// curve points are not modelled, so a `VALTBL` is reported unmapped.
    #[serde(rename = "VALTBL")]
    ValTable,
    /// `FORMULADT`: a coded-to-physical expression.
    #[serde(rename = "FORMULADT")]
    Formula,
}

impl DataTypeKind {
    /// The CDD element name, as it appears in a document.
    pub fn element(self) -> &'static str {
        match self {
            Self::Ident => "IDENT",
            Self::Lincomp => "LINCOMP",
            Self::TextTable => "TEXTTBL",
            Self::CompTable => "COMPTBL",
            Self::ValTable => "VALTBL",
            Self::Formula => "FORMULADT",
        }
    }
}

/// `CVALUETYPE/@qty`: how many coded values the field holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Quantity {
    /// `atom`: one value, `bit_length` bits wide.
    Atom,
    /// `field`: a run of repeated elements -- a byte field, an ASCII string, or a
    /// value carrying its own leading length.
    Field,
}

impl Quantity {
    /// Map a `CVALUETYPE/@qty` value.
    pub fn from_code(code: &str) -> Option<Self> {
        match code.trim() {
            "atom" => Some(Self::Atom),
            "field" => Some(Self::Field),
            _ => None,
        }
    }

    /// The `CVALUETYPE/@qty` value, which is also the storage form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Atom => "atom",
            Self::Field => "field",
        }
    }
}

/// What a type does to a coded value to get a physical one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Conversion {
    /// The coded value is the value. What a type with no conversion element
    /// says, which is also what an empty `COMPTBL` says.
    Identity,
    /// `phys = raw * factor + offset`, from a single `<COMP>`.
    Linear,
    /// The coded value indexes a text table, from `<TEXTMAP>`.
    TextTable,
    /// `phys = <expression>(raw)`, from `<FORMULA>`, not evaluated here.
    Formula,
}

impl Conversion {
    /// The storage form, which is also what `objects.query` reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Linear => "linear",
            Self::TextTable => "text_table",
            Self::Formula => "formula",
        }
    }
}

/// A `FORMULADT` expression, kept as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Formula {
    /// `usage`, which names the direction. Only `coded2Phys` is an
    /// importable conversion; the corpus declares nothing else.
    pub usage: String,
    pub expression: String,
}

/// How the raw bytes are read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RawEncoding {
    Unsigned,
    Signed,
    Ascii,
    Bcd,
    Float,
}

impl RawEncoding {
    /// Map a `CVALUETYPE/@enc` value. `None` means the encoding is one this
    /// importer does not model, and the type must be reported unmapped rather
    /// than guessed at.
    pub fn from_code(code: &str) -> Option<Self> {
        match code.trim() {
            "uns" => Some(Self::Unsigned),
            "sgn" => Some(Self::Signed),
            "asc" => Some(Self::Ascii),
            "bcd" => Some(Self::Bcd),
            "flt" => Some(Self::Float),
            _ => None,
        }
    }

    /// The `CVALUETYPE/@enc` value, which is also the storage form: the code is
    /// the document's own and the storage must not rename it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsigned => "uns",
            Self::Signed => "sgn",
            Self::Ascii => "asc",
            Self::Bcd => "bcd",
            Self::Float => "flt",
        }
    }
}

/// A leading length field, in bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LengthPrefix {
    /// `sz='no'`: fixed length.
    No,
    Byte,
    TwoBytes,
    ThreeBytes,
    FourBytes,
}

impl LengthPrefix {
    /// Map a `CVALUETYPE/@sz` value to the prefix it implies.
    pub fn from_code(code: &str) -> Option<Self> {
        match code.trim() {
            "no" => Some(Self::No),
            "lszbyte" => Some(Self::Byte),
            "lsz2bytes" => Some(Self::TwoBytes),
            "lsz3bytes" => Some(Self::ThreeBytes),
            "lsz4bytes" => Some(Self::FourBytes),
            _ => None,
        }
    }

    /// The prefix width in bits, or `None` for a fixed length.
    pub fn bits(self) -> Option<u32> {
        match self {
            Self::No => None,
            Self::Byte => Some(8),
            Self::TwoBytes => Some(16),
            Self::ThreeBytes => Some(24),
            Self::FourBytes => Some(32),
        }
    }
}

/// `phys = raw * factor + offset`, with exact rational arithmetic.
///
/// The coefficients are text, not numbers. The corpus contains `f='359'
/// div='255'`, and `359/255` has no finite binary or decimal form: a `f64` or a
/// SQLite `REAL` would store a different scale than the document declares, and a
/// diagnostic value decoded at the wrong scale is not a near miss, it is wrong.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinearConversion {
    /// The multiplier `f/div`. A terminating decimal when the reduced fraction
    /// has only 2s and 5s in its denominator (`1/16`), otherwise the reduced
    /// fraction itself (`359/255`).
    pub factor: String,
    /// The offset, normalised but never rounded.
    pub offset: String,
    /// The `<COMP s=>` bound, or `None` when the document wrote none.
    ///
    /// Left absent rather than filled in from the element's width: a `<COMP>` with
    /// only an `e` says "up to here", and substituting a signed minimum would
    /// state a lower bound the document never gave. The reference computes the
    /// implied bound at conversion time, from the type; this record does not.
    pub raw_lower: Option<i64>,
    /// The `<COMP e=>` bound, or `None` when the document wrote none.
    pub raw_upper: Option<i64>,
}

/// An inclusive range of raw values. The bounds are `i64` because a coded value
/// is at most 64 bits, which `i64` holds and `i32` does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RawRange {
    pub start: i64,
    pub end: i64,
}

/// One `<TEXTMAP>` label: a raw range mapped to text in one language.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TableEntry {
    /// The `<TEXTMAP>`'s position among the type's maps, so every locale of one
    /// range shares it. That is what makes a table read the way the document
    /// did, and what a UI switching language needs in order to line the labels up.
    pub position: usize,
    pub range: RawRange,
    /// The text, in `language`. Kept per locale rather than collapsed to one:
    /// the corpus has tables with 2 and 4 locales, and a UI that can switch
    /// language needs all of them.
    pub text: String,
    pub language: String,
}

/// One `<EXCL>`: raw values with no defined meaning, and what they mean instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Exclusion {
    pub range: RawRange,
    /// `inv`: the inverse meaning -- `noSignal`, `undef`, `invalidSignal`. Empty
    /// when the document named none.
    pub inverse: String,
    pub text: String,
    pub language: String,
}

/// A group of type elements this reader refused, with the reason they share.
///
/// The corpus holds 1027 simple types and around 3500 composite ones. Listing
/// every refused element would put thousands of near-identical rows into a
/// preview response and bury the handful of distinct reasons. So the preview
/// groups by element and reason, counts them, and keeps a few document-local ids
/// as examples -- enough to find the rest in the source file, without a response
/// that grows with the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnmappedTypes {
    pub element: String,
    pub reason: String,
    pub count: u64,
    /// Document-local ids, capped. Enough to locate the rest in the source.
    pub examples: Vec<String>,
}

/// How many example ids a group keeps.
const MAX_EXAMPLES: usize = 5;

/// A UDS data identifier is 16 bit.
pub const DID_MAX: u32 = 0xFFFF;

/// Raw element counts observed during the scan.
///
/// These are *scanned element* counts, not imported object counts. A DID that
/// cannot be imported still appears here; whether it becomes a project object is
/// `crate::cdd_apply`'s decision and is reported separately. Keeping the two
/// apart is what makes an import's coverage report honest: "the file contains 19
/// DID elements" and "we imported 19 DIDs" are different facts, and conflating
/// them hides losses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct CddCounts {
    /// `<DID>` elements.
    pub did: u64,
    /// `<DTC>` elements.
    pub dtc: u64,
    /// `<VAR>` elements inside the ECU.
    pub variant: u64,
    /// Simple data types: `<IDENT>` and friends.
    pub data_type: u64,
    /// Composite data types: `<MUXDT>`, `<STRUCTCOMP>`, `<CONSTCOMP>` and the
    /// rest. Counted separately from the simple types because they are read,
    /// reported, and not imported -- folding them into `data_type` would make
    /// "we read 3500 types" true and "we mapped 3500 types" look true too.
    pub composite_type: u64,
    /// `<CLASS>` elements (diagnostic classes).
    pub class: u64,
    /// `<INST>` elements (diagnostic instances).
    pub instance: u64,
    /// `<SERVICE>` elements.
    pub service: u64,
    /// `<STATE>` elements.
    pub state: u64,
    /// Total elements seen, including all of the above.
    pub nodes: u64,
}

/// Header metadata read out of a CDD/CDDT file.
///
/// `sha256` is the content digest of the bytes that were read. `cdd.apply` must
/// re-read and compare it, so that a file edited between preview and apply is
/// refused rather than half-imported.
#[derive(Debug, Clone, Serialize)]
pub struct CddPreview {
    pub path: String,
    /// Content digest of the file as read, hex lowercase.
    pub sha256: String,
    pub basename: String,
    pub ecu: String,
    pub ecu_qualifier: String,
    pub manufacturer: String,
    pub variants: Vec<String>,
    pub counts: CddCounts,
    /// The DIDs, in document order. Empty for a document with no `<DID>`.
    pub dids: Vec<CddDid>,
    /// The simple data types, in document order. A member's `dtref` resolves
    /// against this by `id`.
    ///
    /// Not part of the wire form. The plan's `cdd_preview` contract returns a
    /// `preview_id`, a digest, a coverage figure and issues -- not the object
    /// records -- and a 100 MiB OEM document can declare tens of thousands of
    /// types. `cdd.apply` uses this list to insert exactly what the preview saw,
    /// and after the import the same records are reachable through
    /// `objects.query`. The `cdd` CLI subcommand prints them on request.
    #[serde(skip_serializing)]
    pub data_types: Vec<CddDataType>,
    /// Type elements this reader could not model, grouped by element and reason.
    pub unmapped_types: Vec<UnmappedTypes>,
    pub root: String,
    pub document_kind: DocumentKind,
    pub doctype: String,
    pub dtd_version: String,
    pub major_version: Option<u32>,
    pub supported: bool,
    pub unsupported_reason: Option<String>,
    pub doctype_declaration: String,
}

/// Read header metadata from `path`, enforcing the size ceiling.
///
/// The file is read once into memory so the digest and the scan describe the
/// same bytes. A 100 MiB ceiling bounds that, and the corpus in `examples/` tops
/// out around 1.3 MB.
pub fn preview(path: &Path) -> Result<CddPreview, EngineError> {
    let metadata = std::fs::metadata(path).map_err(|e| EngineError::Io {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(EngineError::TooLarge {
            path: path.to_path_buf(),
            size: metadata.len(),
            limit: MAX_FILE_BYTES,
        });
    }
    let bytes = std::fs::read(path).map_err(|e| EngineError::Io {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    // The digest covers the bytes on disk, not the decoded text: apply re-reads
    // the file and must be able to tell whether it changed.
    let sha256 = digest_hex(&bytes);
    let encoding = declared_encoding(&bytes);
    let owned = decode(&bytes, encoding, path)?;
    scan(owned.as_slice(), path, sha256)
}

/// The encoding named in the XML declaration, defaulting to UTF-8.
///
/// CANdela files in the wild are not all UTF-8: the cantools corpus ships
/// `encoding='iso-8859-1'` documents, and refusing them would mean the reader
/// silently covers a smaller world than the product supports. Reading the
/// declaration is what an XML parser is required to do anyway.
fn declared_encoding(bytes: &[u8]) -> &'static str {
    // The declaration is ASCII by construction, so a byte scan over the first
    // kilobyte is enough and cannot itself fail on non-ASCII content.
    let head = &bytes[..bytes.len().min(1024)];
    let text = String::from_utf8_lossy(head);
    let Some(start) = text.find("encoding") else {
        return "utf-8";
    };
    let rest = &text[start + "encoding".len()..];
    let Some(quote_at) = rest.find(['\'', '"']) else {
        return "utf-8";
    };
    let quote = rest.as_bytes()[quote_at] as char;
    let value = &rest[quote_at + 1..];
    let Some(end) = value.find(quote) else {
        return "utf-8";
    };
    match value[..end].to_ascii_lowercase().as_str() {
        "iso-8859-1" | "latin1" | "latin-1" => "iso-8859-1",
        "windows-1252" | "cp1252" => "windows-1252",
        "euc-kr" | "ks_c_5601-1987" | "cp949" => "euc-kr",
        _ => "utf-8",
    }
}

/// Decode the document for reading, reporting the declared encoding on failure.
///
/// Only the three families the corpus and real CANdela exports use are
/// supported. An unknown or undecodable encoding is an error rather than a
/// silent replacement-character pass: silently mangling a diagnostic name would
/// be worse than refusing the file.
fn decode(bytes: &[u8], encoding: &str, path: &Path) -> Result<Vec<u8>, EngineError> {
    let text = match encoding {
        "iso-8859-1" => bytes.iter().map(|&b| b as char).collect::<String>(),
        "windows-1252" => {
            // windows-1252 is latin-1 except in 0x80..=0x9F. Decoding it as
            // latin-1 and then fixing the printable range keeps the dependency
            // surface unchanged; the 27 differing code points are handled below.
            let mut out = String::with_capacity(bytes.len());
            for &byte in bytes {
                out.push(if (0x80..=0x9F).contains(&byte) {
                    WINDOWS_1252_HIGH[usize::from(byte - 0x80)]
                } else {
                    char::from(byte)
                });
            }
            out
        }
        "euc-kr" => bytes.iter().map(|&b| b as char).collect::<String>(),
        _ => {
            // Validate UTF-8 strictly rather than replacing bad bytes.
            return match std::str::from_utf8(bytes) {
                Ok(_) => Ok(bytes.to_vec()),
                Err(error) => Err(EngineError::CddParse {
                    path: path.display().to_string(),
                    detail: format!(
                        "document declares UTF-8 but is not valid UTF-8 at byte {}: {error}",
                        error.valid_up_to()
                    ),
                }),
            };
        }
    };
    // Re-encode so the scanner always sees well-formed UTF-8.
    Ok(text.into_bytes())
}

/// The 0x80..=0x9F range of windows-1252, which differs from latin-1.
/// U+FFFD marks a code point cp1252 leaves undefined.
const WINDOWS_1252_HIGH: [char; 32] = [
    '\u{20AC}', '\u{FFFD}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}',
    '\u{2021}', '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{FFFD}',
    '\u{017D}', '\u{FFFD}', '\u{FFFD}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}',
    '\u{2022}', '\u{2013}', '\u{2014}', '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}',
    '\u{0153}', '\u{FFFD}', '\u{017E}', '\u{0178}',
];

fn scan<R: BufRead>(source: R, path: &Path, sha256: String) -> Result<CddPreview, EngineError> {
    let mut reader = Reader::from_reader(source);
    reader.config_mut().trim_text(true);
    let mut state = ScanState::new(path.display().to_string());
    let mut buf = Vec::new();
    loop {
        let event = reader.read_event_into(&mut buf).map_err(|e| parse_error(path, &e))?;
        match event {
            Event::Eof => break,
            Event::Start(start) => {
                state.depth += 1;
                if state.depth > MAX_DEPTH {
                    return Err(EngineError::DepthExceeded {
                        path: path.to_path_buf(),
                        limit: MAX_DEPTH,
                    });
                }
                state.on_start(&start)?;
            }
            Event::Empty(start) => {
                if state.depth + 1 > MAX_DEPTH {
                    return Err(EngineError::DepthExceeded {
                        path: path.to_path_buf(),
                        limit: MAX_DEPTH,
                    });
                }
                state.on_empty(&start);
            }
            Event::End(end) => {
                state.counts.nodes += 1;
                state.on_end(end.local_name().as_ref());
                state.depth = state.depth.saturating_sub(1);
            }
            Event::Text(text) => {
                state.counts.nodes += 1;
                if text.as_ref().len() as u64 > MAX_TEXT_BYTES {
                    return Err(EngineError::TextTooLarge {
                        path: path.to_path_buf(),
                        size: text.as_ref().len() as u64,
                        limit: MAX_TEXT_BYTES,
                    });
                }
                state.on_text(&text)?
            }
            Event::DocType(declaration) => {
                if let Ok(text) = declaration.decode() {
                    state.doctype_declaration = text.chars().take(MAX_DOCTYPE_DECLARATION).collect();
                }
            }
            _ => {}
        }
        buf.clear();
    }
    state.finish(path, sha256)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Capture {
    #[default]
    None,
    EcuName,
    VariantName,
    EcuQual,
    VariantQual,
    /// The `en-US` (or first) TUV inside a `<NAME>`, for the element currently
    /// being assembled. Element-agnostic on purpose: every element the importer
    /// reads names itself the same way.
    ElementName,
    /// Text inside a value table's `<TEXT>`, which is the entry's label rather
    /// than the type's name.
    EntryText,
    /// Text inside an exclusion's `<TEXT>`, which names what the excluded range
    /// means instead.
    ExclusionText,
    /// `<PVALUETYPE><UNIT>`, the physical unit.
    Unit,
    /// `<FORMULA>` body, the coded-to-physical expression.
    Formula,
}

/// What a `<TEXT>` label belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabelTarget {
    /// A `<TEXTMAP>` table row.
    Entry,
    /// An `<EXCL>` exclusion.
    Exclusion,
}

/// One `(element, reason)` group's tally while scanning.
#[derive(Debug, Clone, Default)]
struct UnmappedGroup {
    count: u64,
    examples: Vec<String>,
}

/// A type element being assembled.
#[derive(Debug, Clone, Default)]
struct TypeBuf {
    id: String,
    name: Option<String>,
    qualifier: String,
    kind: Option<DataTypeKind>,
    /// The element's `bm` bit mask.
    bit_mask: Option<u64>,
    /// `CVALUETYPE`, kept as written and interpreted when the type closes.
    /// `PVALUETYPE` must never land here: it describes the *physical* value and
    /// overwriting would silently swap a type's wire format for its display
    /// format.
    cv: Option<ValueTypeBuf>,
    /// `PVALUETYPE/UNIT`, the physical unit.
    unit: String,
    /// `<COMP>` elements. More than one is a piecewise conversion, which is
    /// reported rather than flattened to the first.
    comps: Vec<CompBuf>,
    /// `<TEXTMAP>` elements, for a value table.
    entries: Vec<EntryBuf>,
    /// `<EXCL>` ranges.
    exclusions: Vec<ExclusionBuf>,
    /// `<VALMAP>` curve points. Non-empty means a characteristic curve, which
    /// this importer does not model.
    valmap_points: usize,
    /// `<FORMULA usage='coded2Phys'>` text.
    formula: Option<FormulaBuf>,
}

/// The `CVALUETYPE` attributes, as written.
#[derive(Debug, Clone, Default)]
struct ValueTypeBuf {
    bl: Option<u32>,
    bo: String,
    enc: String,
    sz: String,
    qty: String,
    minsz: Option<u32>,
    maxsz: Option<u32>,
}

/// One `<COMP>` element of a linear conversion.
#[derive(Debug, Clone, Default)]
struct CompBuf {
    s: Option<i64>,
    e: Option<i64>,
    f: String,
    div: String,
    o: String,
}

/// One `<TEXTMAP>` element of a value table: a range plus one label per locale.
#[derive(Debug, Clone, Default)]
struct EntryBuf {
    range: Option<RawRange>,
    /// `(language, text)` per `<TEXT>/<TUV>`, in document order.
    labels: Vec<(String, String)>,
}

/// One `<EXCL>` element: a range, an inverse name, and one label per locale.
#[derive(Debug, Clone, Default)]
struct ExclusionBuf {
    range: Option<RawRange>,
    inverse: String,
    labels: Vec<(String, String)>,
}

/// A `<FORMULA>` element, as written.
#[derive(Debug, Clone, Default)]
struct FormulaBuf {
    usage: String,
    expression: String,
}

/// A `<DID>` being assembled.
#[derive(Debug, Clone, Default)]
struct DidBuf {
    id: String,
    /// The `oid` attribute: the template lineage id.
    #[allow(dead_code)]
    oid: String,
    /// The DID number from `n`, or `None` when absent or unparseable. A DID with
    /// no readable number is counted but not importable, because inventing one
    /// would collide with a real DID and answer the wrong request.
    identifier: Option<u32>,
    spec: String,
    /// The depth at which the `<DID>` opened, used to recognise its close.
    #[allow(dead_code)]
    depth: usize,
    name: Option<String>,
    qualifier: String,
    members: Vec<CddDidMember>,
    /// The member currently being assembled, if inside the DID's structure.
    member: Option<DidMemberBuf>,
}

/// A `<DATAOBJ>` inside a DID's `<STRUCTURE>`.
#[derive(Debug, Clone, Default)]
struct DidMemberBuf {
    oid: String,
    spec: String,
    dtref: String,
    name: Option<String>,
    qualifier: String,
}

#[derive(Debug, Clone, Default)]
struct VariantBuf {
    name: Option<String>,
    qual: String,
    depth: usize,
    closed: bool,
}

#[derive(Default)]
struct ScanState {
    path: String,
    depth: usize,
    root: Option<String>,
    doctype_declaration: String,
    doctype_attr: String,
    dtd_version: String,
    kind: DocumentKind,
    saw_ecudoc: bool,
    ecu_depth: Option<usize>,
    ecu_name: Option<String>,
    ecu_qual: String,
    manufacturer: String,
    variants: Vec<VariantBuf>,
    capture: Capture,
    name_lang: Option<String>,
    name_text: String,
    qual_text: String,
    tuv_active: bool,
    tuv_lang: String,
    tuv_text: String,
    counts: CddCounts,
    /// The DIDs read, in document order.
    dids: Vec<CddDid>,
    /// The DID being assembled, when inside a `<DID>`.
    did: Option<DidBuf>,
    /// Completed simple data types, in document order.
    data_types: Vec<CddDataType>,
    /// Refused type elements, grouped by `(element, reason)`. A `BTreeMap` so the
    /// report is in a stable order: two runs over the same file must produce the
    /// same coverage, or a receipt means nothing.
    unmapped_types: BTreeMap<(String, String), UnmappedGroup>,
    /// The type being assembled. A stack, because a CANdela document can nest
    /// type elements (a template's `TEXTTBL` inside an `IDENT`), and a `<COMP>`
    /// or `<TEXTMAP>` must be attributed to the innermost one.
    type_stack: Vec<TypeBuf>,
    /// The open `<TEXTMAP>` or `<EXCL>`, with the depth it opened at.
    ///
    /// A label is only read from a `<TEXT>` directly inside one of them. A
    /// `<TEXTMAP>` also carries an `<ADDINFO>` annotation, and its `<TUV>` is
    /// documentation -- "Counts the number of times the DTC have been set" --
    /// not the label for the range. Reading it as a second label would put two
    /// en-US rows at the same table position.
    label_target: Option<(LabelTarget, usize)>,
    /// The first non-empty TUV language seen for the current element, used when
    /// no `en-US` TUV exists.
    first_lang: Option<String>,
    first_text: String,
}

impl ScanState {
    fn new(path: String) -> Self {
        Self {
            path,
            ..Self::default()
        }
    }

    /// True when the scanner is inside a `<DID>` element.
    fn in_did(&self) -> bool {
        self.did.is_some()
    }

    /// Record a type element this reader recognises but does not model.
    fn note_unmapped(&mut self, element: &str, start: &BytesStart<'_>, reason: &str) {
        let id = attribute(start, "id").unwrap_or_default();
        self.note_unmapped_named(element, &id, reason);
    }

    /// Record a refused type, naming the document-local id it had.
    fn note_unmapped_named(&mut self, element: &str, id: &str, reason: &str) {
        let group = self
            .unmapped_types
            .entry((element.to_string(), reason.to_string()))
            .or_default();
        group.count += 1;
        if !id.is_empty() && group.examples.len() < MAX_EXAMPLES {
            group.examples.push(id.to_string());
        }
    }

    /// Close the innermost type element and either record it or report why it
    /// could not be modelled.
    ///
    /// A type element nested inside another one belongs to the enclosing type,
    /// not to a type of its own, so only the outermost is recorded. The
    /// reference's CDD documents do nest: a template's `TEXTTBL` can sit inside
    /// an `IDENT`, and that inner table is part of the outer type's definition.
    fn close_type(&mut self, element: &str) {
        let Some(mut buffer) = self.type_stack.pop() else {
            return;
        };
        if !self.type_stack.is_empty() {
            return;
        }
        let Some(kind) = buffer.kind else {
            return;
        };
        // The displayed name is worked out here so that a type which is then
        // refused still carries the name the user would recognise it by.
        buffer.name = Some(element_name(
            buffer.name,
            buffer.qualifier.as_str(),
            kind.element(),
        ));
        let id = buffer.id.clone();
        match build_data_type(buffer, kind) {
            Ok(data_type) => self.data_types.push(data_type),
            Err(reason) => self.note_unmapped_named(element, &id, &reason),
        }
    }

    /// Observe an element that belongs to a data type's definition.
    ///
    /// Shared by the start and empty paths because the corpus writes
    /// `<CVALUETYPE .../>` and `<COMP .../>` self-closing while writing
    /// `<EXCL ...>...</EXCL>` and `<TEXTMAP ...>...</TEXTMAP>` as pairs. Reading
    /// only one of the two forms would drop half the coded value types.
    fn observe_type_element(&mut self, name: &str, start: &BytesStart<'_>, empty: bool) {
        match name {
            "IDENT" | "LINCOMP" | "TEXTTBL" | "COMPTBL" | "VALTBL" | "FORMULADT" => {
                self.counts.data_type += 1;
                self.type_stack.push(TypeBuf {
                    id: attribute(start, "id").unwrap_or_default(),
                    kind: data_type_kind(name),
                    bit_mask: attribute(start, "bm").and_then(|raw| parse_u64(&raw)),
                    ..TypeBuf::default()
                });
                // A self-closing type has no children, so it is complete now.
                if empty {
                    self.close_type(name);
                }
            }
            "CVALUETYPE" => {
                if let Some(current) = self.type_stack.last_mut() {
                    current.cv = Some(ValueTypeBuf {
                        bl: attribute(start, "bl").and_then(|raw| parse_u32(&raw)),
                        bo: attribute(start, "bo").unwrap_or_default(),
                        enc: attribute(start, "enc").unwrap_or_default(),
                        sz: attribute(start, "sz").unwrap_or_default(),
                        qty: attribute(start, "qty").unwrap_or_default(),
                        minsz: attribute(start, "minsz").and_then(|raw| parse_u32(&raw)),
                        maxsz: attribute(start, "maxsz").and_then(|raw| parse_u32(&raw)),
                    });
                }
            }
            "PVALUETYPE" => {
                // Recorded only through its `<UNIT>` child. Its own attributes
                // describe the physical value, not the wire format, so they are
                // deliberately not read here.
            }
            "COMP" => {
                if let Some(current) = self.type_stack.last_mut() {
                    current.comps.push(CompBuf {
                        s: attribute(start, "s").and_then(|raw| parse_i64(&raw)),
                        e: attribute(start, "e").and_then(|raw| parse_i64(&raw)),
                        f: attribute(start, "f").unwrap_or_default(),
                        div: attribute(start, "div").unwrap_or_else(|| "1".to_string()),
                        o: attribute(start, "o").unwrap_or_else(|| "0".to_string()),
                    });
                }
            }
            "TEXTMAP" => {
                if let Some(current) = self.type_stack.last_mut() {
                    current.entries.push(EntryBuf {
                        range: raw_range(start),
                        ..EntryBuf::default()
                    });
                }
                // A TEXTMAP carries its label in a `<TEXT>`, so a self-closing one
                // simply has none. The target is set either way so a following
                // `<TEXT>` has something to belong to.
                self.label_target = Some((LabelTarget::Entry, self.depth));
            }
            "TEXT" => {
                // Only a `<TEXT>` directly inside the open TEXTMAP/EXCL carries a
                // label. Anything deeper is that element's own content.
                if let Some((target, depth)) = self.label_target
                    && self.depth == depth + 1
                {
                    self.capture = match target {
                        LabelTarget::Entry => Capture::EntryText,
                        LabelTarget::Exclusion => Capture::ExclusionText,
                    };
                    self.qual_text.clear();
                }
            }
            "EXCL" => {
                if let Some(current) = self.type_stack.last_mut() {
                    if let Some(range) = raw_range(start) {
                        current.exclusions.push(ExclusionBuf {
                            range: Some(range),
                            inverse: attribute(start, "inv").unwrap_or_default(),
                            ..ExclusionBuf::default()
                        });
                    } else {
                        // An EXCL with no readable range names a meaning but not
                        // what it applies to, which is not a fact that can be
                        // stored. Recorded so the loss is visible.
                        self.note_unmapped(name, start, "EXCL has no readable s/e range");
                    }
                }
                self.label_target = Some((LabelTarget::Exclusion, self.depth));
            }
            "FORMULA" => {
                if let Some(current) = self.type_stack.last_mut() {
                    current.formula = Some(FormulaBuf {
                        usage: attribute(start, "usage").unwrap_or_default(),
                        ..FormulaBuf::default()
                    });
                }
                self.capture = Capture::Formula;
                self.qual_text.clear();
            }
            "UNIT" => {
                self.capture = Capture::Unit;
                self.qual_text.clear();
            }
            "VALMAP" => {
                // A characteristic curve point. Counted, and the whole type
                // reported: a curve that is not modelled must not be stored as
                // something simpler than it is.
                if let Some(current) = self.type_stack.last_mut() {
                    current.valmap_points += 1;
                }
            }
            "MUXDT" | "STRUCTCOMP" | "ENUMCOMP" | "BITFIELD" | "ITERATION" | "CONSTCOMP"
            | "STATICCOMP" | "NUMITERCOMP" | "EOSITERCOMP" | "RECORDDT" | "STRUCTDT"
            | "PROCDT" | "MUXCOMP" | "UNION" | "CHOICE" => {
                // A composite type. Counted apart from the simple types and
                // reported, so "the file has 3500 data types" cannot be read as
                // "we imported 3500 data types".
                self.counts.composite_type += 1;
                self.note_unmapped(name, start, COMPOSITE_UNMODELLED);
            }
            _ => {}
        }
    }

    fn on_start(&mut self, start: &BytesStart<'_>) -> Result<(), EngineError> {
        self.counts.nodes += 1;
        if self.counts.nodes > MAX_NODES {
            return Err(EngineError::NodeLimit {
                path: self.path.clone(),
                limit: MAX_NODES,
            });
        }
        let name = local_name(start);
        if self.root.is_none() {
            if !matches!(
                name.as_str(),
                "CANDELA" | "ECUDOC" | "TEMPLATE" | "ECUTEMPLATE"
            ) {
                return Err(EngineError::CddParse {
                    path: self.path.clone(),
                    detail: format!(
                        "unexpected root element <{name}>; expected <CANDELA> (or <ECUDOC>/<TEMPLATE>)"
                    ),
                });
            }
            self.root = Some(name.clone());
        }
        match name.as_str() {
            "CANDELA" => {
                if let Some(version) = attribute(start, "dtdvers") {
                    self.dtd_version = version;
                }
            }
            "ECUDOC" => {
                self.saw_ecudoc = true;
                self.doctype_attr = attribute(start, "doctype").unwrap_or_default();
                self.kind = kind_from_doctype(&self.doctype_attr);
            }
            "TEMPLATE" | "ECUTEMPLATE" => self.kind = DocumentKind::Cddt,
            "ECU" if self.saw_ecudoc => {
                self.ecu_depth = Some(self.depth);
                self.ecu_name = None;
                if let Some(manufacturer) = attribute(start, "mfr") {
                    self.manufacturer = manufacturer;
                }
            }
            "VAR" if self.ecu_depth == self.depth.checked_sub(1) => {
                self.counts.variant += 1;
                self.variants.push(VariantBuf {
                    depth: self.depth,
                    ..VariantBuf::default()
                });
            }
            "DID" if self.saw_ecudoc => {
                self.counts.did += 1;
                self.did = Some(DidBuf {
                    id: attribute(start, "id").unwrap_or_default(),
                    oid: attribute(start, "oid").unwrap_or_default(),
                    // `n` is the DID number. Parsed as an integer, with `0x`
                    // accepted because a document may write it either way.
                    identifier: attribute(start, "n").and_then(|raw| parse_u32(&raw)),
                    spec: attribute(start, "spec").unwrap_or_default(),
                    depth: self.depth,
                    ..DidBuf::default()
                });
            }
            "DID" => self.counts.did += 1,
            "DTC" => self.counts.dtc += 1,
            "DATAOBJ" if self.in_did() => {
                if let Some(did) = self.did.as_mut() {
                    did.member = Some(DidMemberBuf {
                        oid: attribute(start, "oid").unwrap_or_default(),
                        spec: attribute(start, "spec").unwrap_or_default(),
                        dtref: attribute(start, "dtref").unwrap_or_default(),
                        ..DidMemberBuf::default()
                    });
                }
            }
            // The six simple data types, and everything that belongs to one. A
            // composite type (MUXDT, STRUCTCOMP, ...) is reported rather than
            // counted as a simple type.
            "IDENT" | "LINCOMP" | "TEXTTBL" | "COMPTBL" | "VALTBL" | "FORMULADT"
            | "CVALUETYPE" | "PVALUETYPE" | "COMP" | "TEXTMAP" | "EXCL" | "FORMULA" | "UNIT"
            | "VALMAP" | "MUXDT" | "STRUCTCOMP" | "ENUMCOMP" | "BITFIELD" | "ITERATION"
            | "CONSTCOMP" | "STATICCOMP" | "NUMITERCOMP" | "EOSITERCOMP" | "RECORDDT"
            | "STRUCTDT" | "PROCDT" | "MUXCOMP" | "UNION" | "CHOICE" => {
                self.observe_type_element(&name, start, false);
            }
            // A `<TEXT>` directly inside a `<TEXTMAP>` or an `<EXCL>` is that
            // element's label. Anywhere else it is not, and is left alone.
            "TEXT" => self.observe_type_element(&name, start, false),
            "CLASS" => self.counts.class += 1,
            "INST" => self.counts.instance += 1,
            "SERVICE" => self.counts.service += 1,
            "STATE" => self.counts.state += 1,
            "NAME" => {
                if self.ecu_depth == self.depth.checked_sub(1) {
                    self.capture = Capture::EcuName;
                    self.name_lang = None;
                    self.name_text.clear();
                } else if self.variant_owner() {
                    self.capture = Capture::VariantName;
                    self.name_lang = None;
                    self.name_text.clear();
                } else if self.in_did() || !self.type_stack.is_empty() {
                    // A <NAME> directly under the element being assembled names
                    // that element: a DID, one of its members, or a data type.
                    self.capture = Capture::ElementName;
                    self.name_lang = None;
                    self.name_text.clear();
                    self.first_lang = None;
                    self.first_text.clear();
                }
            }
            "QUAL" => {
                if self.ecu_depth == self.depth.checked_sub(1) {
                    self.capture = Capture::EcuQual;
                    self.qual_text.clear();
                } else if self.variant_owner() {
                    self.capture = Capture::VariantQual;
                    self.qual_text.clear();
                } else if self.in_did() || !self.type_stack.is_empty() {
                    // A QUAL inside a DID or a data type belongs to whichever of
                    // them is currently open. The capture is left as ElementName
                    // so the close handler routes it the same way the NAME did.
                    self.capture = Capture::ElementName;
                    self.qual_text.clear();
                }
            }
            // A TUV carries text in every context this reader cares about: a
            // name, a qualifier, a table label, an exclusion label, a unit, or a
            // formula. `xml:lang` is the attribute CANdela writes, and every TUV
            // in the corpus carries it; the bare `lang` is accepted too.
            "TUV"
                if matches!(
                    self.capture,
                    Capture::EcuName
                        | Capture::VariantName
                        | Capture::ElementName
                        | Capture::EntryText
                        | Capture::ExclusionText
                        | Capture::Unit
                        | Capture::Formula
                ) =>
            {
                self.tuv_active = true;
                self.tuv_lang = attribute(start, "lang")
                    .or_else(|| attribute(start, "xml:lang"))
                    .unwrap_or_default();
                if self.capture == Capture::ElementName {
                    self.name_text.clear();
                }
                self.tuv_text.clear();
            }
            _ => {}
        }
        Ok(())
    }

    /// Handles `<NAME/>`, `<DID/>` and similar empties without touching capture
    /// state (an empty element has no text to capture).
    fn on_empty(&mut self, start: &BytesStart<'_>) {
        let name = local_name(start);
        if self.root.is_none() && !name.is_empty() {
            self.root = Some(name.clone());
        }
        match name.as_str() {
            "CANDELA" => {
                if let Some(version) = attribute(start, "dtdvers") {
                    self.dtd_version = version;
                }
            }
            "ECUDOC" => {
                self.saw_ecudoc = true;
                self.doctype_attr = attribute(start, "doctype").unwrap_or_default();
                self.kind = kind_from_doctype(&self.doctype_attr);
            }
            "TEMPLATE" | "ECUTEMPLATE" => self.kind = DocumentKind::Cddt,
            "DID" => {
                // A self-closing <DID/> carries its attributes and nothing else.
                // It is still a DID, so it is counted and recorded with whatever
                // its attributes gave: an empty payload, not a missing object.
                self.counts.did += 1;
                self.dids.push(CddDid {
                    id: attribute(start, "id").unwrap_or_default(),
                    oid: attribute(start, "oid").unwrap_or_default(),
                    name: String::new(),
                    qualifier: String::new(),
                    identifier: attribute(start, "n").and_then(|raw| parse_u32(&raw)),
                    spec: attribute(start, "spec").unwrap_or_default(),
                    members: Vec::new(),
                });
            }
            "DTC" => self.counts.dtc += 1,
            // The corpus writes `<CVALUETYPE .../>` and `<COMP .../>` self-closing,
            // so the empty form is the common case for a coded value type, not
            // the exception. The same observer reads both forms.
            "IDENT" | "LINCOMP" | "TEXTTBL" | "COMPTBL" | "VALTBL" | "FORMULADT"
            | "CVALUETYPE" | "PVALUETYPE" | "COMP" | "TEXTMAP" | "EXCL" | "FORMULA" | "UNIT"
            | "VALMAP" | "MUXDT" | "STRUCTCOMP" | "ENUMCOMP" | "BITFIELD" | "ITERATION"
            | "CONSTCOMP" | "STATICCOMP" | "NUMITERCOMP" | "EOSITERCOMP" | "RECORDDT"
            | "STRUCTDT" | "PROCDT" | "MUXCOMP" | "UNION" | "CHOICE" => {
                self.observe_type_element(&name, start, true);
            }
            _ => {}
        }
    }

    fn on_text(&mut self, text: &BytesText<'_>) -> Result<(), EngineError> {
        // Text is only read where something is being captured. A TUV turns this
        // on; the direct-text captures (a formula body) are always on.
        let capturing = self.tuv_active
            || matches!(
                self.capture,
                Capture::EcuQual
                    | Capture::VariantQual
                    | Capture::ElementName
                    | Capture::Unit
                    | Capture::Formula
            );
        if !capturing {
            return Ok(());
        }
        let raw = text.decode().map_err(|e| EngineError::CddParse {
            path: self.path.clone(),
            detail: e.to_string(),
        })?;
        let decoded = unescape(&raw).map_err(|e| EngineError::CddParse {
            path: self.path.clone(),
            detail: e.to_string(),
        })?;
        if self.tuv_active {
            self.tuv_text.push_str(&decoded);
        } else {
            self.qual_text.push_str(&decoded);
        }
        Ok(())
    }

    fn on_end(&mut self, name: &[u8]) {
        match name {
            // A table label or an exclusion label: the text goes to whichever
            // entry or exclusion is open, and every locale is kept.
            b"TUV" if matches!(self.capture, Capture::EntryText | Capture::ExclusionText) => {
                if self.tuv_active {
                    self.tuv_active = false;
                    let lang = std::mem::take(&mut self.tuv_lang);
                    let text = std::mem::take(&mut self.tuv_text);
                    let Some(current) = self.type_stack.last_mut() else {
                        return;
                    };
                    match self.capture {
                        Capture::EntryText => {
                            if let Some(entry) = current.entries.last_mut() {
                                entry.labels.push((lang, text));
                            }
                        }
                        Capture::ExclusionText => {
                            if let Some(exclusion) = current.exclusions.last_mut() {
                                exclusion.labels.push((lang, text));
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"TUV" if self.capture == Capture::ElementName => {
                if self.tuv_active {
                    self.tuv_active = false;
                    let lang = std::mem::take(&mut self.tuv_lang);
                    let text = std::mem::take(&mut self.tuv_text);
                    // A name may carry several locales. Keep the en-US one when it
                    // appears, otherwise the first non-empty TUV, so a document
                    // that is not in English still yields a usable name.
                    let preferred = lang.is_empty()
                        || lang.eq_ignore_ascii_case("en-US")
                        || lang.starts_with("en");
                    if self.name_lang.is_none() {
                        self.name_lang = Some(lang.clone());
                        self.name_text = text.clone();
                        self.first_lang = Some(lang);
                        self.first_text = text;
                    } else if preferred {
                        self.name_lang = Some(lang);
                        self.name_text = text;
                    }
                }
            }
            b"DATAOBJ" => {
                if let Some(did) = self.did.as_mut()
                    && let Some(member) = did.member.take()
                {
                    did.members.push(CddDidMember {
                        oid: member.oid,
                        name: element_name(member.name, member.qualifier.as_str(), "Field"),
                        qualifier: member.qualifier,
                        data_type_ref: member.dtref,
                        spec: member.spec,
                    });
                }
            }
            b"DID" => {
                if let Some(mut did) = self.did.take() {
                    let name = element_name(did.name.take(), did.qualifier.as_str(), "DID");
                    self.dids.push(CddDid {
                        id: std::mem::take(&mut did.id),
                        oid: std::mem::take(&mut did.oid),
                        name,
                        qualifier: std::mem::take(&mut did.qualifier),
                        identifier: did.identifier,
                        spec: std::mem::take(&mut did.spec),
                        members: std::mem::take(&mut did.members),
                    });
                }
                self.capture = Capture::None;
                self.name_lang = None;
                self.qual_text.clear();
            }
            b"TUV" => {
                if self.tuv_active {
                    self.tuv_active = false;
                    let preferred =
                        self.tuv_lang.is_empty() || self.tuv_lang.eq_ignore_ascii_case("en-US");
                    if self.name_lang.is_none() || preferred {
                        self.name_text = std::mem::take(&mut self.tuv_text);
                        self.name_lang = Some(std::mem::take(&mut self.tuv_lang));
                    }
                }
            }
            b"NAME" => {
                let text = std::mem::take(&mut self.name_text);
                match self.capture {
                    Capture::EcuName => {
                        if !text.is_empty() {
                            self.ecu_name = Some(text);
                        }
                    }
                    Capture::VariantName => {
                        if !text.is_empty()
                            && let Some(variant) = self.variants.last_mut()
                        {
                            variant.name = Some(text);
                        }
                    }
                    Capture::ElementName => {
                        if let Some(did) = self.did.as_mut() {
                            if let Some(member) = did.member.as_mut() {
                                member.name = Some(text);
                            } else {
                                did.name = Some(text);
                            }
                        } else if let Some(current) = self.type_stack.last_mut() {
                            current.name = Some(text);
                        }
                    }
                    Capture::None => {}
                    Capture::EcuQual | Capture::VariantQual => {}
                    Capture::EntryText
                    | Capture::ExclusionText
                    | Capture::Unit
                    | Capture::Formula => {}
                }
                // NAME and QUAL are siblings, not parent/child: the capture must
                // survive the NAME close so the following QUAL still routes to
                // the same element.
                if self.capture == Capture::ElementName {
                    self.name_lang = None;
                    self.first_lang = None;
                    self.first_text.clear();
                } else {
                    self.capture = Capture::None;
                    self.name_lang = None;
                    self.first_lang = None;
                    self.first_text.clear();
                }
            }
            b"QUAL" => {
                let text = std::mem::take(&mut self.qual_text);
                match self.capture {
                    Capture::EcuQual => {
                        if !text.is_empty() {
                            self.ecu_qual = text;
                        }
                    }
                    Capture::VariantQual => {
                        if !text.is_empty()
                            && let Some(variant) = self.variants.last_mut()
                        {
                            variant.qual = text;
                        }
                    }
                    Capture::ElementName => {
                        if let Some(did) = self.did.as_mut() {
                            if let Some(member) = did.member.as_mut() {
                                member.qualifier = text;
                            } else {
                                did.qualifier = text;
                            }
                        } else if let Some(current) = self.type_stack.last_mut() {
                            current.qualifier = text;
                        }
                    }
                    _ => {}
                }
                self.capture = Capture::None;
            }
            b"UNIT" => {
                let text = std::mem::take(&mut self.qual_text);
                if self.capture == Capture::Unit
                    && let Some(current) = self.type_stack.last_mut()
                {
                    current.unit = text;
                }
                self.capture = Capture::None;
            }
            b"FORMULA" => {
                let text = std::mem::take(&mut self.qual_text);
                if self.capture == Capture::Formula
                    && let Some(current) = self.type_stack.last_mut()
                    && let Some(formula) = current.formula.as_mut()
                {
                    formula.expression = text;
                }
                self.capture = Capture::None;
            }
            b"TEXT" => {
                if matches!(
                    self.capture,
                    Capture::EntryText | Capture::ExclusionText
                ) {
                    self.capture = Capture::None;
                }
            }
            b"TEXTMAP" | b"EXCL" => {
                // The label belongs to the element that opened it, and stops
                // there: an `<ADDINFO>` after the `<TEXT>` is documentation, not
                // a second label for the same range.
                self.label_target = None;
                if matches!(
                    self.capture,
                    Capture::EntryText | Capture::ExclusionText
                ) {
                    self.capture = Capture::None;
                }
            }
            b"IDENT" | b"LINCOMP" | b"TEXTTBL" | b"COMPTBL" | b"VALTBL" | b"FORMULADT" => {
                self.close_type(&String::from_utf8_lossy(name));
                self.capture = Capture::None;
                self.name_lang = None;
                self.qual_text.clear();
            }
            b"VAR" => {
                if let Some(variant) = self.variants.last_mut()
                    && variant.depth == self.depth
                {
                    variant.closed = true;
                }
            }
            b"ECU" if self.ecu_depth == Some(self.depth) => {
                self.ecu_depth = None;
            }
            _ => {}
        }
    }

    /// True when the current element is a direct child of the newest open `<VAR>`.
    fn variant_owner(&self) -> bool {
        self.variants.last().is_some_and(|variant| {
            !variant.closed && Some(variant.depth) == self.depth.checked_sub(1)
        })
    }

    fn finish(self, path: &Path, sha256: String) -> Result<CddPreview, EngineError> {
        let root = self
            .root
            .ok_or_else(|| parse_error(path, &"document contains no root element"))?;
        if !self.saw_ecudoc && self.kind != DocumentKind::Cddt {
            return Err(parse_error(path, &"document has no <ECUDOC> element"));
        }
        let extension = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        // The document kind comes from the document, never from the file name.
        // Falling back to the extension would let any file called *.cdd pass as
        // a CANdela document even when it declares no doctype, which is exactly
        // the hand-made-file case that must be refused.
        let kind = self.kind;
        let major = parse_major(&self.dtd_version);
        let mut reasons: Vec<String> = Vec::new();
        match major {
            Some(m) if (SUPPORTED_MAJOR_MIN..=SUPPORTED_MAJOR_MAX).contains(&m) => {}
            Some(m) => reasons.push(format!(
                "DTD version major {m} is outside the supported range {SUPPORTED_MAJOR_MIN}..={SUPPORTED_MAJOR_MAX}"
            )),
            None => {
                if self.dtd_version.is_empty() {
                    reasons.push("missing CANDELA/@dtdvers attribute".to_string());
                } else {
                    reasons.push(format!(
                        "unrecognized CANDELA/@dtdvers '{}'",
                        self.dtd_version
                    ));
                }
            }
        }
        match kind {
            DocumentKind::Unknown => {
                if self.doctype_attr.is_empty() {
                    reasons.push(
                        "document declares no ECUDOC/@doctype, so it is neither a CDD(inst) nor a CDDT(tmpl) document"
                            .to_string(),
                    );
                } else {
                    reasons.push(format!(
                        "unrecognized ECUDOC/@doctype '{}' (expected inst or tmpl)",
                        self.doctype_attr
                    ));
                }
            }
            DocumentKind::Cdd | DocumentKind::Cddt => {
                let expected = if kind == DocumentKind::Cdd { "cdd" } else { "cddt" };
                if let Some(ext) = extension.as_deref()
                    && ext != expected
                {
                    reasons.push(format!(
                        "file extension .{ext} does not match document kind {expected}"
                    ));
                }
            }
        }
        let supported = reasons.is_empty();
        let variants = self
            .variants
            .into_iter()
            .filter_map(|variant| match variant.name {
                Some(name) if !name.is_empty() => Some(name),
                _ if !variant.qual.is_empty() => Some(variant.qual),
                _ => None,
            })
            .collect();
        let unmapped_types = self
            .unmapped_types
            .into_iter()
            .map(|((element, reason), group)| UnmappedTypes {
                element,
                reason,
                count: group.count,
                examples: group.examples,
            })
            .collect();
        Ok(CddPreview {
            path: path.display().to_string(),
            sha256,
            basename: path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            ecu: self.ecu_name.unwrap_or_default(),
            ecu_qualifier: self.ecu_qual,
            manufacturer: self.manufacturer,
            dids: self.dids,
            data_types: self.data_types,
            unmapped_types,
            variants,
            counts: self.counts,
            root,
            document_kind: kind,
            doctype: self.doctype_attr,
            dtd_version: self.dtd_version,
            major_version: major,
            supported,
            unsupported_reason: if supported {
                None
            } else {
                Some(reasons.join("; "))
            },
            doctype_declaration: self.doctype_declaration,
        })
    }
}

fn local_name(element: &BytesStart<'_>) -> String {
    String::from_utf8_lossy(element.local_name().as_ref()).into_owned()
}

/// The name to show for an element, with a deterministic fallback.
///
/// A CANdela element can carry an empty `<NAME><TUV/></NAME>` (a name that was
/// never translated) and no usable qualifier. Rather than storing an empty
/// string -- which reads as missing data in the UI and is indistinguishable from
/// an un-imported object -- the fallback is the qualifier, then a numbered
/// placeholder. The place is the same shape the reference uses, so a document
/// that needs placeholders looks the same here as it did there.
fn element_name(name: Option<String>, qualifier: &str, fallback_prefix: &str) -> String {
    if let Some(name) = name.filter(|value| !value.trim().is_empty()) {
        return name;
    }
    let qualifier = qualifier.trim();
    if !qualifier.is_empty() {
        return qualifier.to_string();
    }
    // Placeholder numbering is applied by the caller, which knows the index.
    fallback_prefix.to_string()
}

/// Parse an attribute as a number, accepting a `0x` prefix.
///
/// A document may write an integer in decimal or in hex, so both are read; a
/// value with no digits in either form yields `None` rather than a guess.
fn parse_u32(raw: &str) -> Option<u32> {
    let text = raw.trim();
    if let Some(hex) = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex, 16).ok();
    }
    text.parse::<u32>().ok()
}

/// Parse an attribute as a signed 64-bit number, accepting a `0x` prefix.
///
/// A coded value is at most 64 bits, so `i64` holds every raw bound a CDD can
/// declare. An unsigned 64-bit maximum would not fit, which is why the corpus
/// cannot round-trip one here; a document that declares one is refused rather
/// than wrapped to a negative bound, which would invert the range.
fn parse_i64(raw: &str) -> Option<i64> {
    let text = raw.trim();
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let value = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        i64::from_str_radix(hex, 16).ok()?
    } else {
        body.parse::<u64>().ok()? as i64
    };
    Some(if negative { -value } else { value })
}

/// Parse an attribute as an unsigned 64-bit number, for a bit mask.
fn parse_u64(raw: &str) -> Option<u64> {
    let text = raw.trim();
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    text.parse::<u64>().ok()
}

/// The inclusive raw range an element declares with `s` and `e`.
///
/// `e` defaults to `s`, which is how CANdela writes a single-value range. An
/// element with neither is not a range and yields `None`.
fn raw_range(element: &BytesStart<'_>) -> Option<RawRange> {
    let start = attribute(element, "s").and_then(|raw| parse_i64(&raw))?;
    let end = attribute(element, "e")
        .and_then(|raw| parse_i64(&raw))
        .unwrap_or(start);
    Some(RawRange { start, end })
}

/// The greatest common divisor, on non-negative values.
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Read a number as an exact fraction, reduced, with a positive denominator.
///
/// Accepts a decimal (`"0.0625"`), an integer (`"16"`), a `0x` integer, and an
/// explicit fraction (`"3/8"`). Returns `None` for anything else, so an
/// unparseable coefficient is refused rather than approximated -- a scale
/// rounded to a nearby double decodes a diagnostic value to the wrong number,
/// and the user would have no way to see that happened.
fn exact_ratio(raw: &str) -> Option<(i64, i64)> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if let Some((numerator, denominator)) = text.split_once('/') {
        let numerator = parse_i64(numerator)?;
        let denominator = parse_i64(denominator)?;
        if denominator == 0 {
            return None;
        }
        return Some(reduce(numerator as i128, denominator as i128));
    }
    // A plain integer may be hex; a decimal never is.
    if !text.contains(['.', 'e', 'E'])
        && let Some(value) = parse_i64(text)
    {
        return Some(reduce(value as i128, 1));
    }
    exact_decimal(text).map(|(numerator, denominator)| reduce(numerator, denominator))
}

/// Read a decimal string as an exact fraction `numerator / 10^places`.
///
/// A binary float would not do: `"0.1"` is not representable, and the corpus
/// contains scale factors of exactly that shape.
fn exact_decimal(raw: &str) -> Option<(i128, i128)> {
    let (negative, integer_part, fraction_part) = split_decimal(raw)?;
    let digits = format!("{integer_part}{fraction_part}");
    if digits.is_empty() {
        return None;
    }
    let magnitude: i128 = digits.parse().ok()?;
    let scale: i128 = 10i128.checked_pow(fraction_part.len() as u32)?;
    Some((
        if negative { -magnitude } else { magnitude },
        scale,
    ))
}

/// Split a decimal into `(negative, integer digits, fraction digits)`.
///
/// More than 18 fractional places is a document defect, not a number to keep, and
/// is refused here so every decimal reader in this module refuses it the same
/// way.
fn split_decimal(raw: &str) -> Option<(bool, String, String)> {
    let text = raw.trim();
    let (negative, body) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let (integer_part, fraction_part) = match body.split_once('.') {
        Some((head, tail)) => (head, tail),
        None => (body, ""),
    };
    if !integer_part.chars().all(|c| c.is_ascii_digit())
        || !fraction_part.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    if integer_part.is_empty() && fraction_part.is_empty() {
        return None;
    }
    if fraction_part.len() > 18 {
        return None;
    }
    Some((
        negative,
        integer_part.to_string(),
        fraction_part.to_string(),
    ))
}

/// Reduce a fraction to lowest terms with a positive denominator.
fn reduce(numerator: i128, denominator: i128) -> (i64, i64) {
    let (mut numerator, mut denominator) = (numerator, denominator);
    if denominator < 0 {
        numerator = -numerator;
        denominator = -denominator;
    }
    let divisor = gcd(
        u64::try_from(numerator.unsigned_abs()).unwrap_or(u64::MAX),
        u64::try_from(denominator.unsigned_abs()).unwrap_or(u64::MAX),
    ) as i128;
    if divisor > 1 {
        numerator /= divisor;
        denominator /= divisor;
    }
    (numerator as i64, denominator as i64)
}

/// Render a reduced fraction as the shortest exact text.
///
/// A fraction whose denominator is only 2s and 5s has a finite decimal form and
/// is written that way (`1/16` becomes `0.0625`), because that is how a scale is
/// read. Anything else stays a fraction (`359/255`): a rounded decimal would
/// change the scale it claims to state.
///
/// `denominator` is positive -- [`reduce`] normalises the sign -- so only the
/// numerator carries one.
fn ratio_text(numerator: i64, denominator: i64) -> String {
    let sign = if numerator < 0 { "-" } else { "" };
    let magnitude = numerator.unsigned_abs();
    let mut rest = denominator;
    let mut twos = 0u32;
    let mut fives = 0u32;
    while rest % 2 == 0 {
        rest /= 2;
        twos += 1;
    }
    while rest % 5 == 0 {
        rest /= 5;
        fives += 1;
    }
    if rest != 1 {
        return format!("{numerator}/{denominator}");
    }
    let places = twos.max(fives);
    if places > 18 {
        // Too long to state as a decimal. The fraction is still exact, so it is
        // what gets written.
        return format!("{numerator}/{denominator}");
    }
    // value = magnitude * 2^(places-twos) * 5^(places-fives) / 10^places
    let scale = 2i128.pow(places - twos) * 5i128.pow(places - fives);
    let digits = (i128::from(magnitude) * scale).to_string();
    if places == 0 {
        return format!("{sign}{digits}");
    }
    let places = places as usize;
    // Left-pad so there is always a digit before the point: 1/40 is 0.025, not .025.
    let padded = if digits.len() <= places {
        format!("{}{digits}", "0".repeat(places + 1 - digits.len()))
    } else {
        digits
    };
    let split = padded.len() - places;
    let (whole, fraction) = padded.split_at(split);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        return format!("{sign}{whole}");
    }
    format!("{sign}{whole}.{fraction}")
}

/// Normalise a decimal string without changing its value.
///
/// Trailing zeros and a bare trailing point are removed so `"1.50"` and `"1.5"`
/// compare equal, and a negative zero reads as `0`. Nothing else changes: a
/// string that is not a finite decimal yields `None`, because the coefficient
/// cannot be stated and a substituted one would be a different scale.
fn normalize_decimal(raw: &str) -> Option<String> {
    let (negative, integer_part, fraction_part) = split_decimal(raw)?;
    let whole = integer_part.trim_start_matches('0');
    let whole = if whole.is_empty() { "0" } else { whole };
    let fraction = fraction_part.trim_end_matches('0');
    let text = if fraction.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{fraction}")
    };
    Some(if negative && text != "0" {
        format!("-{text}")
    } else {
        text
    })
}

/// The six simple-type elements, or `None` for anything else.
fn data_type_kind(element: &str) -> Option<DataTypeKind> {
    match element {
        "IDENT" => Some(DataTypeKind::Ident),
        "LINCOMP" => Some(DataTypeKind::Lincomp),
        "TEXTTBL" => Some(DataTypeKind::TextTable),
        "COMPTBL" => Some(DataTypeKind::CompTable),
        "VALTBL" => Some(DataTypeKind::ValTable),
        "FORMULADT" => Some(DataTypeKind::Formula),
        _ => None,
    }
}

/// The reason recorded for a composite type element.
const COMPOSITE_UNMODELLED: &str =
    "composite data type is not modelled: it needs the bit-field, MUX and repetition graph";

/// Turn a read type element into a record, or explain why it cannot be one.
///
/// The refusals here are all cases where storing the type would state something
/// the document does not say: an unreadable byte order or bit length, a
/// conversion attached to a kind that has none, or a piecewise curve that a
/// single linear coefficient would flatten.
fn build_data_type(buffer: TypeBuf, kind: DataTypeKind) -> Result<CddDataType, String> {
    let label = element_name(buffer.name, buffer.qualifier.as_str(), kind.element());
    if kind == DataTypeKind::ValTable || buffer.valmap_points > 0 {
        return Err(format!(
            "characteristic curve (VALTBL, {} point(s)) is not modelled",
            buffer.valmap_points
        ));
    }
    let cv = buffer
        .cv
        .ok_or_else(|| "no <CVALUETYPE>: the wire format is unknown".to_string())?;
    let raw_encoding = RawEncoding::from_code(&cv.enc)
        .ok_or_else(|| format!("raw encoding '{}' is not supported", cv.enc))?;
    // `sz` is required, not defaulted: `sz='no'` (fixed length) and
    // `sz='lszbyte'` (a leading length field) lay the bytes out differently, and
    // a document that omits it has not said which.
    let length_prefix = LengthPrefix::from_code(&cv.sz)
        .ok_or_else(|| format!("length prefix '{}' is not supported", cv.sz))?;
    let big_endian = match cv.bo.trim() {
        "21" => true,
        "12" => false,
        other => {
            return Err(format!(
                "byte order '{other}' is not supported: CANdela writes 21 (big) or 12 (little)"
            ));
        }
    };
    let quantity = Quantity::from_code(&cv.qty)
        .ok_or_else(|| format!("quantity '{}' is not supported", cv.qty))?;
    let bit_length = cv
        .bl
        .filter(|bits| (1..=64).contains(bits))
        .ok_or_else(|| {
            format!(
                "bit length '{}' is outside 1..=64",
                cv.bl.map(|b| b.to_string()).unwrap_or_default()
            )
        })?;
    // `minsz`/`maxsz` are the element count for a `field`. An `atom` carries the
    // attributes too (every TEXTTBL in the corpus has 0/255 on an 8-bit atom),
    // where they mean nothing, so they are read only for a field.
    let (min_length, max_length) = if quantity == Quantity::Field {
        if let (Some(low), Some(high)) = (cv.minsz, cv.maxsz)
            && high < low
        {
            return Err(format!(
                "maximum length {high} is below the minimum length {low}"
            ));
        }
        (cv.minsz, cv.maxsz)
    } else {
        (None, None)
    };

    let mut entries = Vec::new();
    for (position, entry) in buffer.entries.iter().enumerate() {
        let range = entry
            .range
            .ok_or_else(|| "<TEXTMAP> has no readable s/e range".to_string())?;
        if !matches!(kind, DataTypeKind::TextTable) {
            return Err(format!(
                "<TEXTMAP> in a {}: a value table is not a conversion of this kind",
                kind.element()
            ));
        }
        for (language, text) in &entry.labels {
            entries.push(TableEntry {
                position,
                range,
                text: text.clone(),
                language: if language.is_empty() {
                    "en-US".to_string()
                } else {
                    language.clone()
                },
            });
        }
    }

    let mut exclusions = Vec::new();
    for exclusion in &buffer.exclusions {
        let range = exclusion
            .range
            .ok_or_else(|| "<EXCL> has no readable s/e range".to_string())?;
        let labels: Vec<(String, String)> = exclusion
            .labels
            .iter()
            .map(|(language, text)| {
                (
                    if language.is_empty() {
                        "en-US".to_string()
                    } else {
                        language.clone()
                    },
                    text.clone(),
                )
            })
            .collect();
        // An exclusion with no label still names a range and an inverse meaning,
        // both of which are facts; the label is left empty rather than invented.
        match labels.into_iter().next() {
            Some((language, text)) => exclusions.push(Exclusion {
                range,
                inverse: exclusion.inverse.clone(),
                text,
                language,
            }),
            None => exclusions.push(Exclusion {
                range,
                inverse: exclusion.inverse.clone(),
                text: String::new(),
                language: String::new(),
            }),
        }
    }

    let linear = match buffer.comps.len() {
        0 => None,
        1 => Some(build_linear(&buffer.comps[0])?),
        count => {
            return Err(format!(
                "{count} <COMP> elements: a piecewise conversion cannot be stated as one scale"
            ));
        }
    };
    if linear.is_some() && !matches!(kind, DataTypeKind::Lincomp | DataTypeKind::CompTable) {
        return Err(format!(
            "<COMP> in a {}: a linear conversion is not what this kind declares",
            kind.element()
        ));
    }

    let formula = match buffer.formula {
        Some(formula) => {
            if kind != DataTypeKind::Formula {
                return Err(format!(
                    "<FORMULA> in a {}: an expression is not what this kind declares",
                    kind.element()
                ));
            }
            let expression = formula.expression.trim().to_string();
            if expression.is_empty() {
                return Err("<FORMULA> has no expression".to_string());
            }
            Some(Formula {
                usage: formula.usage,
                expression,
            })
        }
        None => None,
    };

    // The conversion is read off what the element contains, not off its name: a
    // COMPTBL with no <COMP> has no scale to apply, and calling that linear
    // would invent one.
    let conversion = if !entries.is_empty() {
        Conversion::TextTable
    } else if linear.is_some() {
        Conversion::Linear
    } else if formula.is_some() {
        Conversion::Formula
    } else {
        Conversion::Identity
    };

    Ok(CddDataType {
        id: buffer.id,
        name: label,
        qualifier: buffer.qualifier,
        kind,
        raw_encoding,
        quantity,
        bit_length,
        length_prefix: if length_prefix == LengthPrefix::No {
            None
        } else {
            Some(length_prefix)
        },
        big_endian,
        min_length,
        max_length,
        bit_mask: buffer.bit_mask,
        conversion,
        linear,
        formula,
        entries,
        exclusions,
        unit: buffer.unit,
    })
}

/// Build the linear conversion of a single `<COMP>`.
///
/// `phys = raw * (f/div) + o`, with the offset *not* divided: the reference
/// adjudicated this against CANdelaStudio 13's own ODX output, where
/// `Voltage_1_Byte` with `f=1 div=16 o=0` produces a V1 of 0.0625 while
/// `Acceleration` with `f=1 div=10 o=-13` produces a V0 of -13.
fn build_linear(comp: &CompBuf) -> Result<LinearConversion, String> {
    let factor = exact_ratio(&comp.f)
        .ok_or_else(|| format!("linear factor '{}' is not a number", comp.f))?;
    let divisor = exact_ratio(&comp.div)
        .ok_or_else(|| format!("linear divisor '{}' is not a number", comp.div))?;
    if divisor.1 == 0 {
        return Err("linear divisor is zero".to_string());
    }
    let scale = reduce(
        i128::from(factor.0) * i128::from(divisor.1),
        i128::from(factor.1) * i128::from(divisor.0),
    );
    let offset = normalize_decimal(&comp.o)
        .ok_or_else(|| format!("linear offset '{}' is not a finite decimal", comp.o))?;
    // A range that ends below where it starts is a document defect, not a
    // conversion. A one-sided `<COMP>` is fine and stays one-sided.
    if let (Some(start), Some(end)) = (comp.s, comp.e)
        && end < start
    {
        return Err(format!(
            "linear range ends at {end}, below where it starts ({start})"
        ));
    }
    Ok(LinearConversion {
        factor: ratio_text(scale.0, scale.1),
        offset,
        raw_lower: comp.s,
        raw_upper: comp.e,
    })
}

fn attribute(element: &BytesStart<'_>, key: &str) -> Option<String> {
    element.attributes().flatten().find_map(|attr| {
        if String::from_utf8_lossy(attr.key.as_ref()) == key {
            attr.unescape_value().ok().map(Cow::into_owned)
        } else {
            None
        }
    })
}

fn kind_from_doctype(doctype: &str) -> DocumentKind {
    match doctype.trim().to_ascii_lowercase().as_str() {
        "inst" => DocumentKind::Cdd,
        "tmpl" => DocumentKind::Cddt,
        _ => DocumentKind::Unknown,
    }
}

fn parse_major(version: &str) -> Option<u32> {
    version.trim().split('.').next()?.parse::<u32>().ok()
}

/// SHA-256 of a CDD file's bytes, hex lowercase.
///
/// This is a real SHA-256 rather than the non-cryptographic digest the save
/// receipt uses ([`crate::storage::hash_file`]). The difference matters: preview
/// and apply are separate user-visible steps, and apply must refuse a file that
/// changed in between. A content *identifier* that collides is an inconvenience;
/// one an adversary can collide on would let a different document be imported
/// under a preview the user already approved.
pub fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_error(path: &Path, detail: &impl std::fmt::Display) -> EngineError {
    EngineError::CddParse {
        path: path.display().to_string(),
        detail: detail.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const SYNTHETIC: &[u8] = br#"<?xml version='1.0' encoding='utf-8'?>
<!DOCTYPE CANDELA SYSTEM 'candela.dtd'>
<CANDELA dtdvers='13.0.101'>
<ECUDOC doctype='inst'>
<ECU id='_ecu1'><NAME><TUV lang='en-US'>TestEcu</TUV></NAME><QUAL>TestEcu</QUAL>
<VAR id='_var1'><NAME><TUV lang='de-DE'>Basis</TUV><TUV lang='en-US'>Base</TUV></NAME><QUAL>Base</QUAL></VAR>
</ECU>
<DIDS><DID id='_d1' n='64'/><DID id='_d2'/></DIDS>
<DTCS><DTC id='_t1'/></DTCS>
</ECUDOC>
</CANDELA>"#;

    fn preview_bytes(bytes: &[u8], name: &str) -> Result<CddPreview, EngineError> {
        scan(
            Cursor::new(bytes),
            Path::new(name),
            digest_hex(bytes),
        )
    }

    #[test]
    fn header_fields_and_counts_are_extracted() {
        let preview = preview_bytes(SYNTHETIC, "synthetic.cdd").unwrap();
        assert_eq!(preview.root, "CANDELA");
        assert_eq!(preview.document_kind, DocumentKind::Cdd);
        assert_eq!(preview.doctype, "inst");
        assert_eq!(preview.dtd_version, "13.0.101");
        assert_eq!(preview.major_version, Some(13));
        assert!(preview.supported);
        assert_eq!(preview.ecu, "TestEcu");
        assert_eq!(preview.variants, vec!["Base".to_string()]);
        assert_eq!(preview.counts.did, 2);
        assert_eq!(preview.counts.dtc, 1);
        assert!(preview.doctype_declaration.contains("candela.dtd"));
    }

    /// The DID number is the element's `n` attribute, read as an integer. It is
    /// not a length and not text inside the name: the corpus contains seven DIDs
    /// in `ABS_ESP-Example.cdd` whose `n` is above 61840, all in the 0xF1xx
    /// OEM-extension range, which no bit length could take.
    #[test]
    fn a_did_identifier_is_read_from_the_n_attribute() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'><ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL><VAR><NAME><TUV xml:lang='en-US'>Base</TUV></NAME><QUAL>Base</QUAL>
<DIDS>
<DID id='_a' n='263'><NAME><TUV xml:lang='en-US'>Supply Voltage ECU</TUV></NAME><QUAL>Supply_Voltage_ECU</QUAL><STRUCTURE><DATAOBJ oid='o1' dtref='_t1'><NAME><TUV xml:lang='en-US'>Test</TUV></NAME><QUAL>Test</QUAL></DATAOBJ></STRUCTURE></DID>
<DID id='_b' n='61848'><QUAL>Fingerprint</QUAL></DID>
<DID id='_c'><QUAL>NoNumber</QUAL></DID>
<DID id='_d' n='0x200'><QUAL>Hex</QUAL></DID>
</DIDS>
</VAR></ECU></ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "dids.cdd").unwrap();
        assert_eq!(preview.dids.len(), 4, "{:?}", preview.dids);

        assert_eq!(preview.dids[0].identifier, Some(263));
        assert_eq!(preview.dids[0].name, "Supply Voltage ECU");
        assert_eq!(preview.dids[0].qualifier, "Supply_Voltage_ECU");
        assert_eq!(preview.dids[0].id, "_a");
        assert_eq!(preview.dids[0].members.len(), 1);
        assert_eq!(preview.dids[0].members[0].name, "Test");
        assert_eq!(preview.dids[0].members[0].data_type_ref, "_t1");

        // A value above 512 is an OEM-extension DID, not a length.
        assert_eq!(preview.dids[1].identifier, Some(61848));

        // No `n`: counted, but not importable.
        assert_eq!(preview.dids[2].identifier, None);
        assert_eq!(preview.dids[2].qualifier, "NoNumber");

        // A document may write the number in hex.
        assert_eq!(preview.dids[3].identifier, Some(0x200));
    }

    /// Decimal and hex are both accepted, and anything else yields `None` rather
    /// than a guess.
    #[test]
    fn identifier_parsing_accepts_decimal_and_hex_only() {
        assert_eq!(parse_u32("263"), Some(263));
        assert_eq!(parse_u32(" 263 "), Some(263));
        assert_eq!(parse_u32("0x200"), Some(0x200));
        assert_eq!(parse_u32("0X200"), Some(0x200));
        assert_eq!(parse_u32("0"), Some(0));
        assert_eq!(parse_u32("65535"), Some(65535));
        assert_eq!(parse_u32(""), None);
        assert_eq!(parse_u32("abc"), None);
        assert_eq!(parse_u32("0x"), None);
        assert_eq!(parse_u32("-1"), None, "a negative DID number is not a DID");
        assert_eq!(parse_u32("1.5"), None);
    }

    /// A DID identifier outside the 16-bit UDS range cannot be stored, and the
    /// schema says so rather than truncating it.
    #[test]
    fn the_identifier_range_is_the_uds_range() {
        assert_eq!(DID_MAX, 0xFFFF);
    }

    /// A linear scale is stored as the shortest *exact* text.
    ///
    /// The corpus supplies both shapes: `f='1' div='16'` and `f='359' div='255'`.
    /// A `f64` or a SQLite `REAL` cannot hold the second one, and a scale that is
    /// off by a rounding error decodes a diagnostic value to a different number
    /// while looking correct.
    #[test]
    fn a_linear_scale_is_the_shortest_exact_text() {
        assert_eq!(exact_ratio("1"), Some((1, 1)));
        assert_eq!(exact_ratio("0.0625"), Some((1, 16)));
        assert_eq!(exact_ratio("0.1"), Some((1, 10)));
        assert_eq!(exact_ratio("359"), Some((359, 1)));
        assert_eq!(exact_ratio("3/8"), Some((3, 8)));
        assert_eq!(exact_ratio("8/4"), Some((2, 1)), "reduced");
        assert_eq!(exact_ratio("0.50"), Some((1, 2)), "trailing zeros are not a value");
        assert_eq!(exact_ratio("-0.25"), Some((-1, 4)));
        assert_eq!(exact_ratio("0x10"), Some((16, 1)), "a hex integer is accepted");
        assert_eq!(exact_ratio(""), None);
        assert_eq!(exact_ratio("abc"), None);
        assert_eq!(exact_ratio("1e5"), None, "an exponent is not read as written");
        assert_eq!(
            exact_ratio("0.1234567890123456789"),
            None,
            "19 fractional places is a document defect, not a number to keep"
        );

        // A terminating decimal is written as one; anything else stays a fraction.
        assert_eq!(ratio_text(1, 1), "1");
        assert_eq!(ratio_text(1, 16), "0.0625");
        assert_eq!(ratio_text(1, 10), "0.1");
        assert_eq!(ratio_text(359, 255), "359/255");
        assert_eq!(ratio_text(16, 1), "16");
        assert_eq!(ratio_text(-1, 4), "-0.25");
        assert_eq!(ratio_text(1, 3), "1/3");
        assert_eq!(ratio_text(0, 5), "0");
    }

    /// The offset is normalised but never rounded: it is not divided by the
    /// factor, which the reference adjudicated against CANdelaStudio's own ODX
    /// output (`f=1 div=10 o=-13` gives a V0 of -13, not -1.3).
    #[test]
    fn an_offset_is_kept_verbatim_in_value() {
        assert_eq!(normalize_decimal("0"), Some("0".into()));
        assert_eq!(normalize_decimal("-13"), Some("-13".into()));
        assert_eq!(normalize_decimal("1.50"), Some("1.5".into()));
        assert_eq!(normalize_decimal("-0.0"), Some("0".into()));
        assert_eq!(normalize_decimal("1/2"), None, "a fraction is not an offset");
        assert_eq!(normalize_decimal("abc"), None);
    }

    /// A coded value type is read from `CVALUETYPE` only, and the physical type
    /// is not mistaken for it.
    ///
    /// `PVALUETYPE` follows `CVALUETYPE` in every corpus type and describes the
    /// *display* value -- an 8-bit coded byte shown as a 64-bit double, an ASCII
    /// string shown as UTF-8 text. Reading it as the wire format would decode
    /// every value of the type wrongly.
    #[test]
    fn the_coded_value_type_wins_over_the_physical_one() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>Base</TUV></NAME><QUAL>Base</QUAL></VAR></ECU>
<DATATYPES>
<LINCOMP id='_t1' bm='65535'>
<NAME><TUV xml:lang='en-US'>Voltage</TUV></NAME><QUAL>Voltage</QUAL>
<CVALUETYPE bl='8' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
<PVALUETYPE bl='64' bo='21' enc='dbl' sig='0' df='dec' qty='atom' sz='no' minsz='0' maxsz='255'><UNIT>V</UNIT></PVALUETYPE>
<COMP s='1' e='255' f='1' div='16' o='0'></COMP>
</LINCOMP>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "cv.cdd").unwrap();
        assert_eq!(preview.data_types.len(), 1, "{:?}", preview.unmapped_types);
        let data_type = &preview.data_types[0];
        assert_eq!(data_type.raw_encoding, RawEncoding::Unsigned);
        assert_eq!(data_type.bit_length, 8, "the coded width, not the physical 64");
        assert_eq!(data_type.quantity, Quantity::Atom);
        assert!(data_type.big_endian);
        assert_eq!(data_type.length_prefix, None);
        assert_eq!(data_type.bit_mask, Some(0xFFFF));
        assert_eq!(data_type.unit, "V");
        assert_eq!(data_type.conversion, Conversion::Linear);
        let linear = data_type.linear.as_ref().expect("a single COMP is a scale");
        assert_eq!(linear.factor, "0.0625");
        assert_eq!(linear.offset, "0");
        assert_eq!(linear.raw_lower, Some(1));
        assert_eq!(linear.raw_upper, Some(255));
    }

    /// A type whose wire format cannot be read is refused with the value that
    /// caused it, not imported with a guess.
    ///
    /// The corpus has a file whose single defect is `bo='4321'`, so this is a real
    /// case and not a hypothetical. Guessing an endianness would decode every
    /// value of that type backwards.
    #[test]
    fn a_type_with_an_unreadable_wire_format_is_refused_with_the_reason() {
        // A valid `CVALUETYPE`, then one attribute replaced at a time. Each case
        // overrides exactly one value, so the refusal is attributable to that one
        // and not to some interaction between several.
        const VALID: &[(&str, &str)] = &[
            ("bl", "8"),
            ("bo", "21"),
            ("enc", "uns"),
            ("sig", "0"),
            ("df", "hex"),
            ("qty", "atom"),
            ("sz", "no"),
        ];
        for (key, bad_value, expected) in [
            ("bo", "4321", "byte order '4321'"),
            ("enc", "wat", "raw encoding 'wat'"),
            ("sz", "lsz9bytes", "length prefix 'lsz9bytes'"),
            ("qty", "herd", "quantity 'herd'"),
            ("bl", "0", "bit length '0' is outside 1..=64"),
            ("bl", "", "bit length '' is outside 1..=64"),
            ("sz", "", "length prefix '' is not supported"),
        ] {
            let attributes: Vec<String> = VALID
                .iter()
                .map(|(name, value)| {
                    let value = if *name == key { bad_value } else { value };
                    format!("{name}='{value}'")
                })
                .collect();
            let xml = format!(
                "<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>\
                 <ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>\
                 <VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>\
                 <DATATYPES><IDENT id='_t1' bm='255'>\
                 <NAME><TUV xml:lang='en-US'>Odd</TUV></NAME><QUAL>Odd</QUAL>\
                 <CVALUETYPE {}/>\
                 </IDENT></DATATYPES></ECUDOC></CANDELA>",
                attributes.join(" ")
            );
            let preview = preview_bytes(xml.as_bytes(), "odd.cdd").unwrap();
            assert!(
                preview.data_types.is_empty(),
                "{key}='{bad_value}' must not import a type: {:?}",
                preview.data_types
            );
            assert_eq!(preview.counts.data_type, 1, "it is still counted as scanned");
            assert_eq!(preview.unmapped_types.len(), 1, "{:?}", preview.unmapped_types);
            let group = &preview.unmapped_types[0];
            assert_eq!(group.element, "IDENT");
            assert_eq!(group.count, 1);
            assert_eq!(group.examples, vec!["_t1".to_string()]);
            assert!(
                group.reason.contains(expected),
                "{key}='{bad_value}': reason '{}' must say {expected}",
                group.reason
            );
        }
    }

    /// A `<CVALUETYPE>` written self-closing is the common form in the corpus,
    /// so the empty-element path has to read it too.
    #[test]
    fn a_self_closing_coded_value_type_is_read() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<IDENT id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Byte</TUV></NAME><QUAL>Byte</QUAL>
<CVALUETYPE bl='8' bo='12' enc='sgn' sig='0' df='dec' qty='atom' sz='no' minsz='0' maxsz='255'/>
<PVALUETYPE bl='8' bo='12' enc='sgn' sig='0' df='dec' qty='atom' sz='no' minsz='0' maxsz='255'/>
</IDENT>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "empty-cv.cdd").unwrap();
        assert_eq!(preview.data_types.len(), 1, "{:?}", preview.unmapped_types);
        let data_type = &preview.data_types[0];
        assert_eq!(data_type.raw_encoding, RawEncoding::Signed);
        assert_eq!(data_type.bit_length, 8);
        assert!(!data_type.big_endian, "bo='12' is little endian");
        assert_eq!(data_type.conversion, Conversion::Identity);
    }

    /// A composite type is counted apart from the simple ones and reported with
    /// the reason, so "the file has 3500 data types" cannot be read as "we
    /// imported 3500 data types".
    #[test]
    fn a_composite_type_is_counted_and_reported_separately() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<IDENT id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Byte</TUV></NAME><QUAL>Byte</QUAL>
<CVALUETYPE bl='8' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/></IDENT>
<CONSTCOMP id='_c1' bm='255'><NAME><TUV xml:lang='en-US'>Struct</TUV></NAME><QUAL>Struct</QUAL></CONSTCOMP>
<CONSTCOMP id='_c2' bm='255'><NAME><TUV xml:lang='en-US'>Struct2</TUV></NAME><QUAL>Struct2</QUAL></CONSTCOMP>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "composite.cdd").unwrap();
        assert_eq!(preview.counts.data_type, 1, "one simple type");
        assert_eq!(preview.counts.composite_type, 2, "two composite types");
        assert_eq!(preview.data_types.len(), 1);
        assert_eq!(preview.unmapped_types.len(), 1, "one group: {:?}", preview.unmapped_types);
        let group = &preview.unmapped_types[0];
        assert_eq!(group.element, "CONSTCOMP");
        assert_eq!(group.count, 2);
        assert_eq!(group.examples, vec!["_c1".to_string(), "_c2".to_string()]);
    }

    /// A piecewise scale is refused rather than flattened to its first segment.
    ///
    /// Taking `comp[0]` would state a scale for raw values the document assigns a
    /// different one to, and a diagnostic value decoded at the wrong scale is not
    /// a near miss.
    #[test]
    fn a_piecewise_scale_is_refused_rather_than_flattened() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<COMPTBL id='_t1' bm='65535'><NAME><TUV xml:lang='en-US'>Piece</TUV></NAME><QUAL>Piece</QUAL>
<CVALUETYPE bl='16' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
<COMP s='0' e='100' f='1' o='0'></COMP>
<COMP s='50' e='200' f='1' o='500'></COMP>
</COMPTBL>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "piecewise.cdd").unwrap();
        assert!(preview.data_types.is_empty(), "{:?}", preview.data_types);
        assert!(
            preview.unmapped_types[0].reason.contains("piecewise"),
            "{}",
            preview.unmapped_types[0].reason
        );
    }

    /// A `COMPTBL` with no `<COMP>` has no scale, and saying `linear` would
    /// invent one.
    #[test]
    fn a_conversion_is_read_off_the_content_not_the_element_name() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<COMPTBL id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Empty</TUV></NAME><QUAL>Empty</QUAL>
<CVALUETYPE bl='8' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
</COMPTBL>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "empty-comptbl.cdd").unwrap();
        assert_eq!(preview.data_types.len(), 1, "{:?}", preview.unmapped_types);
        assert_eq!(preview.data_types[0].kind, DataTypeKind::CompTable);
        assert_eq!(
            preview.data_types[0].conversion,
            Conversion::Identity,
            "no <COMP> means no scale, whatever the element is called"
        );
    }

    /// An `<ADDINFO>` inside a `<TEXTMAP>` is documentation, not a label.
    ///
    /// `Vector_UDS_13.0.cddt` has `<TEXTMAP s='1' e='1'>` holding a `<TEXT>` with
    /// the label and an `<ADDINFO>` describing it. Reading the annotation as a
    /// second label would put two en-US rows at the same table position.
    #[test]
    fn a_textmap_label_comes_from_its_text_and_not_its_addinfo() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<TEXTTBL id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Counter</TUV></NAME><QUAL>Counter</QUAL>
<CVALUETYPE bl='8' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
<PVALUETYPE bl='16' bo='21' enc='utf' sig='0' df='text' qty='field' sz='no' minsz='0' maxsz='65535'/>
<TEXTMAP s='1' e='1'>
<TEXT><TUV xml:lang='en-US'>OccurrenceCounter</TUV></TEXT>
<ADDINFO><TUV xml:lang='en-US'>Counts the number of times the DTC have been set.</TUV></ADDINFO>
</TEXTMAP>
<TEXTMAP s='0' e='0'>
<TEXT><TUV xml:lang='en-US'>false</TUV><TUV xml:lang='de-DE'>falsch</TUV></TEXT>
</TEXTMAP>
</TEXTTBL>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "textmap.cdd").unwrap();
        assert_eq!(preview.data_types.len(), 1, "{:?}", preview.unmapped_types);
        let data_type = &preview.data_types[0];
        assert_eq!(data_type.conversion, Conversion::TextTable);
        assert_eq!(
            data_type.entries.len(),
            3,
            "one label for the first range, two locales for the second: {:?}",
            data_type.entries
        );
        assert_eq!(data_type.entries[0].position, 0);
        assert_eq!(data_type.entries[0].text, "OccurrenceCounter");
        assert_eq!(data_type.entries[0].language, "en-US");
        assert_eq!(data_type.entries[1].position, 1);
        assert_eq!(data_type.entries[1].text, "false");
        assert_eq!(data_type.entries[2].text, "falsch");
        assert_eq!(data_type.entries[2].language, "de-DE");
        for entry in &data_type.entries {
            assert!(
                !entry.text.starts_with("Counts the number"),
                "an annotation is not a label: {entry:?}"
            );
        }
    }

    /// An `<EXCL>` keeps its range, its inverse meaning, and its label.
    #[test]
    fn an_exclusion_keeps_its_range_inverse_and_label() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<LINCOMP id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Counter</TUV></NAME><QUAL>Counter</QUAL>
<CVALUETYPE bl='8' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
<EXCL s='101' e='255' inv='undef'><TEXT><TUV xml:lang='en-US'>Reserved</TUV></TEXT></EXCL>
<COMP s='0' e='100' f='1' o='0'></COMP>
</LINCOMP>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "excl.cdd").unwrap();
        assert_eq!(preview.data_types.len(), 1, "{:?}", preview.unmapped_types);
        let data_type = &preview.data_types[0];
        assert_eq!(data_type.exclusions.len(), 1);
        let exclusion = &data_type.exclusions[0];
        assert_eq!(exclusion.range, RawRange { start: 101, end: 255 });
        assert_eq!(exclusion.inverse, "undef");
        assert_eq!(exclusion.text, "Reserved");
        assert_eq!(exclusion.language, "en-US");
    }

    /// A leading length field changes the byte layout, so it is recorded rather
    /// than treated as a fixed length.
    #[test]
    fn a_leading_length_field_is_recorded_with_its_bounds() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<IDENT id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Memory</TUV></NAME><QUAL>Memory</QUAL>
<CVALUETYPE bl='8' bo='21' enc='uns' sig='0' df='hex' qty='field' sz='lsz2bytes' minsz='1' maxsz='65530'/>
<PVALUETYPE bl='8' bo='21' enc='uns' sig='0' df='hex' qty='field' sz='lsz2bytes' minsz='1' maxsz='65530'/>
</IDENT>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "prefix.cdd").unwrap();
        assert_eq!(preview.data_types.len(), 1, "{:?}", preview.unmapped_types);
        let data_type = &preview.data_types[0];
        assert_eq!(data_type.length_prefix, Some(LengthPrefix::TwoBytes));
        assert_eq!(data_type.quantity, Quantity::Field);
        assert_eq!(data_type.min_length, Some(1));
        assert_eq!(data_type.max_length, Some(65530));
    }

    /// An `atom` carries `minsz`/`maxsz` too, where they mean nothing. Every
    /// TEXTTBL in the corpus declares 0/255 on an 8-bit atom; reading those as a
    /// count would give every table a length of up to 255 values.
    #[test]
    fn bounds_are_read_only_for_a_field() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<IDENT id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Bit</TUV></NAME><QUAL>Bit</QUAL>
<CVALUETYPE bl='1' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
</IDENT>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "atom.cdd").unwrap();
        assert_eq!(preview.data_types.len(), 1, "{:?}", preview.unmapped_types);
        let data_type = &preview.data_types[0];
        assert_eq!(data_type.quantity, Quantity::Atom);
        assert_eq!(data_type.min_length, None);
        assert_eq!(data_type.max_length, None);
    }

    /// A `FORMULADT` keeps its expression verbatim, unevaluated.
    #[test]
    fn a_formula_is_stored_verbatim_and_not_evaluated() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<FORMULADT id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Temperature</TUV></NAME><QUAL>Temperature</QUAL>
<CVALUETYPE bl='8' bo='21' enc='sgn' sig='0' df='dec' qty='atom' sz='no' minsz='0' maxsz='255'/>
<FORMULA usage='coded2Phys'>270+510*(1-pow(e,-0.0255*x))</FORMULA>
</FORMULADT>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "formula.cdd").unwrap();
        assert_eq!(preview.data_types.len(), 1, "{:?}", preview.unmapped_types);
        let data_type = &preview.data_types[0];
        assert_eq!(data_type.conversion, Conversion::Formula);
        let formula = data_type.formula.as_ref().expect("the formula is kept");
        assert_eq!(formula.usage, "coded2Phys");
        assert_eq!(formula.expression, "270+510*(1-pow(e,-0.0255*x))");
        assert_eq!(
            data_type.linear, None,
            "a formula is not a scale, whatever an offset it happens to contain"
        );
    }

    /// A bit mask narrower than the declared width is kept, because it is the
    /// field's real width. The corpus has two such types.
    #[test]
    fn a_partial_bit_mask_is_kept_rather_than_dropped() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<IDENT id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Version</TUV></NAME><QUAL>Version</QUAL>
<CVALUETYPE bl='32' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
</IDENT>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "mask.cdd").unwrap();
        assert_eq!(preview.data_types.len(), 1, "{:?}", preview.unmapped_types);
        let data_type = &preview.data_types[0];
        assert_eq!(data_type.bit_length, 32);
        assert_eq!(data_type.bit_mask, Some(0xFF));
        assert!(
            data_type.bit_mask < Some(1u64 << data_type.bit_length),
            "the mask is narrower than the declared width, and that is the point"
        );
    }

    /// A type element nested inside another one belongs to the outer type, and
    /// must not be recorded as a type in its own right.
    #[test]
    fn a_nested_type_element_is_part_of_the_outer_type() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<IDENT id='_outer' bm='255'><NAME><TUV xml:lang='en-US'>Outer</TUV></NAME><QUAL>Outer</QUAL>
<CVALUETYPE bl='8' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
<TEXTTBL id='_inner' bm='255'><NAME><TUV xml:lang='en-US'>Inner</TUV></NAME><QUAL>Inner</QUAL>
<CVALUETYPE bl='1' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
</TEXTTBL>
</IDENT>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "nested.cdd").unwrap();
        assert_eq!(preview.counts.data_type, 2, "both are scanned");
        assert_eq!(
            preview.data_types.len(),
            1,
            "only the outer one is a type: {:?}",
            preview.data_types
        );
        assert_eq!(preview.data_types[0].id, "_outer");
    }

    /// A `VALTBL` is a characteristic curve, which this importer does not model.
    /// Storing it as something simpler would be a lie about what it is.
    #[test]
    fn a_characteristic_curve_is_refused_rather_than_simplified() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'>
<ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL>
<VAR><NAME><TUV xml:lang='en-US'>B</TUV></NAME><QUAL>B</QUAL></VAR></ECU>
<DATATYPES>
<VALTBL id='_t1' bm='255'><NAME><TUV xml:lang='en-US'>Curve</TUV></NAME><QUAL>Curve</QUAL>
<CVALUETYPE bl='8' bo='21' enc='uns' sig='0' df='hex' qty='atom' sz='no' minsz='0' maxsz='255'/>
</VALTBL>
</DATATYPES>
</ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "valtbl.cdd").unwrap();
        assert!(preview.data_types.is_empty(), "{:?}", preview.data_types);
        assert_eq!(preview.unmapped_types[0].element, "VALTBL");
        assert!(
            preview.unmapped_types[0].reason.contains("characteristic"),
            "{}",
            preview.unmapped_types[0].reason
        );
    }

    /// An element with an empty TUV (an untranslated name) must not produce an
    /// empty name, which is indistinguishable from missing data in the UI.
    #[test]
    fn an_empty_tuv_falls_back_to_the_qualifier() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'><ECU><NAME><TUV xml:lang='en-US'>E</TUV></NAME><QUAL>E</QUAL><VAR><NAME><TUV xml:lang='en-US'>Base</TUV></NAME><QUAL>Base</QUAL>
<DIDS><DID id='_a'><NAME><TUV xml:lang='en-US'></TUV></NAME><QUAL>Field_One</QUAL></DID></DIDS>
</VAR></ECU></ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "empty-tuv.cdd").unwrap();
        assert_eq!(preview.dids[0].name, "Field_One", "{:?}", preview.dids[0]);
    }

    #[test]
    fn preferred_language_wins_regardless_of_order() {
        let xml = br#"<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'><ECU><NAME><TUV xml:lang='de-DE'>Deutsch</TUV><TUV xml:lang='en-US'>English</TUV></NAME><QUAL>Q</QUAL></ECU></ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "lang.cdd").unwrap();
        assert_eq!(preview.ecu, "English");
    }

    #[test]
    fn unsupported_major_is_reported_not_failed() {
        let xml = br#"<CANDELA dtdvers='14.0.1'><ECUDOC doctype='inst'><ECU><NAME><TUV lang='en-US'>FutureEcu</TUV></NAME><QUAL>FutureEcu</QUAL></ECU></ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "future.cdd").unwrap();
        assert!(!preview.supported);
        assert_eq!(preview.major_version, Some(14));
        assert_eq!(preview.ecu, "FutureEcu");
        let reason = preview.unsupported_reason.unwrap();
        assert!(reason.contains("14"), "{reason}");
    }

    #[test]
    fn template_doctype_is_cddt() {
        let xml = br#"<CANDELA dtdvers='13.0.100'><ECUDOC doctype='tmpl'><ECU><NAME><TUV lang='en-US'>Tpl</TUV></NAME><QUAL>Tpl</QUAL></ECU></ECUDOC></CANDELA>"#;
        let preview = preview_bytes(xml, "template.cddt").unwrap();
        assert_eq!(preview.document_kind, DocumentKind::Cddt);
        assert!(preview.supported, "{preview:?}");
    }

    #[test]
    fn extension_mismatch_marks_document_unsupported() {
        let preview = preview_bytes(SYNTHETIC, "renamed.cddt").unwrap();
        assert!(!preview.supported);
        assert!(
            preview
                .unsupported_reason
                .as_deref()
                .unwrap_or_default()
                .contains("does not match"),
            "{preview:?}"
        );
    }

    #[test]
    fn wrong_root_is_rejected() {
        let error = preview_bytes(b"<DBC><MESSAGES/></DBC>", "not-cdd.cdd").unwrap_err();
        assert!(matches!(error, EngineError::CddParse { .. }), "{error}");
    }

    #[test]
    fn missing_ecudoc_is_rejected() {
        let error = preview_bytes(b"<CANDELA dtdvers='13.0.101'></CANDELA>", "empty.cdd").unwrap_err();
        assert!(matches!(error, EngineError::CddParse { .. }), "{error}");
    }

    #[test]
    fn depth_bomb_is_rejected() {
        let mut xml = String::from("<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'><ECU>");
        for _ in 0..(MAX_DEPTH + 8) {
            xml.push_str("<A>");
        }
        for _ in 0..(MAX_DEPTH + 8) {
            xml.push_str("</A>");
        }
        xml.push_str("</ECU></ECUDOC></CANDELA>");
        let error = preview_bytes(xml.as_bytes(), "deep.cdd").unwrap_err();
        assert!(matches!(error, EngineError::DepthExceeded { .. }), "{error}");
    }

    #[test]
    fn external_entities_are_never_expanded() {
        let xml = br#"<?xml version='1.0'?>
<!DOCTYPE CANDELA [<!ENTITY xxe SYSTEM "file:///nonexistent-xxe-target">]>
<CANDELA dtdvers='13.0.101'><ECUDOC doctype='inst'><ECU><NAME><TUV lang='en-US'>&xxe;</TUV></NAME><QUAL>Q</QUAL></ECU></ECUDOC></CANDELA>"#;
        match preview_bytes(xml, "xxe.cdd") {
            Ok(preview) => {
                assert!(!preview.ecu.contains("nonexistent-xxe-target"));
                assert!(!preview.ecu.contains("root:"));
            }
            Err(EngineError::CddParse { .. }) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
}
