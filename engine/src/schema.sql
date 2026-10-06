-- diagauthor engine -- CDD-oriented project schema, version 1.
--
-- This is a diagnostic authoring schema (CANdela CDD/CDDT domain), deliberately
-- not a DBC/network schema. It is written into a single SQLite file whose
-- `application_id` is 0x44494147 ("DIAG") and whose `user_version` is 1.
--
-- Design notes:
--   * `source_files.content` keeps the ORIGINAL bytes of every imported CDD/CDDT,
--     so exports can reproduce anything the importer did not understand.
--   * `source_mappings` links a domain object back to its XML node.
--   * `unsupported_nodes` is the "preserved original" table: unrecognized XML is
--     stored verbatim with its path and a `semantic` flag (0 = informational,
--     1 = blocking export).
--   * Foreign keys cascade with their parent; nothing silently orphans. The
--     connection must run with `PRAGMA foreign_keys=ON` (the engine does).
--   * Hot lookups (identifier, code_hex, name) have dedicated indexes.

CREATE TABLE project_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE source_files (
    id          INTEGER PRIMARY KEY,
    path        TEXT    NOT NULL,
    kind        TEXT    NOT NULL CHECK (kind IN ('cdd', 'cddt')),
    sha256      TEXT    NOT NULL DEFAULT '',
    byte_size   INTEGER NOT NULL DEFAULT 0 CHECK (byte_size >= 0),
    content     BLOB    NOT NULL,
    imported_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX source_files_kind_idx ON source_files (kind);

CREATE TABLE source_mappings (
    object_id      TEXT    NOT NULL,
    source_file_id INTEGER NOT NULL REFERENCES source_files (id) ON DELETE CASCADE,
    xpath          TEXT    NOT NULL,
    element_name   TEXT    NOT NULL,
    PRIMARY KEY (object_id, source_file_id, xpath)
);

CREATE INDEX source_mappings_file_idx ON source_mappings (source_file_id);

CREATE TABLE unsupported_nodes (
    id             INTEGER PRIMARY KEY,
    source_file_id INTEGER NOT NULL REFERENCES source_files (id) ON DELETE CASCADE,
    node_path      TEXT    NOT NULL,
    element_name   TEXT    NOT NULL,
    reason_code    TEXT    NOT NULL,
    semantic       INTEGER NOT NULL DEFAULT 0 CHECK (semantic IN (0, 1)),
    raw_xml        TEXT
);

CREATE INDEX unsupported_nodes_file_idx ON unsupported_nodes (source_file_id);
CREATE INDEX unsupported_nodes_semantic_idx ON unsupported_nodes (semantic);

CREATE TABLE ecus (
    id             INTEGER PRIMARY KEY,
    name           TEXT    NOT NULL,
    qualifier      TEXT    NOT NULL UNIQUE,
    -- A CDD carries the manufacturer on the ECU element. Kept as read data:
    -- a document that omits it yields '' rather than a guess.
    manufacturer   TEXT    NOT NULL DEFAULT '',
    source_file_id INTEGER REFERENCES source_files (id) ON DELETE SET NULL
);

CREATE INDEX ecus_name_idx ON ecus (name);

CREATE TABLE variants (
    id               INTEGER PRIMARY KEY,
    ecu_id           INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    name             TEXT    NOT NULL,
    qualifier        TEXT    NOT NULL DEFAULT '',
    -- Exactly one variant per document is the base variant: the one a new
    -- project starts from (plan 9절). Every other variant is a coding variant
    -- layered on it.
    is_base          INTEGER NOT NULL DEFAULT 0 CHECK (is_base IN (0, 1)),
    coding_keys_json TEXT    NOT NULL DEFAULT '[]',
    UNIQUE (ecu_id, name)
);

CREATE INDEX variants_name_idx ON variants (name);
CREATE INDEX variants_base_idx ON variants (ecu_id, is_base);

-- A simple data type, as a CDD document declares one.
--
-- The document's own vocabulary is kept rather than a resolved primitive: the
-- element name, the coded encoding, and the element count. `CVALUETYPE` is the
-- only source for the wire format, so `PVALUETYPE` never lands in these columns.
CREATE TABLE data_types (
    id            INTEGER PRIMARY KEY,
    ecu_id        INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    qualifier     TEXT    NOT NULL DEFAULT '',
    -- The CDD element this came from. `objects.query` reports it as `kind`, so
    -- the grid shows "TEXTTBL" rather than a primitive the document never named.
    kind          TEXT    NOT NULL
                    CHECK (kind IN ('IDENT', 'LINCOMP', 'TEXTTBL', 'COMPTBL',
                                    'VALTBL', 'FORMULADT')),
    -- The element-local id, which is what a DID member's dtref points at.
    source_ref    TEXT    NOT NULL DEFAULT '',
    -- What the type does to a coded value, read off what the element contains.
    conversion    TEXT    NOT NULL
                    CHECK (conversion IN ('identity', 'linear', 'text_table', 'formula')),
    -- CVALUETYPE/@enc. `bcd` is kept because a packed BCD is a distinct wire
    -- format, not a rendering of an integer.
    raw_encoding  TEXT    NOT NULL
                    CHECK (raw_encoding IN ('uns', 'sgn', 'asc', 'bcd', 'flt')),
    -- CVALUETYPE/@qty: one value, or a run of them.
    quantity      TEXT    NOT NULL DEFAULT 'atom'
                    CHECK (quantity IN ('atom', 'field')),
    -- CVALUETYPE/@bl: bits per element. An ordinal, so INTEGER.
    bit_length    INTEGER NOT NULL CHECK (bit_length BETWEEN 1 AND 64),
    -- CVALUETYPE/@bo. Only 21 (big) and 12 (little) exist; anything else is a
    -- document defect and is refused at import rather than assumed.
    byte_order    TEXT    NOT NULL CHECK (byte_order IN ('big', 'little')),
    -- CVALUETYPE/@sz: a leading length field, in bits. NULL is `sz='no'`, which
    -- is a fixed length -- a different byte layout, not an unknown one.
    length_prefix_bits INTEGER
                    CHECK (length_prefix_bits IS NULL
                           OR length_prefix_bits IN (8, 16, 24, 32)),
    -- CVALUETYPE/@minsz / @maxsz. Meaningful only for quantity='field'; an atom
    -- carries the attributes too, where they mean nothing.
    min_length    INTEGER CHECK (min_length IS NULL OR min_length >= 0),
    max_length    INTEGER CHECK (max_length IS NULL OR max_length >= 0),
    -- The element's @bm bit mask. A mask narrower than bit_length means the
    -- field occupies only part of those bits, so the mask -- not bit_length
    -- alone -- is the field's width. Stored as text because it can be the full
    -- 64-bit value, which REAL cannot hold.
    bit_mask      TEXT,
    -- phys = raw * factor_text + offset_text, exact. The corpus contains
    -- f='359' div='255', and 359/255 has no finite binary or decimal form: a
    -- REAL would store a different scale than the document declares. A
    -- terminating decimal is written as one ("0.0625"), anything else as a
    -- reduced fraction ("359/255").
    factor_text   TEXT    NOT NULL DEFAULT '1',
    offset_text   TEXT    NOT NULL DEFAULT '0',
    -- The <COMP s= e=> bounds. Each absent when the document wrote none: a
    -- one-sided conversion is one-sided, and filling in the implied bound would
    -- state something the file does not say.
    raw_lower     TEXT,
    raw_upper     TEXT,
    -- FORMULADT: the expression verbatim, and the direction it converts. Not
    -- evaluated -- an unevaluated formula is preserved data.
    formula_expression TEXT,
    formula_usage TEXT,
    -- PVALUETYPE/UNIT, the physical unit.
    unit          TEXT    NOT NULL DEFAULT ''
);

CREATE INDEX data_types_name_idx ON data_types (name);
CREATE INDEX data_types_kind_idx ON data_types (kind);
CREATE INDEX data_types_source_ref_idx ON data_types (ecu_id, source_ref);

-- Uniqueness is on the document-local id and nothing else, because a document
-- may name many types the same thing: `cantools_example.cdd` has 59 data types
-- all called `LocalTable`, so a name or qualifier key would reject a perfectly
-- valid file. The id is what a DID member's `dtref` resolves against, so two
-- types sharing one would make that reference ambiguous.
--
-- The index is partial because a type with no `id` has no key to be unique on,
-- and an unkeyed type must still import rather than collide with another.
CREATE UNIQUE INDEX data_types_source_ref_unique
    ON data_types (ecu_id, source_ref) WHERE source_ref <> '';

CREATE TABLE conversion_entries (
    type_id  INTEGER NOT NULL REFERENCES data_types (id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    kind     TEXT    NOT NULL DEFAULT 'enum',
    raw      TEXT    NOT NULL,
    scaled   TEXT    NOT NULL,
    PRIMARY KEY (type_id, position)
);

-- A TEXTTBL's coded-value to text mapping. One row per TEXTMAP/TUV, so a
-- multilingual document keeps every locale instead of collapsing to one.
CREATE TABLE type_table_entries (
    type_id   INTEGER NOT NULL REFERENCES data_types (id) ON DELETE CASCADE,
    -- Document order, so the table reads the way the document did.
    position  INTEGER NOT NULL CHECK (position >= 0),
    locale    TEXT    NOT NULL DEFAULT 'en-US',
    -- Bounds are TEXT, not INTEGER: they are the document's literals, and an
    -- absent bound stays absent.
    raw_lower TEXT    NOT NULL,
    raw_upper TEXT,
    label     TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (type_id, position, locale)
);

-- An <EXCL>: raw values with no defined meaning, and what they mean instead
-- (noSignal, undef, invalidSignal). The bounds are TEXT for the same reason.
CREATE TABLE type_exclusions (
    type_id   INTEGER NOT NULL REFERENCES data_types (id) ON DELETE CASCADE,
    position  INTEGER NOT NULL CHECK (position >= 0),
    kind      TEXT    NOT NULL,
    raw_lower TEXT    NOT NULL,
    raw_upper TEXT,
    locale    TEXT    NOT NULL DEFAULT '',
    label     TEXT    NOT NULL DEFAULT '',
    PRIMARY KEY (type_id, position, locale)
);

CREATE TABLE structures (
    id          INTEGER PRIMARY KEY,
    ecu_id      INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    name        TEXT    NOT NULL,
    qualifier   TEXT    NOT NULL DEFAULT '',
    byte_length INTEGER CHECK (byte_length IS NULL OR byte_length >= 0)
);

CREATE INDEX structures_name_idx ON structures (name);

CREATE TABLE structure_members (
    structure_id    INTEGER NOT NULL REFERENCES structures (id) ON DELETE CASCADE,
    position        INTEGER NOT NULL CHECK (position >= 0),
    name            TEXT    NOT NULL,
    data_type_id    INTEGER REFERENCES data_types (id) ON DELETE RESTRICT,
    bit_offset      INTEGER NOT NULL DEFAULT 0 CHECK (bit_offset >= 0),
    bit_length      INTEGER CHECK (bit_length IS NULL OR bit_length >= 0),
    repeat_count    INTEGER NOT NULL DEFAULT 1 CHECK (repeat_count >= 1),
    is_mux_selector INTEGER NOT NULL DEFAULT 0 CHECK (is_mux_selector IN (0, 1)),
    PRIMARY KEY (structure_id, position)
);

CREATE INDEX structure_members_name_idx ON structure_members (name);

CREATE TABLE dids (
    id            INTEGER PRIMARY KEY,
    ecu_id        INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    name          TEXT    NOT NULL,
    -- A UDS data identifier: 0x0000..=0xFFFF, stored as the integer itself.
    -- The UI renders it in hex; storing the hex text would make the sort order
    -- and the range check both wrong.
    identifier    INTEGER NOT NULL CHECK (identifier BETWEEN 0 AND 65535),
    data_type_id  INTEGER REFERENCES data_types (id) ON DELETE SET NULL,
    structure_id  INTEGER REFERENCES structures (id) ON DELETE SET NULL,
    session_ref   TEXT,
    security_ref  TEXT,
    UNIQUE (ecu_id, identifier)
);

CREATE INDEX dids_identifier_idx ON dids (identifier);
CREATE INDEX dids_name_idx ON dids (name);

-- A CDD's DID qualifier is an identifier the UI shows, not the row key, so it
-- lives beside the DID rather than in `dids` itself. Kept separate because two
-- DIDs may share a qualifier across variants.
CREATE TABLE did_qualifiers (
    did_id    INTEGER NOT NULL REFERENCES dids (id) ON DELETE CASCADE,
    qualifier TEXT    NOT NULL,
    PRIMARY KEY (did_id, qualifier)
);

-- A DID's payload structure, recorded member by member. The byte layout itself
-- is not built here: it needs the data-type graph (bit fields, MUX, variable
-- repetition), so the members are kept as read and the layout is computed when
-- those types are imported.
CREATE TABLE did_members (
    id            INTEGER PRIMARY KEY,
    did_id        INTEGER NOT NULL REFERENCES dids (id) ON DELETE CASCADE,
    position      INTEGER NOT NULL CHECK (position >= 0),
    name          TEXT    NOT NULL DEFAULT '',
    qualifier     TEXT    NOT NULL DEFAULT '',
    -- The CDD's dtref: the document-local id of the data type. Kept as text
    -- beside the resolved reference, because it points into the source document
    -- and must still be readable for a type this importer could not model -- a
    -- composite CONSTCOMP, say, which has no data_types row to point at.
    data_type_ref TEXT    NOT NULL DEFAULT '',
    -- The imported data type, when dtref resolved to one this importer mapped.
    -- NULL is a fact, not a gap: it says the member's type is not in the project
    -- yet, and the UI says so rather than showing an empty cell.
    data_type_id  INTEGER REFERENCES data_types (id) ON DELETE SET NULL,
    UNIQUE (did_id, position)
);

CREATE INDEX did_members_did_idx ON did_members (did_id, position);

CREATE TABLE dtcs (
    id                  INTEGER PRIMARY KEY,
    ecu_id              INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    code_hex            TEXT    NOT NULL,
    name                TEXT    NOT NULL,
    description         TEXT    NOT NULL DEFAULT '',
    severity            TEXT,
    functional_unit     TEXT,
    snapshot_record_id  INTEGER REFERENCES structures (id) ON DELETE SET NULL,
    extended_record_id  INTEGER REFERENCES structures (id) ON DELETE SET NULL,
    UNIQUE (ecu_id, code_hex)
);

CREATE INDEX dtcs_code_hex_idx ON dtcs (code_hex);
CREATE INDEX dtcs_name_idx ON dtcs (name);

CREATE TABLE diagnostic_classes (
    id        INTEGER PRIMARY KEY,
    ecu_id    INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    name      TEXT    NOT NULL,
    qualifier TEXT    NOT NULL DEFAULT '',
    UNIQUE (ecu_id, name)
);

CREATE INDEX diagnostic_classes_name_idx ON diagnostic_classes (name);

CREATE TABLE diagnostic_instances (
    id        INTEGER PRIMARY KEY,
    ecu_id    INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    class_id  INTEGER REFERENCES diagnostic_classes (id) ON DELETE CASCADE,
    name      TEXT    NOT NULL,
    qualifier TEXT    NOT NULL DEFAULT '',
    UNIQUE (ecu_id, name)
);

CREATE INDEX diagnostic_instances_name_idx ON diagnostic_instances (name);

CREATE TABLE services (
    id             INTEGER PRIMARY KEY,
    ecu_id         INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    instance_id    INTEGER REFERENCES diagnostic_instances (id) ON DELETE CASCADE,
    name           TEXT    NOT NULL,
    service_id_hex TEXT    NOT NULL,
    is_default     INTEGER NOT NULL DEFAULT 0 CHECK (is_default IN (0, 1)),
    session_ref    TEXT,
    security_ref   TEXT
);

CREATE INDEX services_name_idx ON services (name);
CREATE INDEX services_service_id_hex_idx ON services (service_id_hex);

CREATE TABLE telegrams (
    id             INTEGER PRIMARY KEY,
    service_id     INTEGER NOT NULL REFERENCES services (id) ON DELETE CASCADE,
    kind           TEXT    NOT NULL CHECK (kind IN ('request', 'positive', 'negative')),
    byte_position  INTEGER NOT NULL DEFAULT 0 CHECK (byte_position >= 0),
    structure_id   INTEGER REFERENCES structures (id) ON DELETE SET NULL,
    structure_name TEXT    NOT NULL DEFAULT '',
    UNIQUE (service_id, kind, byte_position)
);

CREATE TABLE states (
    id     INTEGER PRIMARY KEY,
    ecu_id INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    name   TEXT    NOT NULL,
    number INTEGER,
    UNIQUE (ecu_id, name)
);

CREATE INDEX states_name_idx ON states (name);

CREATE TABLE state_groups (
    id       INTEGER PRIMARY KEY,
    ecu_id   INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    name     TEXT    NOT NULL,
    kind     TEXT    NOT NULL CHECK (kind IN ('session', 'security')),
    state_id INTEGER REFERENCES states (id) ON DELETE SET NULL,
    UNIQUE (ecu_id, name, state_id)
);

CREATE INDEX state_groups_name_idx ON state_groups (name);

CREATE TABLE nrcs (
    id          INTEGER PRIMARY KEY,
    ecu_id      INTEGER NOT NULL REFERENCES ecus (id) ON DELETE CASCADE,
    code_hex    TEXT    NOT NULL,
    name        TEXT    NOT NULL,
    description TEXT    NOT NULL DEFAULT '',
    UNIQUE (ecu_id, code_hex)
);

CREATE INDEX nrcs_code_hex_idx ON nrcs (code_hex);
CREATE INDEX nrcs_name_idx ON nrcs (name);

CREATE TABLE variant_dids (
    variant_id INTEGER NOT NULL REFERENCES variants (id) ON DELETE CASCADE,
    did_id     INTEGER NOT NULL REFERENCES dids (id) ON DELETE CASCADE,
    PRIMARY KEY (variant_id, did_id)
);

CREATE INDEX variant_dids_did_idx ON variant_dids (did_id);

CREATE TABLE variant_dtcs (
    variant_id INTEGER NOT NULL REFERENCES variants (id) ON DELETE CASCADE,
    dtc_id     INTEGER NOT NULL REFERENCES dtcs (id) ON DELETE CASCADE,
    PRIMARY KEY (variant_id, dtc_id)
);

CREATE INDEX variant_dtcs_dtc_idx ON variant_dtcs (dtc_id);

CREATE TABLE import_issues (
    id         INTEGER PRIMARY KEY,
    severity   TEXT NOT NULL CHECK (severity IN ('info', 'warning', 'error')),
    rule       TEXT NOT NULL,
    object_ref TEXT,
    field      TEXT,
    message    TEXT NOT NULL
);

CREATE INDEX import_issues_rule_idx ON import_issues (rule);

CREATE TABLE schema_migrations (
    version    INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
