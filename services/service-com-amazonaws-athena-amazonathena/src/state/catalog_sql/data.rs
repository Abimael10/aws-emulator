// SPDX-License-Identifier: CC0-1.0

//! The DATA under a catalog table: the objects at the table's location, read as the rows
//! its SerDe says they hold.
//!
//! A Data Catalog table says where its data lives (`StorageDescriptor$Location`) and how
//! to read it (`StorageDescriptor$SerdeInfo` plus the table's own properties), and
//! Athena's answer to a query over that table is computed from the OBJECTS at that
//! location. So the rows a query reads here are the bytes the object store holds, decoded
//! by the SerDe the catalog recorded — never values synthesised from the query or from the
//! table's definition.

/// The records one object of the columnar Parquet format holds.
pub(super) mod parquet_objects;
/// The compression one TEXT object at a table's location carries.
mod text_codecs;

use ::std::borrow::Cow;
use ::std::collections::{HashMap, HashSet};

use ::service_com_amazonaws_glue_awsglue::state as glue_state;
use ::service_com_amazonaws_s3_amazons3::state as s3_state;

use super::ddl;
use super::hive_types;
use super::statement::{self, DeclaredColumn, Relation};
use super::Failure;
use crate::state::lake_formation::ReadScope;
use crate::state::query_engine::{self, LocalColumn, LocalObject, LocalTable};

/// The URI scheme a table's location names its data by.
const S3_SCHEME: &str = "s3://";

/// The table property that says how many lines at the head of each object are a header
/// rather than data, as Hive names it.
const SKIP_HEADER_PROPERTY: &str = "skip.header.line.count";

/// The SerDe parameters that carry the character fields are separated by, in the order
/// Hive reads them: the field delimiter, and the serialization format a table that names
/// only that one falls back to. Both names are the SIBLING's — the one the DDL writes
/// them under — so a writer and a reader of one parameter can never spell it differently.
const FIELD_DELIMITER_PARAMETERS: &[&str] = &[
    statement::FIELD_DELIMITER_PARAMETER,
    statement::SERIALIZATION_FORMAT_PARAMETER,
];

/// The SerDe parameter whose value Hive writes a separator's CHARACTER CODE in rather
/// than the character itself: the default field separator is recorded as the code `1`,
/// which is the unit separator and not the digit.
const CODED_PARAMETER: &str = statement::SERIALIZATION_FORMAT_PARAMETER;

/// The largest character code [`CODED_PARAMETER`] can name.
const MAX_CHARACTER_CODE: u32 = 255;

/// The SerDe parameter `OpenCSVSerde` carries its separator in.
const CSV_SEPARATOR_PARAMETER: &str = "separatorChar";

/// The SerDe parameter `OpenCSVSerde` carries its quote character in.
const CSV_QUOTE_PARAMETER: &str = "quoteChar";

/// The SerDe parameter that says what text stands for a NULL value.
const NULL_FORMAT_PARAMETER: &str = "serialization.null.format";

/// The text a field holds when it carries no value, as Hive writes a NULL by default.
const DEFAULT_NULL_TEXT: &str = "\\N";

/// The character fields are separated by when a table's SerDe names none: Hive's own
/// default field separator, the ASCII unit separator.
pub const DEFAULT_FIELD_DELIMITER: char = '\u{1}';

/// The serialization library of the delimited-text SerDe that reads quoted CSV.
const OPEN_CSV_SERDE: &str = "org.apache.hadoop.hive.serde2.OpenCSVSerde";

/// The serialization libraries that read one JSON document per line.
const JSON_SERDES: &[&str] = &[
    "org.apache.hive.hcatalog.data.JsonSerDe",
    "org.apache.hadoop.hive.serde2.JsonSerDe",
    "org.openx.data.jsonserde.JsonSerDe",
];

/// The table property that records which open table format a catalog entry holds its
/// data in, as the engines that write such a table record it.
const TABLE_TYPE_PROPERTY: &str = "table_type";

/// The value [`TABLE_TYPE_PROPERTY`] carries for a table of the Iceberg open table
/// format.
const ICEBERG_TABLE_TYPE: &str = "ICEBERG";

/// The table property that records WHERE an Iceberg table's current metadata file is: the
/// one object every read of such a table has to go through, because the table's schema,
/// its snapshots and the data files of each are named there rather than in the catalog.
const METADATA_LOCATION_PROPERTY: &str = "metadata_location";

/// The serialization library an Iceberg table's storage descriptor carries.
const ICEBERG_SERDE: &str = "org.apache.iceberg.mr.hive.HiveIcebergSerDe";

/// The `InputFormat` an Iceberg table's storage descriptor carries.
const ICEBERG_INPUT_FORMAT: &str = "org.apache.iceberg.mr.hive.HiveIcebergInputFormat";

/// How the objects at a table's location are decoded into rows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decoder {
    /// One row per line, fields separated by a character, with an optional quote
    /// character a field may be wrapped in.
    Delimited {
        /// What separates two fields.
        separator: char,
        /// What a field may be wrapped in, when the SerDe quotes its fields.
        quote: Option<char>,
    },
    /// One row per line, each line a JSON document whose members are the columns.
    Json,
    /// One row per record of a Parquet file, whose leaves are the columns
    /// ([`parquet_objects`]).
    Parquet,
}

/// A column of a catalog table's data that the table does not DECLARE: what Athena reports
/// about the OBJECT each row was read from.
///
/// Such a column is addressed by the name below and carries, for every row, a property of
/// the split that row came from. It is HIDDEN: a statement sees it only where it names it,
/// so it is no part of the table's declared columns, of what `SELECT *` expands to, or of
/// what the catalog reports the table's schema to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataColumn {
    /// Where the object the row was read from lives, as its `s3://` URI.
    Path,
    /// How many bytes that object stores.
    FileSize,
    /// When that object was last written.
    FileModifiedTime,
}

impl MetadataColumn {
    /// Every one of these columns, in the order a read holds them.
    pub const ALL: &'static [Self] = &[Self::Path, Self::FileSize, Self::FileModifiedTime];

    /// The name a statement addresses this column by.
    pub fn name(self) -> &'static str {
        match self {
            Self::Path => "$path",
            Self::FileSize => "$file_size",
            Self::FileModifiedTime => "$file_modified_time",
        }
    }

    /// The type the engine holds it as, which is also the type a result set reports for it:
    /// the location is character data, the size an exact number of bytes, and the instant is
    /// held the way every other instant this engine reads is ([`timestamp_text`]).
    fn declared(self) -> &'static str {
        match self {
            Self::Path | Self::FileModifiedTime => TYPE_TEXT,
            Self::FileSize => TYPE_BIGINT,
        }
    }

    /// What this column carries for a row read out of the object `at`.
    fn value(self, at: &str, object: &s3_state::Object) -> ::rusqlite::types::Value {
        use ::rusqlite::types::Value;
        match self {
            Self::Path => Value::Text(at.to_string()),
            Self::FileSize => {
                Value::Integer(i64::try_from(object.body.len()).unwrap_or(i64::MAX))
            }
            Self::FileModifiedTime => Value::Text(instant_text(object.last_modified)),
        }
    }

    /// This column as the engine holds it.
    fn local(self) -> LocalColumn {
        LocalColumn {
            name: self.name().to_string(),
            declared: self.declared(),
        }
    }
}

/// Everything about one catalog table that decides what rows a read of it produces.
#[derive(Debug, Clone)]
pub struct DataSource {
    /// The relation this table was addressed by, for the messages a failed read reports.
    pub relation: Relation,
    /// The name the rows are materialised under in the engine database.
    pub local_name: String,
    /// The table's columns, in order: the storage descriptor's, and then the partition keys
    /// ([`ddl::relation_columns`]).
    pub columns: Vec<DeclaredColumn>,
    /// How many of the LAST [`Self::columns`] are partition keys, whose values come from the
    /// partition a row's object belongs to rather than from that object's own records; `0`
    /// for a table that declares no partition keys.
    partition_keys: usize,
    /// The partitions the catalog holds for this table, each with the location its objects
    /// live under and the values every row read there carries. EMPTY for a table that
    /// declares no partition keys, and for a partitioned table whose partitions the catalog
    /// does not hold — whose rows carry what their own object paths name instead
    /// ([`Self::hive_partition_values`]).
    partitions: Vec<PartitionSplit>,
    /// Where the data lives, as the catalog recorded it.
    location: Option<String>,
    /// How the objects there are decoded.
    decoder: Decoder,
    /// How many lines at the head of each object are a header.
    skip_header_lines: usize,
    /// What text stands for a NULL value.
    null_text: String,
    /// Which of [`Self::columns`], by position and in the table's own column order, the
    /// reader may read: every one of them, until a Lake Formation grant narrows the read
    /// ([`Self::narrowed`]).
    visible: Vec<usize>,
    /// The boolean SQL expression a row has to satisfy for the reader to see it, when a
    /// Lake Formation grant carries one.
    row_predicate: Option<String>,
    /// What the statement's own predicate admits for each PARTITION KEY, in key order: the
    /// values that key may hold for a row the statement reads, or `None` for a key its
    /// predicate says nothing about. Every entry `None` until the read is narrowed
    /// ([`Self::pruned`]), which is what makes a filtered read of a partitioned table scan
    /// only the partitions it can read rows from.
    partition_filter: Vec<Option<Vec<String>>>,
}

