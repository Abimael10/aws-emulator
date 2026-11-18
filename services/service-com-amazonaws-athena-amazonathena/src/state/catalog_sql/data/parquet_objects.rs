// SPDX-License-Identifier: CC0-1.0

//! The RECORDS one object of the columnar Parquet format holds.
//!
//! A Data Catalog table says how the objects at its location are written
//! (`StorageDescriptor$SerdeInfo` and `StorageDescriptor$InputFormat`), and a table whose
//! descriptor names the Parquet format holds Parquet FILES — a self-describing, columnar,
//! per-column-compressed format, not lines of text. So such an object is read by the Apache
//! implementation of that format, which is also the one this workspace writes such objects
//! with: the values a read reports are the values that reader decoded out of the file's own
//! pages, and nothing here reads the bytes as text or infers a value from the table's
//! definition.
//!
//! A record's values are taken BY NAME, the way Hive's Parquet SerDe resolves a column
//! against a file's schema: a file carrying a column the table does not declare contributes
//! nothing for it, and a column the table declares that the file does not carry is NULL.

use ::std::collections::HashMap;

use ::parquet::file::reader::{FileReader, SerializedFileReader};
use ::parquet::record::{Field, Row};
use ::service_com_amazonaws_glue_awsglue::state as glue_state;

use super::{ddl, Budget, Failure, Relation};

/// The serialization libraries that read the columnar Parquet format: the one Athena's own
/// `STORED AS PARQUET` records ([`ddl::PARQUET_SERDE`]), and the name the Hive integration
/// of that format originally shipped under, which a catalog entry an older writer wrote
/// still carries.
const PARQUET_SERDES: &[&str] = &[ddl::PARQUET_SERDE, "parquet.hive.serde.ParquetHiveSerDe"];

/// The input formats of a table whose objects are Parquet files; the second spelling is the
/// original one, for the same reason [`PARQUET_SERDES`] carries two.
const PARQUET_INPUT_FORMATS: &[&str] = &[
    ddl::PARQUET_INPUT_FORMAT,
    "parquet.hive.DeprecatedParquetInputFormat",
];

/// How many digits of a second each kind of timestamp leaf carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Precision {
    /// A millisecond leaf, which carries three.
    Millisecond,
    /// A microsecond leaf, which carries six.
    Microsecond,
}

impl Precision {
    /// The instant `count` of these sub-seconds after the epoch names.
    fn instant(self, count: i64) -> Option<::chrono::DateTime<::chrono::Utc>> {
        match self {
            Self::Millisecond => ::chrono::DateTime::from_timestamp_millis(count),
            Self::Microsecond => ::chrono::DateTime::from_timestamp_micros(count),
        }
    }

    /// How the time of day is written out at this precision.
    fn format(self) -> &'static str {
        match self {
            Self::Millisecond => "%Y-%m-%d %H:%M:%S%.3f",
            Self::Microsecond => "%Y-%m-%d %H:%M:%S%.6f",
        }
    }
}

/// Whether the storage descriptor of a table says its objects are Parquet files.
///
/// EITHER half says so, as for every other format the catalog records: the serialization
/// library names the SerDe the records are written by, and the input format names how the
/// objects themselves are — a catalog entry carrying one of them and not the other still
/// names this format.
pub fn stores_parquet(library: &str, input_format: Option<&str>) -> bool {
    PARQUET_SERDES
        .iter()
        .any(|serde| library.trim().eq_ignore_ascii_case(serde))
        || input_format.is_some_and(|format| {
            PARQUET_INPUT_FORMATS
                .iter()
                .any(|known| format.trim().eq_ignore_ascii_case(known))
        })
}

/// What one read of a Parquet object may decode, as the query reading it can still afford.
///
/// A columnar object is held COMPRESSED and column by column, and what one of its pages
/// decodes to is DECLARED in the file's own metadata — so the declaration is read as a lower
/// bound on what decoding the object would cost, and a ROW GROUP declaring more than the
/// query can hold is refused before a page of it is read. What the records really decode to
/// is charged record by record by the caller, which is the authoritative bound and the one
/// that stops a file whose groups each fit but whose records together do not; the stored
/// bytes the object was fetched under bound its compressed size only.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// How many records the rest of this query may still materialise.
    pub records: usize,
    /// How many bytes of decoded object data it may still hold.
    pub decoded_bytes: usize,
}

