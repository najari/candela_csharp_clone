//! SQLite project storage.
//!
//! A project is a single `.diagproj` SQLite file tagged with
//! `application_id = 0x44494147` ("DIAG") and `user_version = 1`. The working
//! connection runs WAL + `synchronous=FULL`; every write goes through
//! `transaction()`, which rolls back when the closure returns an error.
//!
//! `open()` distinguishes three failure modes so the UI can tell them apart:
//! a file that is not SQLite at all or fails `quick_check` (Corrupt), a valid
//! SQLite file with a foreign `application_id` (NotAProject), and a project
//! from a newer engine (UnsupportedSchema). Files older than the current schema
//! are migrated on open.

use crate::EngineError;
use crate::cdd::LengthPrefix;
use crate::protocol::SCHEMA_VERSION;
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

/// `0x44494147` is ASCII "DIAG": the marker every diagauthor project carries.
pub const APPLICATION_ID: i64 = 0x4449_4147;
const SCHEMA_SQL: &str = include_str!("schema.sql");
/// Ordered `(version, ddl)` pairs. Add future schema versions here.
const MIGRATIONS: &[(i64, &str)] = &[(1, SCHEMA_SQL)];
const BUSY_TIMEOUT_MS: u64 = 5000;

/// Object kinds `objects.query` understands, in a stable order for messages.
pub const SUPPORTED_KINDS: &[&str] = &[
    "ecu",
    "variant",
    "data_type",
    "structure",
    "did",
    "dtc",
    "nrc",
    "class",
    "instance",
    "service",
    "state",
    "source_file",
];

pub struct Storage {
    path: PathBuf,
    connection: Connection,
    session_id: String,
}