/// One PARTITION of a catalog table as a read of it addresses it: the values that address
/// the partition, and where its objects live.
#[derive(Debug, Clone)]
struct PartitionSplit {
    /// The partition's values, one per declared partition key and in key order — the
    /// `PartitionInput$Values` the catalog stored it under.
    values: Vec<String>,
    /// The `s3://` location the partition's objects live under: the one its own storage
    /// descriptor records, and otherwise the Hive directory of the table's location that
    /// its values name ([`hive_partition_location`]).
    location: String,
}

/// One DATA DIRECTORY a read of a table scans: where to list, and which partition the rows
/// it finds there belong to.
#[derive(Debug, Clone)]
struct Scan {
    /// The `s3://` location to list under.
    location: String,
    /// The partition values every row found there carries, or `None` when the partition a
    /// row belongs to is the one its own object's path names.
    values: Option<Vec<String>>,
}


/// What one query may read, which every read of a table charges against.
///
/// ONE budget per statement, carried through the whole resolution and every read it
/// drives, so what bounds the work is the TOTAL — not each factor of it separately: a
/// statement naming many relations, each a view over many more, spends the same relation
/// count as one naming them directly, and a deadline stops the resolution whatever it has
/// reached.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    /// How many relations this statement may still resolve.
    pub relations_left: usize,
    /// How many rows one query may still materialise.
    pub rows_left: usize,
    /// How many VALUES one query may still materialise: a row of a wide table costs more
    /// than a row of a narrow one, and both the rows an object holds and the columns a
    /// table declares are caller-chosen, so what is bounded is their PRODUCT.
    pub cells_left: usize,
    /// How many bytes of object data one query may still read.
    pub bytes_left: usize,
    /// How many bytes of object data this query may scan in all, where the workgroup it
    /// runs in sets a cutoff (`WorkGroupConfiguration$BytesScannedCutoffPerQuery`, "the
    /// upper data usage limit (cutoff) for the amount of bytes a single query in a
    /// workgroup is allowed to scan"). `None` for a query whose workgroup sets none.
    pub scan_cutoff: Option<usize>,
    /// How many bytes of object data this query has scanned so far, which is what the
    /// cutoff above is measured against: the cutoff bounds the whole query, so every
    /// relation it reads charges the same running total.
    pub scanned: usize,
    /// When the resolution must stop, whatever is left of the counts above.
    pub deadline: ::std::time::Instant,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            relations_left: query_engine::MAX_RESOLVED_RELATIONS,
            rows_left: query_engine::MAX_MATERIALISED_ROWS,
            cells_left: query_engine::MAX_MATERIALISED_CELLS,
            bytes_left: query_engine::MAX_MATERIALISED_BYTES,
            scan_cutoff: None,
            scanned: 0,
            deadline: ::std::time::Instant::now() + query_engine::MAX_QUERY_DEADLINE,
        }
    }
}

impl Budget {
    /// The DEFAULT cap on the object data one query reads is never above the CEILING a
    /// configured cutoff may raise it to — asserted here, where the raising below reads both
    /// constants, so a change to either of them fails to compile instead of reaching a
    /// caller's request.
    const BOUNDS_ARE_ORDERED: () = assert!(
        query_engine::MAX_MATERIALISED_BYTES <= query_engine::MAX_SCAN_BYTES,
        "the default read cap must not be above the ceiling a configured cutoff raises it to"
    );

    /// The budget one query runs under, given the data usage cutoff its workgroup sets.
    ///
    /// A cutoff in force is what the query is held to, so the emulator's own cap on the
    /// object data one query reads is raised to it: a limit the caller configured must
    /// decide where the read stops, rather than being pre-empted by a lower cap of this
    /// emulator's — the smallest cutoff the model admits is already above that cap. It is
    /// raised no further than [`query_engine::MAX_SCAN_BYTES`], the ceiling on what one
    /// query can make this process read whatever cutoff is configured.
    pub fn for_scan_cutoff(scan_cutoff: Option<i64>) -> Self {
        let () = Self::BOUNDS_ARE_ORDERED;
        let scan_cutoff = scan_cutoff
            .and_then(|cutoff| usize::try_from(cutoff).ok())
            .filter(|cutoff| *cutoff > 0);
        let bytes_left = match scan_cutoff {
            Some(cutoff) => cutoff.clamp(
                query_engine::MAX_MATERIALISED_BYTES,
                query_engine::MAX_SCAN_BYTES,
            ),
            None => query_engine::MAX_MATERIALISED_BYTES,
        };
        Self {
            bytes_left,
            scan_cutoff,
            ..Self::default()
        }
    }

    /// Charge `bytes` of STORED OBJECT data this query scanned, before it is read.
    ///
    /// Two limits, in this order, because they end the query differently: the cutoff the
    /// workgroup configured ABORTS it — the query is interrupted at the limit it was given
    /// ([`Failure::scan_cutoff_exceeded`]) — while this emulator's own cap on what one query
    /// reads FAILS it ([`Self::too_many_bytes`]). The configured limit is judged first, so a
    /// scan past it reports the limit the caller set rather than a bound of this emulator.
    pub fn charge_scan(&mut self, relation: &Relation, bytes: usize) -> Result<(), Failure> {
        self.charge_deadline()?;
        self.scanned = self.scanned.saturating_add(bytes);
        if let Some(cutoff) = self.scan_cutoff {
            if self.scanned > cutoff {
                return Err(Failure::scan_cutoff_exceeded(cutoff));
            }
        }
        if bytes > self.bytes_left {
            return Err(Self::too_many_bytes(relation));
        }
        self.bytes_left -= bytes;
        Ok(())
    }

    /// Charge ONE relation against this budget, or say that the statement has reached
    /// what this emulator resolves for one query.
    pub fn charge_relation(&mut self) -> Result<(), Failure> {
        self.charge_deadline()?;
        if self.relations_left == 0 {
            return Err(Failure::emulator(format!(
                "this statement reads more than the {} relations this emulator resolves \
                 for one query",
                query_engine::MAX_RESOLVED_RELATIONS
            )));
        }
        self.relations_left -= 1;
        Ok(())
    }

    /// Charge ONE row of `width` values against this budget, BEFORE it is built: what the
    /// budget bounds is what a read allocates, not only what it keeps, so a decode stops
    /// at the first row over budget rather than after the whole object is in memory.
    pub fn charge_row(&mut self, relation: &Relation, width: usize) -> Result<(), Failure> {
        self.charge_deadline()?;
        if self.rows_left == 0 {
            return Err(Self::too_many_rows(relation));
        }
        if width > self.cells_left {
            return Err(Failure::emulator(format!(
                "reading {}.{} would materialise more than the {} values this emulator \
                 holds for one query",
                relation.database,
                relation.table,
                query_engine::MAX_MATERIALISED_CELLS
            )));
        }
        self.rows_left -= 1;
        self.cells_left -= width;
        Ok(())
    }

    /// Charge `bytes` of DECODED object data against this budget.
    ///
    /// What an object holds once decoded is not what it was stored as: a columnar or
    /// compressed object expands when it is read, so a read of one charges what it holds at
    /// the size it decoded to, over and above the stored bytes it was already charged for.
    pub fn charge_decoded(&mut self, relation: &Relation, bytes: usize) -> Result<(), Failure> {
        self.charge_deadline()?;
        if bytes > self.bytes_left {
            return Err(Self::too_many_bytes(relation));
        }
        self.bytes_left -= bytes;
        Ok(())
    }