/// Read `body` as one Parquet object, handing `take` every record it holds in file order.
///
/// `split` is where the object lives, which is what a failure to OPEN it names: the unit of
/// work Hive calls a split is one object of the table's location, and an object that is not
/// the Parquet file the table says it is cannot be opened as one at all.
///
/// STREAMING: the reader decodes one record at a time and each is handed over as it is
/// read, so a caller that charges a record against a budget stops the read at the first one
/// over it rather than after the whole object is decoded.
pub fn for_each_record(
    body: &[u8],
    relation: &Relation,
    split: &str,
    limits: Limits,
    mut take: impl FnMut(&Row) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let reader = SerializedFileReader::new(::bytes::Bytes::copy_from_slice(body))
        .map_err(|reason| cannot_open_split(split, &reason.to_string()))?;
    for group in reader.metadata().row_groups() {
        if declared(group.num_rows()) > limits.records {
            return Err(Budget::too_many_rows(relation));
        }
        let chunks = group
            .columns()
            .iter()
            .map(|chunk| declared(chunk.uncompressed_size()))
            .fold(0usize, usize::saturating_add);
        if declared(group.total_byte_size()).max(chunks) > limits.decoded_bytes {
            return Err(Budget::too_many_bytes(relation));
        }
    }
    let records = reader
        .get_row_iter(None)
        .map_err(|reason| cannot_open_split(split, &reason.to_string()))?;
    for record in records {
        let record = record.map_err(|reason| bad_data(relation, &reason.to_string()))?;
        take(&record)?;
    }
    Ok(())
}

/// One count a file's own metadata declares, as the size it stands for: a count this
/// platform's own counts cannot hold declares more than any budget allows.
fn declared(count: i64) -> usize {
    usize::try_from(count).unwrap_or(usize::MAX)
}

/// The values one record holds, keyed by the name each was written under, folded the way the
/// Data Catalog folds a column name — so a file that spells a column in another case still
/// fills it, exactly as a JSON document's member does.
///
/// A leaf carrying no value at all is left out: a column the record holds no value for is
/// NULL, which is the same answer as one the file does not carry.
pub fn values_of(record: &Row) -> HashMap<String, Cell> {
    let mut values: HashMap<String, Cell> = HashMap::new();
    for (name, field) in record.get_column_iter() {
        if let Some(cell) = cell_of(field) {
            values.insert(glue_state::fold_name(name), cell);
        }
    }
    values
}

/// One value a Parquet record carries, as the data it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cell {
    /// The text the value reads as, which is how every Parquet type but binary data is
    /// held: a number, a boolean, a string, a calendar value or a nested structure.
    Text(String),
    /// Binary data, held as the BYTES themselves: a binary column's value is not text, so it
    /// never passes through a text reading of it.
    Bytes(Vec<u8>),
}

impl Cell {
    /// This value as the text a column reads it from.
    pub fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Bytes(data) => String::from_utf8_lossy(data).into_owned(),
        }
    }

    /// How many bytes holding this value costs.
    pub fn size(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Bytes(data) => data.len(),
        }
    }
}

/// One Parquet leaf as the value it holds, or `None` for a leaf holding no value.
///
/// Each kind of leaf is rendered as the VALUE it carries rather than by the crate's own
/// `Display`, which quotes a string and writes a word for a null: a calendar leaf carries a
/// count from the epoch, so the text its column holds is that instant written out, and every
/// other primitive is the number, boolean or string it is.
fn cell_of(field: &Field) -> Option<Cell> {
    let text = match field {
        Field::Null => return None,
        Field::Str(value) => value.clone(),
        // Binary data is the one kind of value that is not text.
        Field::Bytes(value) => return Some(Cell::Bytes(value.data().to_vec())),
        Field::Date(days) => date_text(i64::from(*days))?,
        Field::TimestampMillis(millis) => instant_text(*millis, Precision::Millisecond)?,
        Field::TimestampMicros(micros) => instant_text(*micros, Precision::Microsecond)?,
        // Every remaining leaf — a number, a boolean, a time of day, or a nested group,
        // list or map — renders as the value it holds.
        other => other.to_string(),
    };
    Some(Cell::Text(text))
}

/// The calendar date `days` after the epoch, as a `date` column holds one.
fn date_text(days: i64) -> Option<String> {
    let epoch = ::chrono::NaiveDate::from_ymd_opt(1970, 1, 1)?;
    Some(
        epoch
            .checked_add_signed(::chrono::TimeDelta::try_days(days)?)?
            .format("%Y-%m-%d")
            .to_string(),
    )
}

/// The instant `count` sub-seconds after the epoch, as a `timestamp` column holds one: the
/// calendar date and the time of day, to the digits of a second `precision` carries.
fn instant_text(count: i64, precision: Precision) -> Option<String> {
    Some(
        precision
            .instant(count)?
            .format(precision.format())
            .to_string(),
    )
}

/// The failure a read of an object that is not the Parquet file the table says it is
/// reports: Hive's own class for a split it cannot OPEN, naming the object and why the
/// format's own reader refused it.
fn cannot_open_split(split: &str, reason: &str) -> Failure {
    Failure::user(format!(
        "HIVE_CANNOT_OPEN_SPLIT: Error opening Hive split {split}: {reason}"
    ))
}