impl Storage {
    /// Create a new project file. Refuses to clobber an existing non-empty file
    /// unless `overwrite` is set.
    pub fn create(path: &Path, overwrite: bool) -> Result<Self, EngineError> {
        let existed = path.exists();
        if existed {
            let size = std::fs::metadata(path)
                .map_err(|e| io_error(path, &e))?
                .len();
            if size > 0 && !overwrite {
                return Err(EngineError::ProjectExists(path.to_path_buf()));
            }
            std::fs::remove_file(path).map_err(|e| io_error(path, &e))?;
        }
        let connection = Connection::open(path).map_err(|e| EngineError::Io {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        configure(&connection)?;
        connection.pragma_update(None, "application_id", APPLICATION_ID)?;
        let mut storage = Self {
            path: path.to_path_buf(),
            connection,
            session_id: Uuid::new_v4().to_string(),
        };
        if let Err(error) = storage.migrate() {
            drop(storage);
            if !existed {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
        Ok(storage)
    }

    /// Open an existing project, validating `application_id`, `user_version` and
    /// `quick_check`, then applying any pending migrations.
    pub fn open(path: &Path) -> Result<Self, EngineError> {
        if !path.is_file() {
            return Err(EngineError::Io {
                path: path.to_path_buf(),
                detail: "project file does not exist".to_string(),
            });
        }
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|e| classify_read_failure(path, &e))?;
        let found_app =
            pragma_i64(&connection, "application_id").map_err(|e| classify_read_failure(path, &e))?;
        if found_app != APPLICATION_ID {
            return Err(EngineError::NotAProject {
                path: path.display().to_string(),
                found: found_app,
                expected: APPLICATION_ID,
            });
        }
        let found_version =
            pragma_i64(&connection, "user_version").map_err(|e| classify_read_failure(path, &e))?;
        if found_version > SCHEMA_VERSION {
            return Err(EngineError::UnsupportedSchema {
                found: found_version,
                expected: SCHEMA_VERSION,
            });
        }
        let check: String = connection
            .query_row("PRAGMA quick_check", [], |row| row.get(0))
            .map_err(|e| classify_read_failure(path, &e))?;
        if check != "ok" {
            return Err(EngineError::Corrupt {
                path: path.display().to_string(),
                detail: format!("integrity check failed: {check}"),
            });
        }
        configure(&connection)?;
        let mut storage = Self {
            path: path.to_path_buf(),
            connection,
            session_id: Uuid::new_v4().to_string(),
        };
        storage.migrate()?;
        Ok(storage)
    }

    /// Apply every migration newer than the file's `user_version`.
    pub fn migrate(&mut self) -> Result<Vec<i64>, EngineError> {
        let mut applied = Vec::new();
        for (version, ddl) in MIGRATIONS {
            if *version <= self.schema_version()? {
                continue;
            }
            let tx = self.connection.transaction()?;
            tx.execute_batch(ddl)?;
            tx.pragma_update(None, "user_version", *version)?;
            tx.execute(
                "INSERT OR REPLACE INTO schema_migrations(version) VALUES (?1)",
                [version],
            )?;
            tx.commit()?;
            applied.push(*version);
        }
        Ok(applied)
    }

    /// Write a standalone single-file snapshot (DELETE journal, no sidecars) to
    /// `destination` via a temp file in the same directory, then rename.
    pub fn save_as(&self, destination: &Path) -> Result<(), EngineError> {
        let directory = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let temp = directory.join(format!(".diagauthor-save-{}.tmp", Uuid::new_v4()));
        if let Err(error) = self
            .connection
            .execute("VACUUM INTO ?1", params![temp.display().to_string()])
        {
            let _ = std::fs::remove_file(&temp);
            return Err(EngineError::Database(error));
        }
        if let Err(error) = publish_snapshot(&temp, destination) {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
        Ok(())
    }

    /// Run `work` inside one transaction; an error rolls the whole thing back.
    pub fn transaction<T, F>(&mut self, work: F) -> Result<T, EngineError>
    where
        F: FnOnce(&Transaction<'_>) -> Result<T, EngineError>,
    {
        let tx = self.connection.transaction()?;
        let value = work(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn schema_version(&self) -> Result<i64, EngineError> {
        Ok(pragma_i64(&self.connection, "user_version")?)
    }

    /// Record an imported source file's original bytes.
    ///
    /// The bytes are stored verbatim: an export must be able to reproduce
    /// anything the importer did not understand (schema.sql design note).
    pub fn record_source_file(
        &self,
        path: &str,
        kind: &str,
        sha256: &str,
        content: &[u8],
    ) -> Result<i64, EngineError> {
        self.connection.execute(
            "INSERT INTO source_files (path, kind, sha256, byte_size, content) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![path, kind, sha256, content.len() as i64, content],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Link a domain object back to the XML node it came from.
    pub fn map_source_node(
        &self,
        object_id: i64,
        source_file_id: i64,
        xpath: &str,
        element_name: &str,
    ) -> Result<(), EngineError> {
        self.connection.execute(
            "INSERT OR REPLACE INTO source_mappings (object_id, source_file_id, xpath, element_name) VALUES (?1, ?2, ?3, ?4)",
            params![object_id.to_string(), source_file_id, xpath, element_name],
        )?;
        Ok(())
    }

    /// Insert the ECU row and return its id.
    ///
    /// `qualifier` is UNIQUE in the schema, and an import can legitimately
    /// carry a duplicate or empty one. Rather than failing the whole import on a
    /// name collision, the qualifier is made unique here -- a qualifier is an
    /// identifier, not data the user reads.
    pub fn insert_ecu(
        &self,
        name: &str,
        qualifier: &str,
        manufacturer: &str,
    ) -> Result<i64, EngineError> {
        let base = if qualifier.trim().is_empty() {
            "ImportedECU"
        } else {
            qualifier.trim()
        };
        let qualifier = self.unique_qualifier("ecus", "qualifier", base)?;
        self.connection.execute(
            "INSERT INTO ecus (name, qualifier, manufacturer) VALUES (?1, ?2, ?3)",
            params![name, qualifier, manufacturer],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Find a qualifier that is not yet taken in `table`, appending a counter.
    ///
    /// The first candidate is tried unchanged so a document's own qualifier is
    /// preserved whenever it does not collide.
    fn unique_qualifier(
        &self,
        table: &str,
        column: &str,
        base: &str,
    ) -> Result<String, EngineError> {
        let sql = format!("SELECT 1 FROM {table} WHERE {column} = ?1 LIMIT 1");
        let taken = |value: &str| -> Result<bool, EngineError> {
            match self.connection.query_row(&sql, params![value], |_| Ok(())) {
                Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
                Err(error) => Err(EngineError::Database(error)),
                Ok(()) => Ok(true),
            }
        };
        if !taken(base)? {
            return Ok(base.to_string());
        }
        for suffix in 2..10_000u32 {
            let candidate = format!("{base}{suffix}");
            if !taken(&candidate)? {
                return Ok(candidate);
            }
        }
        Err(EngineError::Unsupported(format!(
            "could not find an unused {column} derived from '{base}' in {table}"
        )))
    }

    /// Insert a variant and return its id. `is_base` marks the document's base
    /// variant, which the plan requires to be exactly one.
    pub fn insert_variant(
        &self,
        ecu_id: i64,
        name: &str,
        is_base: bool,
    ) -> Result<i64, EngineError> {
        self.connection.execute(
            "INSERT INTO variants (ecu_id, name, is_base) VALUES (?1, ?2, ?3)",
            params![ecu_id, name, i64::from(is_base)],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Insert a DID and return its id.
    ///
    /// The schema is `UNIQUE (ecu_id, identifier)`. A document may legitimately
    /// repeat a DID across variants, so the caller supplies one identifier per
    /// document and the uniqueness is resolved here rather than failing the
    /// import over a collision.
    pub fn insert_did(
        &self,
        ecu_id: i64,
        name: &str,
        identifier: u32,
    ) -> Result<i64, EngineError> {
        // A duplicate identifier inside one ECU is a document defect, not a
        // reason to renumber: a renumbered DID answers a different request. The
        // caller records the loss and the import continues.
        if identifier > crate::cdd::DID_MAX {
            return Err(EngineError::Unsupported(format!(
                "DID identifier {identifier} is outside the 16-bit range"
            )));
        }
        let identifier = i64::from(identifier);
        self.connection.execute(
            "INSERT INTO dids (ecu_id, name, identifier) VALUES (?1, ?2, ?3)",
            params![ecu_id, name, identifier],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Record a DID's document qualifier beside the DID.
    pub fn set_did_qualifier(&self, did_id: i64, qualifier: &str) -> Result<(), EngineError> {
        let qualifier = qualifier.trim();
        if qualifier.is_empty() {
            return Ok(());
        }
        self.connection.execute(
            "INSERT OR IGNORE INTO did_qualifiers (did_id, qualifier) VALUES (?1, ?2)",
            params![did_id, qualifier],
        )?;
        Ok(())
    }

    /// Link a DID to a variant. The pair is the join table's whole content, so a
    /// repeat is a no-op rather than an error.
    pub fn link_variant_did(&self, variant_id: i64, did_id: i64) -> Result<(), EngineError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO variant_dids (variant_id, did_id) VALUES (?1, ?2)",
            params![variant_id, did_id],
        )?;
        Ok(())
    }

    /// Insert a DID member and return its id.
    ///
    /// `data_type_id` is `None` when the member's `dtref` pointed at a type this
    /// importer could not model. That is a real state, not a failure: the
    /// composite types a DID structure usually points at are not imported yet,
    /// and pretending otherwise would show a member with a type it does not have.
    pub fn insert_did_member(
        &self,
        did_id: i64,
        position: i64,
        name: &str,
        qualifier: &str,
        data_type_ref: &str,
        data_type_id: Option<i64>,
    ) -> Result<i64, EngineError> {
        self.connection.execute(
            "INSERT INTO did_members (did_id, position, name, qualifier, data_type_ref, data_type_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                did_id,
                position,
                name,
                qualifier,
                data_type_ref,
                data_type_id
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Insert a simple data type and return its id.
    ///
    /// Uniqueness is on the document-local `id` and nothing else, because a
    /// document may name many types the same thing -- `cantools_example.cdd` has
    /// 59 data types all called `LocalTable`, so a name or qualifier key would
    /// reject a valid file. The id is what a member's `dtref` resolves against,
    /// so a repeat of it is refused by the schema and reported by the caller
    /// rather than making that reference ambiguous.
    pub fn insert_data_type(
        &self,
        ecu_id: i64,
        data_type: &crate::cdd::CddDataType,
    ) -> Result<i64, EngineError> {
        let linear = data_type.linear.as_ref();
        let formula = data_type.formula.as_ref();
        self.connection.execute(
            "INSERT INTO data_types (
                 ecu_id, name, qualifier, kind, source_ref, conversion, raw_encoding,
                 quantity, bit_length, byte_order, length_prefix_bits, min_length,
                 max_length, bit_mask, factor_text, offset_text, raw_lower,
                 raw_upper, formula_expression, formula_usage, unit
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                       ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                ecu_id,
                data_type.name,
                data_type.qualifier,
                data_type.kind.element(),
                data_type.id,
                data_type.conversion.as_str(),
                data_type.raw_encoding.as_str(),
                data_type.quantity.as_str(),
                i64::from(data_type.bit_length),
                if data_type.big_endian { "big" } else { "little" },
                data_type.length_prefix.and_then(LengthPrefix::bits).map(i64::from),
                data_type.min_length.map(i64::from),
                data_type.max_length.map(i64::from),
                data_type.bit_mask.map(|mask| mask.to_string()),
                // A type with no linear conversion still has the identity scale,
                // which is what the document's silence says.
                linear.map(|l| l.factor.as_str()).unwrap_or("1"),
                linear.map(|l| l.offset.as_str()).unwrap_or("0"),
                linear.and_then(|l| l.raw_lower).map(|v| v.to_string()),
                linear.and_then(|l| l.raw_upper).map(|v| v.to_string()),
                formula.map(|f| f.expression.as_str()),
                formula.map(|f| f.usage.as_str()),
                data_type.unit,
            ],
        )?;
        let type_id = self.connection.last_insert_rowid();

        // Table entries keep their TEXTMAP position and their locale. A TEXTMAP
        // with four TUVs becomes four rows at one position, which is what lets a
        // UI switch language without the other three being lost.
        for entry in &data_type.entries {
            self.connection.execute(
                "INSERT INTO type_table_entries (type_id, position, locale, raw_lower, raw_upper, label) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    type_id,
                    entry.position as i64,
                    entry.language,
                    entry.range.start.to_string(),
                    entry.range.end.to_string(),
                    entry.text
                ],
            )?;
        }
        for (index, exclusion) in data_type.exclusions.iter().enumerate() {
            self.connection.execute(
                "INSERT INTO type_exclusions (type_id, position, kind, raw_lower, raw_upper, locale, label) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    type_id,
                    index as i64,
                    exclusion.inverse,
                    exclusion.range.start.to_string(),
                    exclusion.range.end.to_string(),
                    exclusion.language,
                    exclusion.text
                ],
            )?;
        }
        Ok(type_id)
    }

    /// Map document-local type ids to imported row ids for one ECU.
    ///
    /// A DID member's `dtref` is a document-local id, so resolving it means
    /// looking up what that id became. Only the ids that resolved are in the map:
    /// a member whose type was refused must stay unresolved rather than be
    /// pointed at whatever happens to share its number.
    pub fn data_type_ids_by_source_ref(
        &self,
        ecu_id: i64,
    ) -> Result<HashMap<String, i64>, EngineError> {
        let mut statement = self
            .connection
            .prepare("SELECT source_ref, id FROM data_types WHERE ecu_id = ?1 AND source_ref <> ''")?;
        let rows = statement.query_map(params![ecu_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut map = HashMap::new();
        for row in rows {
            let (source_ref, id) = row?;
            map.entry(source_ref).or_insert(id);
        }
        Ok(map)
    }

    /// Record an import finding and return its id.
    pub fn add_import_issue(
        &self,
        severity: &str,
        rule: &str,
        object_ref: Option<&str>,
        field: Option<&str>,
        message: &str,
    ) -> Result<i64, EngineError> {
        self.connection.execute(
            "INSERT INTO import_issues (severity, rule, object_ref, field, message) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![severity, rule, object_ref, field, message],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<(), EngineError> {
        self.connection.execute(
            "INSERT INTO project_meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>, EngineError> {
        Ok(self
            .connection
            .query_row("SELECT value FROM project_meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// M1 projects have exactly one ECU; this resets it to `name`.
    pub fn set_primary_ecu(&mut self, name: &str) -> Result<(), EngineError> {
        let tx = self.connection.transaction()?;
        tx.execute("DELETE FROM ecus", [])?;
        tx.execute(
            "INSERT INTO ecus(name, qualifier) VALUES (?1, ?1)",
            params![name],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn primary_ecu(&self) -> Result<Option<String>, EngineError> {
        Ok(self
            .connection
            .query_row("SELECT name FROM ecus ORDER BY id LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// Build the navigation tree: ecu -> variants (dids/dtcs) and classes
    /// (instances); dids/dtcs not linked to any variant hang off the ECU.
    pub fn tree(&self) -> Result<Value, EngineError> {
        let mut nodes = Vec::new();
        let ecus = self.name_rows("SELECT id, name FROM ecus ORDER BY name, id", [])?;
        for (ecu_id, ecu_name) in ecus {
            let mut children = Vec::new();
            let variants = self.name_rows(
                "SELECT id, name FROM variants WHERE ecu_id = ?1 ORDER BY name, id",
                [ecu_id],
            )?;
            for (variant_id, variant_name) in variants {
                let mut variant_children = Vec::new();
                let dids = self.name_rows(
                    "SELECT d.id, d.name FROM dids d
                     JOIN variant_dids vd ON vd.did_id = d.id
                     WHERE vd.variant_id = ?1 ORDER BY d.name, d.id",
                    [variant_id],
                )?;
                for (did_id, did_name) in dids {
                    variant_children.push(node_json(format!("did:{did_id}"), "did", &did_name, &[]));
                }
                let dtcs = self.name_rows(
                    "SELECT d.id, d.name FROM dtcs d
                     JOIN variant_dtcs vd ON vd.dtc_id = d.id
                     WHERE vd.variant_id = ?1 ORDER BY d.name, d.id",
                    [variant_id],
                )?;
                for (dtc_id, dtc_name) in dtcs {
                    variant_children.push(node_json(format!("dtc:{dtc_id}"), "dtc", &dtc_name, &[]));
                }
                children.push(node_json(
                    format!("variant:{variant_id}"),
                    "variant",
                    &variant_name,
                    &variant_children,
                ));
            }
            let classes = self.name_rows(
                "SELECT id, name FROM diagnostic_classes WHERE ecu_id = ?1 ORDER BY name, id",
                [ecu_id],
            )?;
            for (class_id, class_name) in classes {
                let instances = self.name_rows(
                    "SELECT id, name FROM diagnostic_instances WHERE class_id = ?1 ORDER BY name, id",
                    [class_id],
                )?;
                let instance_nodes: Vec<Value> = instances
                    .into_iter()
                    .map(|(instance_id, instance_name)| {
                        node_json(format!("instance:{instance_id}"), "instance", &instance_name, &[])
                    })
                    .collect();
                children.push(node_json(
                    format!("class:{class_id}"),
                    "class",
                    &class_name,
                    &instance_nodes,
                ));
            }
            let loose_dids = self.name_rows(
                "SELECT d.id, d.name FROM dids d
                 WHERE d.ecu_id = ?1
                   AND NOT EXISTS (SELECT 1 FROM variant_dids vd WHERE vd.did_id = d.id)
                 ORDER BY d.name, d.id",
                [ecu_id],
            )?;
            for (did_id, did_name) in loose_dids {
                children.push(node_json(format!("did:{did_id}"), "did", &did_name, &[]));
            }
            let loose_dtcs = self.name_rows(
                "SELECT d.id, d.name FROM dtcs d
                 WHERE d.ecu_id = ?1
                   AND NOT EXISTS (SELECT 1 FROM variant_dtcs vd WHERE vd.dtc_id = d.id)
                 ORDER BY d.name, d.id",
                [ecu_id],
            )?;
            for (dtc_id, dtc_name) in loose_dtcs {
                children.push(node_json(format!("dtc:{dtc_id}"), "dtc", &dtc_name, &[]));
            }
            nodes.push(node_json(format!("ecu:{ecu_id}"), "ecu", &ecu_name, &children));
        }
        Ok(json!({ "nodes": nodes }))
    }

    /// Paged, case-insensitive substring query for one object kind. The caller
    /// clamps `limit`/`offset`; `kind` must be in `SUPPORTED_KINDS`.
    pub fn object_rows(
        &self,
        kind: &str,
        filter: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Value, EngineError> {
        let spec = kind_spec(kind).ok_or_else(|| {
            EngineError::InvalidArgs(format!(
                "unknown object kind '{kind}'; supported kinds: {}",
                SUPPORTED_KINDS.join(", ")
            ))
        })?;
        // Every filterable column is matched as text, because a user types
        // "0xF190" as readily as "F190" or "61840", and an integer column would
        // match none of them. A hex-shaped term is matched against the hex
        // rendering, so a DID is findable by the form the UI displays.
        let mut filter_terms = spec
            .filter_cols
            .iter()
            .map(|column| {
                format!("LOWER(COALESCE(CAST({column} AS TEXT), '')) LIKE '%' || LOWER(?1) || '%'")
            })
            .collect::<Vec<_>>();
        if kind == "did" {
            filter_terms.push(
                "(printf('0x%04x', identifier) LIKE '%' || LOWER(?1) || '%')".to_string(),
            );
        }
        let filter_sql = filter_terms.join(" OR ");
        // The filter matches every projected column as text, because a user
        // types "0xF190" as well as "F190" and a numeric column would not match
        // either. The cast is in the SELECT too, so the value that reaches the
        // client is typed from what SQLite actually returned rather than from
        // what the column was declared as.
        let extras: Vec<String> = spec
            .extras
            .iter()
            .map(|column| format!("CAST({column} AS TEXT)"))
            .collect();
        let projection = if extras.is_empty() {
            "id, name".to_string()
        } else {
            format!("id, name, {}", extras.join(", "))
        };
        let sql = format!(
            "SELECT {projection} FROM {} WHERE {filter_sql} ORDER BY name, id LIMIT ?2 OFFSET ?3",
            spec.table
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params![filter, limit, offset], |row| {
            let mut object = serde_json::Map::new();
            object.insert("id".to_string(), json!(row.get::<_, i64>(0)?));
            object.insert("kind".to_string(), json!(kind));
            object.insert("name".to_string(), json!(row.get::<_, String>(1)?));
            for (index, column) in spec.extras.iter().enumerate() {
                let text = row.get::<_, Option<String>>(index + 2)?;
                // A DID identifier is an integer in the schema. It arrives here
                // as text because the filter needed the cast, so it is parsed
                // back: a client that receives "61840" where the contract says
                // 61840 cannot sort or range-check it.
                if kind == "did" && *column == "identifier" {
                    let value = match text.as_deref() {
                        Some(raw) => raw
                            .parse::<i64>()
                            .map(Value::from)
                            .unwrap_or(Value::Null),
                        None => Value::Null,
                    };
                    object.insert((*column).to_string(), value.clone());
                    if let Some(number) = value.as_i64() {
                        // The UI reads the number in hex; both forms travel so
                        // the shell need not reimplement the conversion.
                        object.insert(
                            "identifier_hex".to_string(),
                            json!(format!("0x{number:04X}")),
                        );
                    }
                } else {
                    object.insert((*column).to_string(), json!(text));
                }
            }
            Ok(Value::Object(object))
        })?;
        let rows: Vec<Value> = rows.collect::<Result<_, _>>()?;
        let total: i64 = self.connection.query_row(
            &format!("SELECT COUNT(*) FROM {} WHERE {filter_sql}", spec.table),
            params![filter],
            |row| row.get(0),
        )?;
        Ok(json!({ "rows": rows, "total": total }))
    }

    fn name_rows(
        &self,
        sql: &str,
        param: impl rusqlite::Params,
    ) -> Result<Vec<(i64, String)>, EngineError> {
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map(param, |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

/// Reject an unknown `objects.query` kind with the supported list.
pub fn validate_kind(kind: &str) -> Result<(), EngineError> {
    if kind_spec(kind).is_some() {
        return Ok(());
    }
    Err(EngineError::InvalidArgs(format!(
        "unknown object kind '{kind}'; supported kinds: {}",
        SUPPORTED_KINDS.join(", ")
    )))
}

#[derive(Debug, Clone, Copy)]
struct KindSpec {
    table: &'static str,
    extras: &'static [&'static str],
    filter_cols: &'static [&'static str],
}

fn kind_spec(kind: &str) -> Option<KindSpec> {
    let spec = match kind {
        "ecu" => KindSpec {
            table: "ecus",
            extras: &["qualifier"],
            filter_cols: &["name", "qualifier"],
        },
        "variant" => KindSpec {
            table: "variants",
            extras: &["qualifier"],
            filter_cols: &["name", "qualifier"],
        },
        "data_type" => KindSpec {
            table: "data_types",
            extras: &["kind", "qualifier"],
            // `kind` is filterable because it is the one column that says what
            // the type *is*: a user looking for "every TEXTTBL" types that, and
            // there is no other way to ask.
            filter_cols: &["name", "qualifier", "kind"],
        },
        "structure" => KindSpec {
            table: "structures",
            extras: &["qualifier"],
            filter_cols: &["name", "qualifier"],
        },
        "did" => KindSpec {
            table: "dids",
            extras: &["identifier"],
            filter_cols: &["name", "identifier"],
        },
        "dtc" => KindSpec {
            table: "dtcs",
            extras: &["code_hex"],
            filter_cols: &["name", "code_hex"],
        },
        "nrc" => KindSpec {
            table: "nrcs",
            extras: &["code_hex"],
            filter_cols: &["name", "code_hex"],
        },
        "class" => KindSpec {
            table: "diagnostic_classes",
            extras: &["qualifier"],
            filter_cols: &["name", "qualifier"],
        },
        "instance" => KindSpec {
            table: "diagnostic_instances",
            extras: &["qualifier"],
            filter_cols: &["name", "qualifier"],
        },
        "service" => KindSpec {
            table: "services",
            extras: &["service_id_hex"],
            filter_cols: &["name", "service_id_hex"],
        },
        "state" => KindSpec {
            table: "states",
            extras: &["number"],
            filter_cols: &["name"],
        },
        "source_file" => KindSpec {
            table: "source_files",
            extras: &["path", "kind"],
            filter_cols: &["path", "kind"],
        },
        _ => return None,
    };
    Some(spec)
}

fn node_json(id: String, kind: &str, label: &str, children: &[Value]) -> Value {
    json!({
        "id": id,
        "kind": kind,
        "label": label,
        "child_count": children.len(),
        "children": children,
    })
}

fn configure(connection: &Connection) -> Result<(), EngineError> {
    connection.busy_timeout(Duration::from_millis(BUSY_TIMEOUT_MS))?;
    connection.execute_batch(
        "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;",
    )?;
    Ok(())
}

fn pragma_i64(connection: &Connection, name: &str) -> Result<i64, rusqlite::Error> {
    connection.query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
}

/// Normalize the snapshot and rename it over the destination. Any failure leaves
/// the destination untouched (the caller removes the temp file).
fn publish_snapshot(temp: &Path, destination: &Path) -> Result<(), EngineError> {
    {
        let snapshot = Connection::open(temp)?;
        // Explicit save is one standalone file: no journal sidecars.
        snapshot.execute_batch("PRAGMA journal_mode=DELETE;")?;
        let check: String = snapshot.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        if check != "ok" {
            return Err(EngineError::Corrupt {
                path: destination.display().to_string(),
                detail: format!("snapshot integrity check failed: {check}"),
            });
        }
    }
    std::fs::rename(temp, destination).map_err(|e| io_error(destination, &e))?;
    Ok(())
}

fn classify_read_failure(path: &Path, error: &rusqlite::Error) -> EngineError {
    match error {
        rusqlite::Error::SqliteFailure(inner, _)
            if inner.code == rusqlite::ErrorCode::NotADatabase =>
        {
            EngineError::Corrupt {
                path: path.display().to_string(),
                detail: "not a SQLite database".to_string(),
            }
        }
        other => EngineError::Corrupt {
            path: path.display().to_string(),
            detail: other.to_string(),
        },
    }
}

/// SHA-256-shaped content identifier for a delivered file, so a save receipt can
/// be tied to exact bytes.
///
/// This is FNV-1a widened to 128 bits by two independently seeded passes, not a
/// cryptographic hash: it identifies a file's content for receipt comparison and
/// is used for neither integrity nor authentication. The plan requires a
/// `file_hash` in the save receipt (4절) but does not require a specific
/// algorithm, and the engine ships without a hash dependency.
pub fn hash_file(path: &Path) -> Result<String, EngineError> {
    let bytes = std::fs::read(path).map_err(|e| EngineError::Io {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    Ok(hex_digest(&bytes))
}

fn hex_digest(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut a = OFFSET;
    let mut b = OFFSET ^ 0x9e37_79b9_7f4a_7c15;
    for (index, byte) in bytes.iter().enumerate() {
        a ^= u64::from(*byte).wrapping_add(index as u64);
        a = a.wrapping_mul(PRIME);
        b = b.rotate_left(7) ^ a;
        b = b.wrapping_mul(PRIME);
    }
    format!("{a:016x}{b:016x}")
}

fn io_error(path: &Path, error: &std::io::Error) -> EngineError {
    EngineError::Io {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("unit.diagproj");
        (dir, path)
    }

    #[test]
    fn create_write_reopen_validates_header() {
        let (_dir, path) = temp_project();
        {
            let mut storage = Storage::create(&path, false).unwrap();
            assert_eq!(storage.schema_version().unwrap(), SCHEMA_VERSION);
            storage.set_meta("name", "unit").unwrap();
            storage.set_primary_ecu("UnitECU").unwrap();
            assert_eq!(storage.primary_ecu().unwrap().as_deref(), Some("UnitECU"));
        }
        let storage = Storage::open(&path).unwrap();
        assert_eq!(storage.meta("name").unwrap().as_deref(), Some("unit"));
        assert_eq!(storage.primary_ecu().unwrap().as_deref(), Some("UnitECU"));
        let check = pragma_i64(&storage.connection, "application_id").unwrap();
        assert_eq!(check, APPLICATION_ID);
    }

    #[test]
    fn create_refuses_to_clobber_without_overwrite() {
        let (_dir, path) = temp_project();
        std::fs::write(&path, b"do not lose me").unwrap();
        match Storage::create(&path, false) {
            Ok(_) => panic!("create must not clobber a non-empty file"),
            Err(error) => assert!(matches!(error, EngineError::ProjectExists(_)), "{error}"),
        }
        assert_eq!(std::fs::read(&path).unwrap(), b"do not lose me");
        let overwritten = Storage::create(&path, true);
        assert!(overwritten.is_ok());
    }

    #[test]
    fn foreign_application_id_is_not_a_project_and_junk_is_corruption() {
        let (dir, _) = temp_project();
        let alien = dir.path().join("alien.sqlite");
        {
            let connection = Connection::open(&alien).unwrap();
            connection
                .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
                .unwrap();
        }
        match Storage::open(&alien) {
            Ok(_) => panic!("a foreign SQLite file must not open as a project"),
            Err(error) => {
                assert!(matches!(error, EngineError::NotAProject { .. }), "{error}");
                let message = error.to_string();
                assert!(message.contains("application_id"), "{message}");
            }
        }
        let junk = dir.path().join("junk.bin");
        std::fs::write(&junk, [0u8; 64]).unwrap();
        match Storage::open(&junk) {
            Ok(_) => panic!("a zero-filled file must not open as a project"),
            Err(error) => assert!(matches!(error, EngineError::Corrupt { .. }), "{error}"),
        }
        let missing = dir.path().join("missing.diagproj");
        match Storage::open(&missing) {
            Ok(_) => panic!("missing files must not be created by open"),
            Err(error) => assert!(matches!(error, EngineError::Io { .. }), "{error}"),
        }
    }

    #[test]
    fn future_schema_version_is_rejected_and_old_schema_is_migrated() {
        let (dir, _) = temp_project();
        let future = dir.path().join("future.diagproj");
        {
            let connection = Connection::open(&future).unwrap();
            connection
                .pragma_update(None, "application_id", APPLICATION_ID)
                .unwrap();
            connection.pragma_update(None, "user_version", 99i64).unwrap();
            connection.execute_batch("CREATE TABLE placeholder(x);").unwrap();
        }
        match Storage::open(&future) {
            Ok(_) => panic!("a newer schema must be rejected"),
            Err(error) => {
                assert!(matches!(error, EngineError::UnsupportedSchema { found: 99, .. }), "{error}")
            }
        }

        let legacy = dir.path().join("legacy.diagproj");
        {
            let connection = Connection::open(&legacy).unwrap();
            connection
                .pragma_update(None, "application_id", APPLICATION_ID)
                .unwrap();
        }
        let storage = Storage::open(&legacy).unwrap();
        assert_eq!(storage.schema_version().unwrap(), SCHEMA_VERSION);
        storage.set_meta("migrated", "yes").unwrap();
        assert_eq!(storage.meta("migrated").unwrap().as_deref(), Some("yes"));
    }

    #[test]
    fn transaction_rollback_leaves_no_partial_rows() {
        let (_dir, path) = temp_project();
        let mut storage = Storage::create(&path, false).unwrap();
        let rolled_back: Result<(), EngineError> = storage.transaction(|tx| {
            tx.execute("INSERT INTO project_meta(key, value) VALUES ('probe_a', '1')", [])?;
            tx.execute("INSERT INTO project_meta(key, value) VALUES ('probe_b', '2')", [])?;
            Err(EngineError::InvalidArgs("intentional rollback".to_string()))
        });
        assert!(rolled_back.is_err());
        assert_eq!(storage.meta("probe_a").unwrap(), None);
        assert_eq!(storage.meta("probe_b").unwrap(), None);

        let committed = storage.transaction(|tx| {
            tx.execute("INSERT INTO project_meta(key, value) VALUES ('probe_c', '3')", [])?;
            Ok(1234_i64)
        });
        assert_eq!(committed.unwrap(), 1234);
        assert_eq!(storage.meta("probe_c").unwrap().as_deref(), Some("3"));
    }

    #[test]
    fn atomic_save_publishes_a_single_file_snapshot() {
        let (dir, path) = temp_project();
        let storage = Storage::create(&path, false).unwrap();
        storage.set_meta("name", "snapshot").unwrap();
        let destination = dir.path().join("saved.diagproj");
        storage.save_as(&destination).unwrap();
        assert!(destination.is_file());
        assert!(!dir.path().join("saved.diagproj-wal").exists());
        assert!(!dir.path().join("saved.diagproj-shm").exists());

        {
            let connection = Connection::open(&destination).unwrap();
            let app: i64 = connection
                .query_row("PRAGMA application_id", [], |row| row.get(0))
                .unwrap();
            let version: i64 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            let mode: String = connection
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            let check: String = connection
                .query_row("PRAGMA quick_check", [], |row| row.get(0))
                .unwrap();
            assert_eq!(app, APPLICATION_ID);
            assert_eq!(version, SCHEMA_VERSION);
            assert_eq!(mode, "delete");
            assert_eq!(check, "ok");
        }

        // Saving again replaces the destination atomically with fresh content.
        storage.set_meta("name", "snapshot-2").unwrap();
        storage.save_as(&destination).unwrap();
        let reopened = Storage::open(&destination).unwrap();
        assert_eq!(reopened.meta("name").unwrap().as_deref(), Some("snapshot-2"));
    }

    #[test]
    fn object_rows_returns_well_formed_rows_and_rejects_unknown_kinds() {
        let (_dir, path) = temp_project();
        let mut storage = Storage::create(&path, false).unwrap();
        storage.set_primary_ecu("RowECU").unwrap();
        storage
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO dids(ecu_id, name, identifier) VALUES (1, 'Alpha', 61840)",
                    [],
                )?;
                tx.execute(
                    "INSERT INTO dids(ecu_id, name, identifier) VALUES (1, 'beta', 61841)",
                    [],
                )?;
                Ok(())
            })
            .unwrap();
        let all = storage.object_rows("did", "", 200, 0).unwrap();
        assert_eq!(all["total"], json!(2));
        let first = &all["rows"][0];
        assert_eq!(first["id"], json!(1));
        assert_eq!(first["kind"], json!("did"));
        assert_eq!(first["name"], json!("Alpha"));
        // The identifier is the integer itself, as the reference stores it. The
        // UI renders it in hex; the wire form stays decimal so the value sorts
        // and range-checks as a number.
        assert_eq!(first["identifier"], json!(61840));
        assert_eq!(first["identifier_hex"], json!("0xF190"));
        let filtered = storage.object_rows("did", "ALPHA", 200, 0).unwrap();
        assert_eq!(filtered["total"], json!(1));
        assert_eq!(filtered["rows"][0]["name"], json!("Alpha"));
        let error = storage.object_rows("wombat", "", 200, 0).unwrap_err();
        assert!(matches!(error, EngineError::InvalidArgs(_)), "{error}");
    }
}