    /// Why a read of `relation` that would hold more rows than one query materialises is
    /// refused — the one wording for that refusal, wherever it is reached.
    pub fn too_many_rows(relation: &Relation) -> Failure {
        Failure::emulator(format!(
            "reading {}.{} would materialise more than the {} rows this emulator holds \
             for one query",
            relation.database,
            relation.table,
            query_engine::MAX_MATERIALISED_ROWS
        ))
    }

    /// Why a read of `relation` that would read more object data than one query reads is
    /// refused; see [`Self::too_many_rows`].
    pub fn too_many_bytes(relation: &Relation) -> Failure {
        Failure::emulator(format!(
            "reading {}.{} would scan more than the {} bytes this emulator reads \
             for one query",
            relation.database,
            relation.table,
            query_engine::MAX_MATERIALISED_BYTES
        ))
    }

    /// Whether the time this emulator gives one statement is spent.
    pub fn charge_deadline(&self) -> Result<(), Failure> {
        if ::std::time::Instant::now() >= self.deadline {
            return Err(Failure::emulator(format!(
                "this statement spent the {:?} this emulator gives one query before it \
                 could read what it names",
                query_engine::MAX_QUERY_DEADLINE
            )));
        }
        Ok(())
    }
}

impl DataSource {
    /// How one stored catalog table is read, or the failure to report for a table this
    /// emulator's engine cannot read the storage format of.
    pub fn of(
        relation: &Relation,
        table: &glue_state::TableEntry,
    ) -> Result<Self, Failure> {
        let storage = table.storage_descriptor.as_ref();
        let serde = storage.and_then(|storage| storage.serde_info.as_ref());
        let serde_parameters: HashMap<String, String> = serde
            .and_then(|serde| serde.parameters.clone())
            .unwrap_or_default();
        let library = serde
            .and_then(|serde| serde.serialization_library.clone())
            .unwrap_or_else(|| ddl::LAZY_SIMPLE_SERDE.to_string());
        let input_format = storage.and_then(|storage| storage.input_format.clone());
        let table_parameters = table.parameters.clone().unwrap_or_default();
        // A table of an open table format is read THROUGH its own metadata, so one the
        // catalog marks as such and holds no metadata location for cannot be read at all
        // — which is a failure of the caller's own catalog entry, of Athena's published
        // class for it, and not a storage format this engine merely does not decode.
        if is_iceberg(&table_parameters, &library, input_format.as_deref())
            && property(&table_parameters, METADATA_LOCATION_PROPERTY).is_none()
        {
            return Err(Failure::iceberg_without_metadata_location());
        }
        let decoder = decoder_of(&library, input_format.as_deref(), &serde_parameters)
            .ok_or_else(|| {
                Failure::emulator(format!(
                    "the data of {}.{} is stored as {library}, which this emulator's query \
                     engine does not read",
                    relation.database, relation.table
                ))
            })?;
        let columns = ddl::relation_columns(table);
        // What the entry DECLARES has to be readable as a table definition before a read of
        // it can be planned: the catalog stores a column's type as the text a client wrote
        // and never reads it, so a column whose type text names no type at all
        // ([`hive_types`]) is an entry this read cannot be planned against — reported as
        // such, rather than read as a column of some fallback type.
        if let Some(column) = columns
            .iter()
            .find(|column| !hive_types::names_a_type(&column.data_type))
        {
            return Err(Failure::invalid_column_type(&column.name, &column.data_type));
        }
        let location = storage.and_then(|storage| storage.location.clone());
        let partition_keys = ddl::partition_key_columns(table).len();
        Ok(Self {
            relation: relation.clone(),
            local_name: relation.local_name(),
            // Every column of the table reads, until a Lake Formation grant narrows it.
            visible: (0..columns.len()).collect(),
            row_predicate: None,
            // Every partition of the table is scanned, until the statement's own predicate
            // narrows which of them can hold a row it reads.
            partition_filter: vec![None; partition_keys],
            partition_keys,
            partitions: partition_splits(table, location.as_deref()),
            columns,
            location,
            decoder,
            skip_header_lines: table_parameters
                .get(SKIP_HEADER_PROPERTY)
                .and_then(|lines| lines.parse().ok())
                .unwrap_or(0),
            null_text: serde_parameters
                .get(NULL_FORMAT_PARAMETER)
                .cloned()
                .unwrap_or_else(|| DEFAULT_NULL_TEXT.to_string()),
        })
    }

    /// This source as the reader `scope` describes may read it: the columns that scope
    /// covers, and the rows its predicate keeps.
    ///
    /// A column the scope does not cover is not read, not materialised and not addressable
    /// — naming it is an unknown column to the statement, which is what a column outside a
    /// reader's grant is.
    #[must_use]
    pub fn narrowed(mut self, scope: &ReadScope) -> Self {
        self.visible = self
            .columns
            .iter()
            .enumerate()
            .filter(|(_, column)| scope.admits_column(&column.name))
            .map(|(position, _)| position)
            .collect();
        self.row_predicate = scope.row_predicate();
        self
    }

    /// This source read under the statement's own `constraints`: the partitions it scans are
    /// narrowed to the ones that can hold a row the statement reads.
    ///
    /// A partitioned table's data lives one directory per partition, so a statement whose
    /// predicate fixes a partition key's value can only read rows from the partitions that
    /// carry it — the others hold no row the statement would keep, and are not scanned at
    /// all, which is what makes such a read scan the data of its own partitions rather than
    /// of the whole table.
    ///
    /// The caller decides whether the statement's constraints can be attributed to this
    /// source at all (`super::resolved_sql`): they can where the statement reads exactly one
    /// relation, and a constraint a statement wrote against ANOTHER relation's name is not
    /// this source's and narrows nothing. Only a key whose declared type is CHARACTER data is
    /// narrowed by: a partition's value is the text its own directory names, and for every
    /// other type the comparison the predicate states is the type's own rather than a
    /// comparison of that text.
    pub fn pruned(&mut self, constraints: &[statement::ColumnConstraint]) {
        let keys: Vec<DeclaredColumn> = self
            .columns
            .iter()
            .skip(self.columns.len().saturating_sub(self.partition_keys))
            .cloned()
            .collect();
        self.partition_filter = keys
            .iter()
            .map(|key| {
                if !is_character_type(&key.data_type) {
                    return None;
                }
                constraints
                    .iter()
                    .find(|constraint| {
                        constraint.column == key.name
                            && constraint
                                .qualifier
                                .as_deref()
                                .is_none_or(|qualifier| qualifier == self.relation.table)
                    })
                    .map(|constraint| constraint.values.clone())
            })
            .collect();
    }

    /// The columns this source reads, in the table's own column order: the ones the
    /// reader's Lake Formation grant covers.
    #[must_use]
    pub fn visible_columns(&self) -> Vec<&DeclaredColumn> {
        self.visible
            .iter()
            .filter_map(|position| self.columns.get(*position))
            .collect()
    }

    /// The columns this source does NOT read because the reader's Lake Formation grant
    /// leaves them out, as the catalog folds their names.
    ///
    /// What a statement that names one of them is told rests on this: a column the table
    /// declares and this reader may not see is one it is not AUTHORIZED to read, which is a
    /// different sentence from one the table does not declare at all.
    #[must_use]
    pub fn hidden_columns(&self) -> Vec<String> {
        self.columns
            .iter()
            .enumerate()
            .filter(|(position, _)| !self.visible.contains(position))
            .map(|(_, column)| glue_state::fold_name(&column.name))
            .collect()
    }

    /// Which columns the decoded rows carry: every column of the table while a row
    /// predicate is still to be evaluated — the predicate may name a column the reader may
    /// not SEE, and the engine evaluates it before the projection — and the visible ones
    /// alone otherwise.
    fn materialised(&self) -> Vec<usize> {
        match self.row_predicate {
            Some(_) => (0..self.columns.len()).collect(),
            None => self.visible.clone(),
        }
    }