/// The failure a record of an object the reader DID open reports when that record cannot be
/// read: Hive's own class for data that is not what the table declares, which is the same
/// class a text object with unreadable records is reported under.
fn bad_data(relation: &Relation, reason: &str) -> Failure {
    Failure::user(format!(
        "HIVE_BAD_DATA: Error reading the data of {}.{}: {reason}",
        relation.database, relation.table
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A storage descriptor names the Parquet format through either half of what the
    /// catalog records, in whatever case the writer spelled it, and names it through
    /// neither for the other formats a catalog entry can carry.
    #[test]
    fn a_storage_descriptor_names_the_parquet_format_through_either_half() {
        // (case, the serialization library, the input format, whether it names Parquet)
        let cases: Vec<(&str, &str, Option<&str>, bool)> = vec![
            (
                "the SerDe and the input format Athena's own STORED AS PARQUET records",
                ddl::PARQUET_SERDE,
                Some(ddl::PARQUET_INPUT_FORMAT),
                true,
            ),
            (
                "the SerDe alone, on an entry carrying no input format",
                ddl::PARQUET_SERDE,
                None,
                true,
            ),
            (
                "the input format alone, on an entry whose SerDe says nothing",
                "",
                Some(ddl::PARQUET_INPUT_FORMAT),
                true,
            ),
            (
                "the original spelling of both names",
                "parquet.hive.serde.ParquetHiveSerDe",
                Some("parquet.hive.DeprecatedParquetInputFormat"),
                true,
            ),
            (
                "the names spelled in another case, and padded",
                " ORG.APACHE.HADOOP.HIVE.QL.IO.PARQUET.SERDE.PARQUETHIVESERDE ",
                None,
                true,
            ),
            (
                "delimited text",
                ddl::LAZY_SIMPLE_SERDE,
                Some(ddl::TEXT_INPUT_FORMAT),
                false,
            ),
            (
                "the optimised row-columnar format, which is columnar but not this one",
                ddl::ORC_SERDE,
                Some(ddl::ORC_INPUT_FORMAT),
                false,
            ),
            (
                "an Avro container",
                ddl::AVRO_SERDE,
                Some(ddl::AVRO_INPUT_FORMAT),
                false,
            ),
        ];
        for (case, library, input_format, expected) in cases {
            let named = stores_parquet(library, input_format);
            assert_eq!(
                named, expected,
                "case {case:?}: expected stores_parquet == {expected}, got {named}"
            );
        }
    }

    /// A calendar leaf holds the instant it counts from the epoch, to the digits of a
    /// second its own kind carries: the rows differ along the kind of leaf and the sign of
    /// the count, and one row is a count no calendar date answers for.
    #[test]
    fn a_calendar_leaf_holds_the_instant_it_counts() {
        // (case, the leaf, the value its column holds)
        let cases: Vec<(&str, Field, Option<&str>)> = vec![
            ("the epoch itself, as a date", Field::Date(0), Some("1970-01-01")),
            ("a date after the epoch", Field::Date(19_724), Some("2024-01-02")),
            ("a date before the epoch", Field::Date(-1), Some("1969-12-31")),
            (
                "a timestamp to the millisecond",
                Field::TimestampMillis(1_704_164_645_123),
                Some("2024-01-02 03:04:05.123"),
            ),
            (
                "a timestamp with no fraction of a second",
                Field::TimestampMillis(0),
                Some("1970-01-01 00:00:00.000"),
            ),
            (
                "a timestamp to the microsecond",
                Field::TimestampMicros(1_704_164_645_123_456),
                Some("2024-01-02 03:04:05.123456"),
            ),
            (
                "a day count no calendar date answers for",
                Field::Date(i32::MAX),
                None,
            ),
        ];
        for (case, field, expected) in cases {
            let held = cell_of(&field);
            let expected = expected.map(|text| Cell::Text(text.to_string()));
            assert_eq!(
                held, expected,
                "case {case:?}: expected {expected:?}, got {held:?}"
            );
        }
    }

    /// Every other kind of leaf holds the value it carries, and a leaf holding none is left
    /// out of the record altogether.
    #[test]
    fn every_other_leaf_holds_the_value_it_carries() {
        // (case, the leaf, the value its column holds)
        let cases: Vec<(&str, Field, Option<Cell>)> = vec![
            ("a leaf holding no value", Field::Null, None),
            (
                "a string, which the crate's own rendering would quote",
                Field::Str("alpha".to_string()),
                Some(Cell::Text("alpha".to_string())),
            ),
            (
                "an empty string",
                Field::Str(String::new()),
                Some(Cell::Text(String::new())),
            ),
            ("a boolean", Field::Bool(true), Some(Cell::Text("true".to_string()))),
            ("a 32-bit integer", Field::Int(-7), Some(Cell::Text("-7".to_string()))),
            (
                "a 64-bit integer",
                Field::Long(9_007_199_254_740_993),
                Some(Cell::Text("9007199254740993".to_string())),
            ),
            (
                "a double",
                Field::Double(1.5),
                Some(Cell::Text("1.5".to_string())),
            ),
            (
                "binary data, which is held as the bytes themselves",
                Field::Bytes(::parquet::data_type::ByteArray::from(vec![0, 159, 2])),
                Some(Cell::Bytes(vec![0, 159, 2])),
            ),
        ];
        for (case, field, expected) in cases {
            let held = cell_of(&field);
            assert_eq!(
                held, expected,
                "case {case:?}: expected {expected:?}, got {held:?}"
            );
        }
    }
}