    /// The table as the engine holds it: its columns with the type each is read as, and
    /// the rows the objects at its location decode to.
    ///
    /// A read the reader's grant narrows by a row predicate is handed to the engine as the
    /// rows plus that predicate ([`LocalObject::Filtered`]), so the engine is what decides
    /// which rows the statement sees.
    /// `metadata` are the hidden metadata columns the statement NAMES ([`MetadataColumn`]):
    /// a read carries them only where the statement reads one, so a statement that names
    /// none is handed exactly the table's own columns.
    pub fn read(
        &self,
        objects: &s3_state::ComAmazonawsS3Amazons3State,
        budget: &mut Budget,
        metadata: &[MetadataColumn],
        read_columns: Option<&HashSet<String>>,
    ) -> Result<(LocalObject, usize), Failure> {
        if self.columns.is_empty() {
            return Err(Failure::user(format!(
                "Table '{}.{}' has no columns, so nothing can be read from it",
                self.relation.database, self.relation.table
            )));
        }
        let (rows, scanned) = self.rows(objects, budget, metadata, read_columns)?;
        let local_column = |position: &usize| {
            self.columns.get(*position).map(|column| LocalColumn {
                name: column.name.clone(),
                declared: engine_type(&column.data_type),
            })
        };
        let hidden = || metadata.iter().map(|column| column.local());
        let materialised: Vec<LocalColumn> = self
            .materialised()
            .iter()
            .filter_map(local_column)
            .chain(hidden())
            .collect();
        let visible: Vec<LocalColumn> = self
            .visible
            .iter()
            .filter_map(local_column)
            .chain(hidden())
            .collect();
        let object = match &self.row_predicate {
            None => LocalObject::Table(LocalTable {
                name: self.local_name.clone(),
                columns: materialised,
                rows,
            }),
            Some(predicate) => LocalObject::Filtered {
                table: LocalTable {
                    name: self.local_name.clone(),
                    columns: visible,
                    rows,
                },
                source_columns: materialised,
                predicate: predicate.clone(),
            },
        };
        Ok((object, scanned))
    }

    /// The rows the objects at this table's location decode to, and the bytes read for
    /// them.
    ///
    /// `read_columns` are the columns the statement NAMES, or `None` where it reads every
    /// column: what a read GOES THROUGH depends on how the objects hold a row, so a columnar
    /// object is read through the column chunks those columns live in and a row-format object
    /// whole ([`Self::scanned_bytes`]).
    fn rows(
        &self,
        objects: &s3_state::ComAmazonawsS3Amazons3State,
        budget: &mut Budget,
        metadata: &[MetadataColumn],
        read_columns: Option<&HashSet<String>>,
    ) -> Result<(Vec<Vec<::rusqlite::types::Value>>, usize), Failure> {
        let mut rows: Vec<Vec<::rusqlite::types::Value>> = Vec::new();
        let mut scanned = 0usize;
        // A table whose catalog entry names no location has nothing under it to read.
        let Some(location) = self.location.as_deref().filter(|at| !at.is_empty()) else {
            return Ok((rows, scanned));
        };
        // A PARTITIONED table's data lives under its partitions, one data directory each; a
        // table that declares no partition keys has exactly one, its own location.
        for scan in self.scans(location) {
            // One partition is one directory to list, and a partitioned table has as many as
            // the catalog holds, so the time this statement may spend is charged before each
            // of them as well as per object below.
            budget.charge_deadline()?;
            let Some((bucket_name, prefix)) = parse_s3_uri(&scan.location) else {
                return Err(Failure::user(format!(
                    "Unable to read the location {} of {}.{}: it is not an Amazon S3 \
                     location",
                    scan.location, self.relation.database, self.relation.table
                )));
            };
            let Some(bucket) = objects.bucket(&bucket_name) else {
                // The storage the entry names is not there at all, so no split of this table
                // can be listed: a failure of the FILESYSTEM class, which is not an access
                // decision about storage the reader may not read.
                return Err(Failure::location_not_readable(&scan.location));
            };
            for (key, object) in bucket.objects() {
                budget.charge_deadline()?;
                if !under_prefix(key, &prefix) || is_hidden(key) {
                    continue;
                }
                // WHICH PARTITION the rows of this object belong to: the one the scan is of,
                // and otherwise the one the object's own Hive path names. An object under no
                // partition of a partitioned table is no part of that table's data, so it is
                // not read at all.
                let partition = match &scan.values {
                    Some(values) => Cow::Borrowed(values.as_slice()),
                    None if self.partition_keys == 0 => Cow::Borrowed([].as_slice()),
                    None => match self.hive_partition_values(key) {
                        Some(values) => Cow::Owned(values),
                        None => continue,
                    },
                };
                // What this read SCANNED is charged before the object is decoded: the
                // cutoff the query's workgroup sets and this emulator's own cap are both
                // measured on the stored data a read goes through
                // ([`Budget::charge_scan`]).
                let going_through = self.scanned_bytes(&object.body, read_columns);
                budget.charge_scan(&self.relation, going_through)?;
                scanned = scanned.saturating_add(going_through);
                let split = format!("s3://{bucket_name}/{key}");
                // What the hidden metadata columns carry is a property of THIS object, so it
                // is read once per object and held by every row that object holds.
                let held: Vec<::rusqlite::types::Value> = metadata
                    .iter()
                    .map(|column| column.value(&split, object))
                    .collect();
                let body = self.uncompressed(key, &object.body, &split, budget)?;
                self.decode(&body, &split, &held, &partition, &mut rows, budget)?;
            }
        }
        Ok((rows, scanned))
    }

    /// How many bytes of ONE stored object a read of it GOES THROUGH, which is what the
    /// query is charged for scanning and what `DataScannedInBytes` reports.
    ///
    /// The format decides it. A COLUMNAR object holds each column apart and self-describes
    /// where, so a read goes through the chunks of the columns the statement NAMES and
    /// nothing else — which is why projecting one column of such a table scans a fraction of
    /// what the same projection over a row-format table does, why those per-column costs add
    /// up, and why even reading every column costs less than the object holds
    /// ([`parquet_objects::scanned_bytes`]). Every row-format object — one record per line,
    /// whatever the SerDe — holds a whole row together, so a read goes through all of it
    /// however few columns the statement projects. A statement that names every column (a
    /// wildcard projection) goes through the whole object either way, and so does a columnar
    /// object this reader cannot read the metadata of.
    fn scanned_bytes(&self, body: &[u8], read_columns: Option<&HashSet<String>>) -> usize {
        let whole = body.len();
        if self.decoder != Decoder::Parquet {
            return whole;
        }
        let Some(named) = read_columns else {
            return whole;
        };
        parquet_objects::scanned_bytes(body, named).unwrap_or(whole)
    }

    /// The data directories a read of this table scans, and which partition the rows found
    /// in each belong to.
    ///
    /// Three cases, and the rule is the same one in all three — a read scans the directories
    /// the table's DATA lives under:
    ///
    /// * a table that declares no partition keys has one data directory, its own location,
    ///   and its rows carry no partition values;
    /// * a partitioned table whose partitions the catalog HOLDS is read partition by
    ///   partition, each under the location that partition records and carrying that
    ///   partition's own values;
    /// * a partitioned table whose partitions the catalog holds none of — the table whose
    ///   partitions Athena PROJECTS rather than registers — is read under its own location,
    ///   and the partition a row belongs to is the one its object's path names.
    fn scans(&self, location: &str) -> Vec<Scan> {
        if self.partition_keys == 0 || self.partitions.is_empty() {
            return vec![Scan {
                location: location.to_string(),
                values: None,
            }];
        }
        self.partitions
            .iter()
            .filter(|partition| self.admits(&partition.values))
            .map(|partition| Scan {
                location: partition.location.clone(),
                values: Some(partition.values.clone()),
            })
            .collect()
    }

    /// Whether a partition addressed by `values` can hold a row the statement reads.
    ///
    /// Every key the statement's predicate fixes has to be satisfied, and a key it says
    /// nothing about admits every value — so a read no predicate narrows scans every
    /// partition, exactly as it did before it was narrowed ([`Self::pruned`]).
    fn admits(&self, values: &[String]) -> bool {
        self.partition_filter
            .iter()
            .enumerate()
            .all(|(key, admitted)| match admitted {
                None => true,
                Some(admitted) => values
                    .get(key)
                    .is_some_and(|value| admitted.iter().any(|allowed| allowed == value)),
            })
    }

    /// The partition values the object `key` names in its own path: for each declared
    /// partition key in order, the value of the `<key>=<value>` path component naming it.
    ///
    /// `None` when the path names no value for one of the keys, which is what says the
    /// object belongs to no partition of this table. The key names are matched FOLDED, the
    /// way the catalog holds every name it stores.
    fn hive_partition_values(&self, key: &str) -> Option<Vec<String>> {
        let mut segments = key.split('/');
        let mut values = Vec::with_capacity(self.partition_keys);
        for column in self.columns.iter().skip(self.data_columns()) {
            let wanted = glue_state::fold_name(&column.name);
            let found = segments.by_ref().find_map(|segment| {
                let (name, value) = segment.split_once('=')?;
                (glue_state::fold_name(name) == wanted).then(|| value.to_string())
            })?;
            values.push(found);
        }
        Some(values)
    }

    /// How many of [`Self::columns`] the objects' own records carry values for: every column
    /// the storage descriptor declares, which is all of them but the partition keys.
    fn data_columns(&self) -> usize {
        self.columns.len().saturating_sub(self.partition_keys)
    }

    /// Which of a partition's values the column at `position` holds, or `None` for a column
    /// the objects' own records carry.
    ///
    /// A partition key is one of the columns AFTER the storage descriptor's, and the value it
    /// holds is the one at that key's own place in the partition's value list — absent for a
    /// partition naming fewer values than the table declares keys, which is NULL rather than
    /// a value read out of a record.
    fn partition_value<'v>(
        &self,
        position: usize,
        partition: &'v [String],
    ) -> Option<Option<&'v str>> {
        let index = position.checked_sub(self.data_columns())?;
        (index < self.partition_keys).then(|| partition.get(index).map(String::as_str))
    }

    /// One split's bytes as this table's SerDe reads them: the object's own bytes, with the
    /// compression its KEY names undone ([`text_codecs`]).
    ///
    /// Only a TEXT object carries its compression in its key. A Parquet file holds its own,
    /// per column and inside the file, which the format's reader undoes, so nothing is taken
    /// off such an object here however it is keyed.
    ///
    /// What the object decompresses to is charged against the read's budget over and above
    /// the stored bytes it was already charged for, and the decompression itself stops at
    /// that bound — so an object that would expand past what one query may hold is refused
    /// rather than held.
    fn uncompressed<'body>(
        &self,
        key: &str,
        body: &'body [u8],
        split: &str,
        budget: &mut Budget,
    ) -> Result<Cow<'body, [u8]>, Failure> {
        if matches!(self.decoder, Decoder::Parquet) {
            return Ok(Cow::Borrowed(body));
        }
        let Some(codec) = text_codecs::of_key(key) else {
            return Ok(Cow::Borrowed(body));
        };
        let decompressed = codec.decompress(body, budget.bytes_left).map_err(|refused| {
            match refused {
                text_codecs::NotDecompressed::TooLarge => {
                    Budget::too_many_bytes(&self.relation)
                }
                text_codecs::NotDecompressed::NotTheCodec(reason) => {
                    cannot_open_split(split, &reason)
                }
                text_codecs::NotDecompressed::NoDecoder(codec) => Failure::emulator(format!(
                    "the data of {}.{} is compressed with the codec the {} suffix names, \
                     which this emulator's query engine does not decompress",
                    self.relation.database,
                    self.relation.table,
                    codec.extension()
                )),
            }
        })?;
        budget.charge_decoded(&self.relation, decompressed.len())?;
        Ok(Cow::Owned(decompressed))
    }

    /// Decode one object's bytes into the rows it holds, appending them to `rows`.
    ///
    /// `split` is where the object this reads lives, which a failure to OPEN it at all names
    /// — the unit of work Hive calls a split is one object of the table's location.
    ///
    /// `metadata` are the values the hidden metadata columns of this object carry, which
    /// every row read out of it holds beside its own fields, and `partition` the partition
    /// values the object's own partition carries, which every row read out of it holds for
    /// the table's partition keys.
    ///
    /// STREAMING, and budget-charged BEFORE each row is built: one record is read, charged
    /// and converted at a time, so a read stops at the first row over budget instead of
    /// holding a whole object's decoded records in memory first.
    fn decode(
        &self,
        body: &[u8],
        split: &str,
        metadata: &[::rusqlite::types::Value],
        partition: &[String],
        rows: &mut Vec<Vec<::rusqlite::types::Value>>,
        budget: &mut Budget,
    ) -> Result<(), Failure> {
        // Only the cells the read MATERIALISES are built, so the width one row charges
        // against the budget is the width of the row the engine is handed.
        let materialised = self.materialised();
        let width = materialised.len().saturating_add(metadata.len());
        match &self.decoder {
            Decoder::Delimited { separator, quote } => {
                // A text SerDe reads a split's bytes AS TEXT, byte for byte: a byte that is
                // not text stands for the replacement character the way Hive's own text
                // reader holds one, so a single such byte never fails the split — the
                // records either side of it are still the records the object holds.
                let text = String::from_utf8_lossy(body);
                let mut reader =
                    delimited_reader(text.as_bytes(), *separator, *quote).map_err(Failure::emulator)?;
                let mut record = ::csv::StringRecord::new();
                let mut skipped = 0usize;
                loop {
                    let read = reader.read_record(&mut record).map_err(|reason| {
                        Failure::emulator(format!(
                            "the records of {split} could not be read as the delimited text \
                             the SerDe of {}.{} declares: {reason}",
                            self.relation.database, self.relation.table
                        ))
                    })?;
                    if !read {
                        break;
                    }
                    if skipped < self.skip_header_lines {
                        skipped += 1;
                        continue;
                    }
                    budget.charge_row(&self.relation, width)?;
                    rows.push(self.row(&materialised, metadata, partition, |position| {
                        record.get(position)
                    }));
                }
            }
            Decoder::Json => {
                let text = String::from_utf8_lossy(body);
                let mut skipped = 0usize;
                for line in text
                    .split('\n')
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                {
                    if skipped < self.skip_header_lines {
                        skipped += 1;
                        continue;
                    }
                    budget.charge_row(&self.relation, width)?;
                    let document: Option<::serde_json::Value> =
                        ::serde_json::from_str(line).ok();
                    let members = document.as_ref().and_then(::serde_json::Value::as_object);
                    // The members a JSON SerDe reads are the table's columns BY NAME, so
                    // each column's value is looked up rather than taken by position, and
                    // only the values this table's columns name are ever built.
                    let fields: Vec<Option<String>> = self
                        .columns
                        .iter()
                        .map(|column| {
                            members.and_then(|members| json_member(members, &column.name))
                        })
                        .collect();
                    rows.push(self.row(&materialised, metadata, partition, |position| {
                        fields.get(position).and_then(Option::as_deref)
                    }));
                }
            }
            // A Parquet object carries its own schema, so what each of its leaves holds is
            // the format's own reading of it and the record's values are taken by NAME —
            // the header lines a text SerDe skips are a property of TEXT objects and have
            // no counterpart here.
            Decoder::Parquet => {
                // What the object's own footer declares a read of it would decode, checked
                // against what this query may still hold BEFORE a page of it is read: a
                // columnar object is held compressed, so what reading it costs is what it
                // decodes to and not the stored bytes already charged above.
                let limits = parquet_objects::Limits {
                    records: budget.rows_left,
                    decoded_bytes: budget.bytes_left,
                };
                parquet_objects::for_each_record(
                    body,
                    &self.relation,
                    split,
                    limits,
                    |record| {
                        budget.charge_row(&self.relation, width)?;
                        let values = parquet_objects::values_of(record);
                        budget.charge_decoded(
                            &self.relation,
                            values
                                .values()
                                .map(parquet_objects::Cell::size)
                                .fold(0usize, usize::saturating_add),
                        )?;
                        rows.push(self.parquet_row(&materialised, metadata, partition, &values));
                        Ok(())
                    },
                )?;
            }
        }
        Ok(())
    }

    /// One Parquet record as the row it holds: one value per MATERIALISED column, each read
    /// out of the record by its column's own NAME, followed by what the hidden metadata
    /// columns of the object it was read from carry.
    fn parquet_row(
        &self,
        materialised: &[usize],
        metadata: &[::rusqlite::types::Value],
        partition: &[String],
        values: &HashMap<String, parquet_objects::Cell>,
    ) -> Vec<::rusqlite::types::Value> {
        materialised
            .iter()
            .filter_map(|position| {
                self.columns.get(*position).map(|column| (*position, column))
            })
            .map(|(position, column)| {
                // A partition key's value is the partition's, not the object's: a Parquet
                // file at a partition's location carries no leaf for it.
                if let Some(value) = self.partition_value(position, partition) {
                    return self.cell(column, value);
                }
                let held = values.get(&glue_state::fold_name(&column.name));
                match held {
                    // Binary data is held as the bytes the object carries: a binary
                    // column's value is not text, so it is never read through one.
                    Some(parquet_objects::Cell::Bytes(data))
                        if engine_type(&column.data_type) == TYPE_BLOB =>
                    {
                        ::rusqlite::types::Value::Blob(data.clone())
                    }
                    Some(held) => self.cell(column, Some(&held.text())),
                    None => self.cell(column, None),
                }
            })
            .chain(metadata.iter().cloned())
            .collect()
    }

    /// One record as the row it holds: one value per MATERIALISED column, in the table's
    /// own column order, each read from the record by its column's own position in the
    /// table — so leaving a column out of the row never shifts the ones beside it — followed
    /// by what the hidden metadata columns of the object it was read from carry.
    fn row<'f>(
        &self,
        materialised: &[usize],
        metadata: &[::rusqlite::types::Value],
        partition: &[String],
        field: impl Fn(usize) -> Option<&'f str>,
    ) -> Vec<::rusqlite::types::Value> {
        materialised
            .iter()
            .filter_map(|position| {
                self.columns.get(*position).map(|column| {
                    // A partition key's value is the partition's, which the object's own
                    // records carry no field for.
                    match self.partition_value(*position, partition) {
                        Some(value) => self.cell(column, value),
                        None => self.cell(column, field(*position)),
                    }
                })
            })
            .chain(metadata.iter().cloned())
            .collect()
    }

    /// One field as the value its column holds it as.
    ///
    /// A field the object does not carry, or one carrying the SerDe's NULL text, is NULL;
    /// so is a field whose text is not a value of the column's own type, which is what
    /// Hive reads a malformed value as rather than failing the whole read.
    fn cell(&self, column: &DeclaredColumn, field: Option<&str>) -> ::rusqlite::types::Value {
        use ::rusqlite::types::Value;
        let Some(field) = field else {
            return Value::Null;
        };
        if field == self.null_text {
            return Value::Null;
        }
        match engine_type(&column.data_type) {
            TYPE_TEXT if is_timestamp(&column.data_type) => {
                Value::Text(timestamp_text(field))
            }
            TYPE_TEXT => Value::Text(field.to_string()),
            TYPE_BLOB => Value::Blob(field.as_bytes().to_vec()),
            TYPE_BOOLEAN => match field.trim() {
                text if text.eq_ignore_ascii_case("true") => Value::Integer(1),
                text if text.eq_ignore_ascii_case("false") => Value::Integer(0),
                _ => Value::Null,
            },
            TYPE_DOUBLE => field
                .trim()
                .parse::<f64>()
                .map_or(Value::Null, Value::Real),
            _ => field
                .trim()
                .parse::<i64>()
                .map_or(Value::Null, Value::Integer),
        }
    }
}

/// Whether a column of Hive type `data_type` holds a TIMESTAMP, whose values are reported
/// to the precision that type carries (see [`timestamp_text`]).
fn is_timestamp(data_type: &str) -> bool {
    glue_state::base_hive_type(data_type) == "timestamp"
}

/// Whether a column of Hive type `data_type` holds CHARACTER data, which is the one class of
/// type whose values are the text a partition's own directory names ([`DataSource::pruned`]).
fn is_character_type(data_type: &str) -> bool {
    matches!(
        glue_state::base_hive_type(data_type).as_str(),
        "string" | "varchar" | "char"
    )
}

/// How many digits of a second a timestamp carries: Athena's `timestamp` is a value to
/// the millisecond, and a read of such a column reports every one of those digits — so a
/// stored value that writes fewer is reported with the rest of them, and one that writes
/// more is reported to that precision.
const TIMESTAMP_FRACTION_DIGITS: usize = 3;

/// One timestamp field as the value its column holds it as: the instant the field names,
/// written to the precision the type carries. Text that names no instant at all is held as
/// it stands, because nothing else can honestly be said about it.
fn timestamp_text(field: &str) -> String {
    let trimmed = field.trim();
    let (instant, fraction) = match trimmed.split_once('.') {
        Some((instant, fraction)) if fraction.chars().all(|digit| digit.is_ascii_digit()) => {
            (instant, fraction)
        }
        Some(_) => return field.to_string(),
        None => (trimmed, ""),
    };
    if !names_an_instant(instant) {
        return field.to_string();
    }
    let mut digits: String = fraction.chars().take(TIMESTAMP_FRACTION_DIGITS).collect();
    while digits.len() < TIMESTAMP_FRACTION_DIGITS {
        digits.push('0');
    }
    format!("{instant}.{digits}")
}

/// One instant the object store holds, as the text a column of instants holds it as: the
/// UTC calendar date and the time of day, to the same digits of a second a stored timestamp
/// is read to ([`TIMESTAMP_FRACTION_DIGITS`]).
fn instant_text(at: ::std::time::SystemTime) -> String {
    let instant: ::chrono::DateTime<::chrono::Utc> = at.into();
    instant.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// Whether `text` names an instant as a timestamp writes one: a date, a separator and a
/// time of day.
fn names_an_instant(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() != "0000-00-00 00:00:00".len() {
        return false;
    }
    text.char_indices().all(|(position, character)| {
        match position {
            4 | 7 => character == '-',
            10 => character == ' ' || character == 'T',
            13 | 16 => character == ':',
            _ => character.is_ascii_digit(),
        }
    })
}

/// The declared type a column of a 32-bit integer type is held as.
const TYPE_INTEGER: &str = "integer";
/// The declared type a column of a 64-bit integer type is held as.
const TYPE_BIGINT: &str = "bigint";
/// The declared type an approximate-numeric column is held as.
const TYPE_DOUBLE: &str = "double";
/// The declared type a boolean column is held as.
const TYPE_BOOLEAN: &str = "boolean";
/// The declared type a character column is held as.
const TYPE_TEXT: &str = "varchar";
/// The declared type a binary column is held as.
const TYPE_BLOB: &str = "blob";

/// The type the engine holds a column of Hive type `data_type` as.
///
/// The catalog records a column's type in Hive's own type names, and each one maps to the
/// type this engine computes over and reports: the exact-numeric types to an integer of
/// the matching width, the approximate-numeric and fixed-point types to a double, Hive's
/// boolean to a boolean, its binary type to binary data, and everything else — the
/// character, date, time and complex types — to character data, which is the text the
/// object holds it as.
pub fn engine_type(data_type: &str) -> &'static str {
    match glue_state::base_hive_type(data_type).as_str() {
        "boolean" => TYPE_BOOLEAN,
        "tinyint" | "smallint" | "int" | "integer" => TYPE_INTEGER,
        "bigint" => TYPE_BIGINT,
        "float" | "real" | "double" | "double precision" | "decimal" | "numeric" => TYPE_DOUBLE,
        "binary" | "varbinary" => TYPE_BLOB,
        _ => TYPE_TEXT,
    }
}

/// The value the table property `name` carries, whatever case the catalog recorded the
/// property's NAME in, and `None` when the property is absent or carries no value at all.
///
/// A property a client wrote through the catalog API carries whatever name that client
/// spelled, so a reader that matched the name byte for byte would miss the same property
/// under another spelling of its case.
fn property<'held>(
    parameters: &'held HashMap<String, String>,
    name: &str,
) -> Option<&'held str> {
    parameters.iter().find_map(|(property, value)| {
        let value = value.trim();
        (property.trim().eq_ignore_ascii_case(name) && !value.is_empty()).then_some(value)
    })
}

/// Whether the catalog marks this entry as a table of the ICEBERG open table format.
///
/// Either half of the entry says so: the `table_type` property an engine that writes such
/// a table records the format in, or a storage descriptor carrying Iceberg's own
/// serialization library or input format.
fn is_iceberg(
    parameters: &HashMap<String, String>,
    library: &str,
    input_format: Option<&str>,
) -> bool {
    property(parameters, TABLE_TYPE_PROPERTY)
        .is_some_and(|table_type| table_type.eq_ignore_ascii_case(ICEBERG_TABLE_TYPE))
        || library.trim().eq_ignore_ascii_case(ICEBERG_SERDE)
        || input_format
            .is_some_and(|format| format.trim().eq_ignore_ascii_case(ICEBERG_INPUT_FORMAT))
}

/// How a table stored with the SerDe `library` under the input format `input_format` is
/// decoded, or `None` for a storage format this emulator's engine does not read.
///
/// BOTH halves of what the catalog records decide it: the serialization library says how a
/// record's fields are written, and the input format says how the objects themselves are.
/// A table whose objects are neither Parquet files nor the delimited text an input format of
/// that kind names holds data in a format this engine cannot decode, whatever its SerDe says
/// — so it is reported rather than decoded as text.
fn decoder_of(
    library: &str,
    input_format: Option<&str>,
    parameters: &HashMap<String, String>,
) -> Option<Decoder> {
    // A table whose descriptor names the COLUMNAR Parquet format holds Parquet files, which
    // carry their own schema and their own per-column compression: they are read by the
    // implementation of that format rather than as text, so this is decided before the text
    // formats are considered at all.
    if parquet_objects::stores_parquet(library, input_format) {
        return Some(Decoder::Parquet);
    }
    let text_objects = input_format
        .map(str::trim)
        .filter(|format| !format.is_empty())
        .is_none_or(|format| format.eq_ignore_ascii_case(ddl::TEXT_INPUT_FORMAT));
    if !text_objects {
        return None;
    }
    if JSON_SERDES
        .iter()
        .any(|serde| library.eq_ignore_ascii_case(serde))
    {
        return Some(Decoder::Json);
    }
    if library.eq_ignore_ascii_case(OPEN_CSV_SERDE) {
        return Some(Decoder::Delimited {
            separator: single_char(parameters.get(CSV_SEPARATOR_PARAMETER)).unwrap_or(','),
            quote: single_char(parameters.get(CSV_QUOTE_PARAMETER)).or(Some('"')),
        });
    }
    if library.eq_ignore_ascii_case(ddl::LAZY_SIMPLE_SERDE) {
        let separator = FIELD_DELIMITER_PARAMETERS
            .iter()
            .find_map(|parameter| separator_char(parameter, parameters.get(*parameter)))
            .unwrap_or(DEFAULT_FIELD_DELIMITER);
        return Some(Decoder::Delimited {
            separator,
            quote: None,
        });
    }
    None
}

/// The partitions the catalog holds for `table`, each with the location its objects live
/// under, or an empty list for a table the catalog holds no partition of.
///
/// `table_location` is the table's own `StorageDescriptor$Location`, which a partition that
/// records no location of its own sits under ([`hive_partition_location`]): the catalog's
/// own partitions are read where the catalog says they are, and a partition registered with
/// no location is where Hive puts it.
fn partition_splits(
    table: &glue_state::TableEntry,
    table_location: Option<&str>,
) -> Vec<PartitionSplit> {
    let keys: Vec<String> = ddl::partition_key_columns(table)
        .iter()
        .map(|column| column.name.clone())
        .collect();
    if keys.is_empty() {
        return Vec::new();
    }
    table
        .partitions
        .values()
        .filter_map(|partition| {
            let location = partition
                .storage_descriptor
                .as_ref()
                .and_then(|storage| storage.location.clone())
                .filter(|location| !location.is_empty())
                .or_else(|| {
                    table_location
                        .filter(|location| !location.is_empty())
                        .map(|location| {
                            hive_partition_location(location, &keys, &partition.values)
                        })
                })?;
            Some(PartitionSplit {
                values: partition.values.clone(),
                location,
            })
        })
        .collect()
}

/// Where Hive keeps the partition `values` of a table at `table_location`: one directory per
/// partition key under it, each named `<key>=<value>`.
///
/// This is the layout Hive, Athena and every engine that writes a partitioned table use, and
/// the one [`DataSource::hive_partition_values`] reads a partition's values back out of — so
/// a partition registered with no location of its own is read where such a writer put it.
///
/// It is also the layout a statement that WRITES a partitioned table lays its objects out in
/// ([`super::query_output`]), so the directory a write puts a partition's objects in and the
/// directory a read looks for them under are one rule.
pub(super) fn hive_partition_location(
    table_location: &str,
    keys: &[String],
    values: &[String],
) -> String {
    let mut location = table_location.trim_end_matches('/').to_string();
    for (key, value) in keys.iter().zip(values) {
        location.push('/');
        location.push_str(key);
        location.push('=');
        location.push_str(value);
    }
    location
}

/// The character a SerDe parameter names as the field separator.
///
/// [`CODED_PARAMETER`] carries a character CODE — Hive records the default separator as
/// the code of the unit separator — so a value that is a number names the character with
/// that code; every other value is the character it carries.
fn separator_char(parameter: &str, value: Option<&String>) -> Option<char> {
    if parameter == CODED_PARAMETER {
        if let Some(code) = value
            .and_then(|value| value.parse::<u32>().ok())
            .filter(|code| *code <= MAX_CHARACTER_CODE)
        {
            return char::from_u32(code);
        }
    }
    single_char(value)
}

/// The one character a SerDe parameter names, or `None` when it names none.
fn single_char(parameter: Option<&String>) -> Option<char> {
    let mut characters = parameter?.chars();
    let first = characters.next()?;
    characters.next().is_none().then_some(first)
}

/// The bucket and key prefix an `s3://` location names, or `None` for a location that
/// names no object store path.
pub fn parse_s3_uri(location: &str) -> Option<(String, String)> {
    let rest = location.strip_prefix(S3_SCHEME)?;
    let (bucket, prefix) = rest.split_once('/').unwrap_or((rest, ""));
    if bucket.is_empty() {
        return None;
    }
    Some((
        bucket.to_string(),
        prefix.trim_end_matches('/').to_string(),
    ))
}

/// Whether `key` names an object AT a table's location: the prefix itself, or anything
/// under it, including the objects of a nested prefix.
pub(super) fn under_prefix(key: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    key == prefix || key.starts_with(&format!("{prefix}/"))
}

/// Whether `key` names an object a read of a table's location SKIPS: the prefix markers a
/// console write leaves behind, and the files Hive treats as hidden — the ones whose name
/// starts with `_` or `.`.
fn is_hidden(key: &str) -> bool {
    if key.ends_with('/') {
        return true;
    }
    let name = key.rsplit('/').next().unwrap_or(key);
    name.is_empty() || name.starts_with('_') || name.starts_with('.')
}

/// The failure a split a read cannot OPEN reports: Hive's own class for it, naming the
/// object and what refused it.
///
/// ONE wording for every reason an object at a table's location cannot be opened at all — a
/// file that is not the format the table's descriptor declares, and one that is not the
/// compressed stream its own key names — because to the reader both are the same thing: a
/// split it could not open.
fn cannot_open_split(split: &str, reason: &str) -> Failure {
    Failure::user(format!(
        "HIVE_CANNOT_OPEN_SPLIT: Error opening Hive split {split}: {reason}"
    ))
}

/// The reader one object of delimited text is read RECORD BY RECORD with, or the reason
/// its SerDe names a separator this reader cannot read.
///
/// The format is read by the maintained reader for delimited text, configured from the
/// table's own SerDe: the separator it names, whether its fields may be quoted, and
/// records of varying width — a Hive table's object may hold a line with fewer fields
/// than the table has columns, and the missing ones are simply absent.
fn delimited_reader(
    body: &[u8],
    separator: char,
    quote: Option<char>,
) -> Result<::csv::Reader<&[u8]>, String> {
    let separator = ascii_byte(separator).ok_or_else(|| {
        format!(
            "the field separator {separator:?} is not one character of the text this \
             reader reads"
        )
    })?;
    let mut builder = ::csv::ReaderBuilder::new();
    builder
        .has_headers(false)
        .flexible(true)
        .delimiter(separator)
        .quoting(quote.is_some());
    if let Some(quote) = quote.and_then(ascii_byte) {
        builder.quote(quote);
    }
    Ok(builder.from_reader(body))
}

/// The one byte `character` is written as, or `None` for a character that is not one byte
/// of text.
fn ascii_byte(character: char) -> Option<u8> {
    character.is_ascii().then_some(character as u8)
}

/// The member `name` of a JSON document, as the text its value denotes.
///
/// A JSON SerDe reads a document's members as the table's columns by name, folded the way
/// the catalog folds a column name, so a document whose member is spelled in another case
/// still fills its column.
fn json_member(
    members: &::serde_json::Map<String, ::serde_json::Value>,
    name: &str,
) -> Option<String> {
    let value = members.get(name).or_else(|| {
        members
            .iter()
            .find(|(member, _)| member.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })?;
    match value {
        ::serde_json::Value::Null => None,
        ::serde_json::Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the decoding table: the bytes one object holds, the SerDe that reads
    /// them, and the records they decode to.
    struct DecodeCase {
        /// What this row is about.
        case: &'static str,
        /// The bytes one object at the table's location holds.
        body: &'static str,
        /// What separates two fields.
        separator: char,
        /// What a field may be wrapped in.
        quote: Option<char>,
        /// The records the object holds, each as its fields.
        records: Vec<Vec<&'static str>>,
    }

    /// An object of delimited text decodes to the records its SerDe says it holds: the
    /// rows differ along the separator, whether fields are quoted, how the lines are
    /// terminated, how many records the object carries and whether a field is empty or a
    /// record short.
    #[test]
    fn an_object_decodes_to_the_records_its_serde_declares() {
        let cases: Vec<DecodeCase> = vec![
            DecodeCase {
                case: "the default Hive field separator, over several records",
                body: "1\u{1}desk\u{1}249.5\n2\u{1}lamp\u{1}19.0\n",
                separator: DEFAULT_FIELD_DELIMITER,
                quote: None,
                records: vec![vec!["1", "desk", "249.5"], vec!["2", "lamp", "19.0"]],
            },
            DecodeCase {
                case: "a comma separator",
                body: "1,desk,249.5\n",
                separator: ',',
                quote: None,
                records: vec![vec!["1", "desk", "249.5"]],
            },
            DecodeCase {
                case: "a tab separator and no final line terminator",
                body: "1\tdesk",
                separator: '\t',
                quote: None,
                records: vec![vec!["1", "desk"]],
            },
            DecodeCase {
                case: "an empty field between separators",
                body: "1,,3\n",
                separator: ',',
                quote: None,
                records: vec![vec!["1", "", "3"]],
            },
            DecodeCase {
                case: "records terminated the way a Windows writer terminates them",
                body: "1,desk\r\n2,lamp\r\n",
                separator: ',',
                quote: None,
                records: vec![vec!["1", "desk"], vec!["2", "lamp"]],
            },
            DecodeCase {
                case: "an empty line between records",
                body: "1,desk\n\n2,lamp\n",
                separator: ',',
                quote: None,
                records: vec![vec!["1", "desk"], vec!["2", "lamp"]],
            },
            DecodeCase {
                case: "a record shorter than the one before it",
                body: "1,desk,249.5\n2,lamp\n",
                separator: ',',
                quote: None,
                records: vec![vec!["1", "desk", "249.5"], vec!["2", "lamp"]],
            },
            DecodeCase {
                case: "a quoted field carrying the separator",
                body: "1,\"desk, oak\",3\n",
                separator: ',',
                quote: Some('"'),
                records: vec![vec!["1", "desk, oak", "3"]],
            },
            DecodeCase {
                case: "a doubled quote inside a quoted field",
                body: "\"a \"\"b\"\" c\",2\n",
                separator: ',',
                quote: Some('"'),
                records: vec![vec!["a \"b\" c", "2"]],
            },
            DecodeCase {
                case: "a quote character where fields are not quoted at all",
                body: "1,\"desk\n",
                separator: ',',
                quote: None,
                records: vec![vec!["1", "\"desk"]],
            },
            DecodeCase {
                case: "an object holding nothing",
                body: "",
                separator: ',',
                quote: None,
                records: vec![],
            },
        ];

        for case in cases {
            let mut reader =
                delimited_reader(case.body.as_bytes(), case.separator, case.quote)
                    .unwrap_or_else(|reason| {
                        panic!("case {:?}: the object must be readable, got {reason:?}", case.case)
                    });
            let actual: Vec<Vec<String>> = reader
                .records()
                .map(|record| {
                    let record = record.unwrap_or_else(|reason| {
                        panic!("case {:?}: the object must decode, got {reason:?}", case.case)
                    });
                    record.iter().map(ToString::to_string).collect()
                })
                .collect();
            assert_eq!(
                actual, case.records,
                "case {:?}: expected the records {:?}, got {actual:?}",
                case.case, case.records
            );
        }
    }

    /// A column's Hive type decides the type the engine holds it as, so a type the
    /// catalog records maps to the one Athena reports for that column.
    #[test]
    fn a_hive_column_type_maps_to_the_type_the_engine_holds_it_as() {
        // (the Hive type the catalog holds, the engine type it is read as)
        let cases: Vec<(&str, &str)> = vec![
            ("bigint", TYPE_BIGINT),
            ("int", TYPE_INTEGER),
            ("integer", TYPE_INTEGER),
            ("smallint", TYPE_INTEGER),
            ("tinyint", TYPE_INTEGER),
            ("double", TYPE_DOUBLE),
            ("float", TYPE_DOUBLE),
            ("decimal(10,2)", TYPE_DOUBLE),
            ("boolean", TYPE_BOOLEAN),
            ("string", TYPE_TEXT),
            ("varchar(20)", TYPE_TEXT),
            ("char(3)", TYPE_TEXT),
            ("date", TYPE_TEXT),
            ("timestamp", TYPE_TEXT),
            ("array<string>", TYPE_TEXT),
            ("binary", TYPE_BLOB),
            ("BIGINT", TYPE_BIGINT),
        ];

        for (declared, expected) in cases {
            let actual = engine_type(declared);
            assert_eq!(
                actual, expected,
                "case {declared:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// An `s3://` location names a bucket and a prefix, and the prefix a table's data
    /// lives under never carries the separator that closed it.
    #[test]
    fn a_location_parses_into_a_bucket_and_a_prefix() {
        // (case, the location, the bucket it names, the prefix it names)
        let cases: Vec<(&str, &str, Option<&str>, &str)> = vec![
            (
                "a prefix with a trailing separator",
                "s3://bucket/data/",
                Some("bucket"),
                "data",
            ),
            (
                "a prefix with none",
                "s3://bucket/data",
                Some("bucket"),
                "data",
            ),
            (
                "a nested prefix",
                "s3://bucket/a/b/c/",
                Some("bucket"),
                "a/b/c",
            ),
            ("the bucket root", "s3://bucket", Some("bucket"), ""),
            (
                "the bucket root with a separator",
                "s3://bucket/",
                Some("bucket"),
                "",
            ),
            ("no object store at all", "hdfs://cluster/data", None, ""),
            ("no bucket", "s3:///data", None, ""),
        ];

        for (case, location, bucket, prefix) in cases {
            let actual = parse_s3_uri(location);
            let expected =
                bucket.map(|bucket| (bucket.to_string(), prefix.to_string()));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// A timestamp field is held as the instant it names, written to the precision the
    /// type carries: the rows differ along how many digits of a second the stored text
    /// writes, and one row is text that names no instant at all.
    #[test]
    fn a_timestamp_field_is_held_to_the_precision_its_type_carries() {
        // (the stored text, the value its column holds)
        let cases: Vec<(&str, &str)> = vec![
            ("2024-01-02 03:04:05", "2024-01-02 03:04:05.000"),
            ("2024-01-02 03:04:05.1", "2024-01-02 03:04:05.100"),
            ("2024-01-02 03:04:05.12", "2024-01-02 03:04:05.120"),
            ("2024-01-02 03:04:05.123", "2024-01-02 03:04:05.123"),
            ("2024-01-02 03:04:05.123456", "2024-01-02 03:04:05.123"),
            ("2024-01-02T03:04:05", "2024-01-02T03:04:05.000"),
            ("  2024-01-02 03:04:05  ", "2024-01-02 03:04:05.000"),
            ("not an instant", "not an instant"),
            ("2024-01-02", "2024-01-02"),
        ];

        for (stored, expected) in cases {
            let actual = timestamp_text(stored);
            assert_eq!(
                actual, expected,
                "case {stored:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The field separator a SerDe parameter names: the serialization format carries a
    /// character CODE, and every other parameter the character itself.
    #[test]
    fn a_serde_parameter_names_the_separator_it_carries() {
        // (case, the parameter, its value, the separator it names)
        let cases: Vec<(&str, &str, &str, Option<char>)> = vec![
            (
                "the default separator, as its code",
                CODED_PARAMETER,
                "1",
                Some(DEFAULT_FIELD_DELIMITER),
            ),
            ("a comma as a code", CODED_PARAMETER, "44", Some(',')),
            ("a comma as itself", CODED_PARAMETER, ",", Some(',')),
            ("a field delimiter", "field.delim", ",", Some(',')),
            ("a field delimiter written as a digit", "field.delim", "1", Some('1')),
            ("more than one character", "field.delim", "::", None),
            ("nothing at all", "field.delim", "", None),
        ];

        for (case, parameter, value, expected) in cases {
            let actual = separator_char(parameter, Some(&value.to_string()));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// What a read of a table's location skips: the prefix markers and the files Hive
    /// hides, and nothing else.
    #[test]
    fn a_read_skips_the_objects_hive_hides() {
        // (the key, whether a read skips it)
        let cases: Vec<(&str, bool)> = vec![
            ("data/rows.txt", false),
            ("data/nested/rows.txt", false),
            ("data/", true),
            ("data/_SUCCESS", true),
            ("data/.hidden", true),
            ("data/_$folder$", true),
        ];

        for (key, expected) in cases {
            let actual = is_hidden(key);
            assert_eq!(
                actual, expected,
                "case {key:?}: expected hidden={expected}, got {actual}"
            );
        }
    }
}
