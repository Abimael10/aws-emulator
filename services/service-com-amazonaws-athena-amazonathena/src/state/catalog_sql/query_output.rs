// SPDX-License-Identifier: CC0-1.0

//! The OBJECTS a statement WRITES: the format they hold the rows in, where under the
//! destination each one lands, and whether that destination may be written at all.
//!
//! Athena's `CREATE TABLE … AS <query>` and `UNLOAD (<query>) TO '<location>'` do not
//! answer with rows: they WRITE the rows their query produced into the object store, as
//! objects of the format the statement's `format` property names, under the location it
//! names. So this module is where those objects come from — the bytes are produced by the
//! maintained writer for each format, the layout is Hive's own (one directory per partition
//! value, named `<key>=<value>`, which is the layout a read of such a table resolves a
//! partition from), and nothing is reported as written that the object store did not take.
//!
//! The destination is REFUSED when it already holds objects: a statement that would write
//! over data already there fails instead, which is the same rule for both statements —
//! only the sentence each reports differs ([`super::Failure`]).

use ::std::collections::HashMap;
use ::std::io::Write;

use ::service_com_amazonaws_s3_amazons3::state as s3_state;

use super::data::{hive_partition_location, parse_s3_uri, under_prefix};
use super::statement::{Position, StorageFormat};
use super::Failure;
use crate::state::query_results::{self, ResultEncryption, ResultObject};

/// The formats a statement's output can be written in, each named by the value Athena's
/// `format` property carries for it.
///
/// Source: the `format` property of Athena's `CREATE TABLE AS SELECT` and `UNLOAD`, whose
/// values are `ORC`, `PARQUET`, `AVRO`, `JSON` and `TEXTFILE`. The three here are the ones
/// this emulator both writes and reads back: a table written in one of them is read by the
/// decoder its catalog entry names ([`super::data`]), so what a later query reads is the
/// rows that were written. The two row-columnar container formats left out are reported as
/// formats this emulator cannot write ([`OutputFormat::named`]) rather than written as
/// something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// `PARQUET`: the columnar format, written by the Apache implementation of it.
    Parquet,
    /// `JSON`: one JSON document per line, whose members are the columns.
    Json,
    /// `TEXTFILE`: one line per row, fields separated by Hive's own default separator.
    Text,
}

/// The `format` value Athena's property takes for each format, so the name a statement
/// writes and the format it selects are one list.
const FORMAT_NAMES: &[(&str, OutputFormat)] = &[
    ("PARQUET", OutputFormat::Parquet),
    ("JSON", OutputFormat::Json),
    ("TEXTFILE", OutputFormat::Text),
];

/// The format a `CREATE TABLE … AS <query>` that declares none writes: Athena's documented
/// default for the property ("format — … If omitted, `PARQUET` is used by default").
pub const DEFAULT_TABLE_FORMAT: OutputFormat = OutputFormat::Parquet;

/// What a NULL is written as in a TEXTFILE object: the text Hive writes a null field as by
/// default, which is also what a read of such an object reads back as a NULL
/// (`super::data`'s own default null text).
const TEXT_NULL: &str = "\\N";

/// What separates two fields of a TEXTFILE object: Hive's own default field separator, the
/// ASCII unit separator — the separator a table whose SerDe names none is read with
/// ([`super::data::DEFAULT_FIELD_DELIMITER`]).
const TEXT_SEPARATOR: char = super::data::DEFAULT_FIELD_DELIMITER;

/// The value a partition key carries in a path when the rows of that partition have NO
/// value for it: Hive's own name for the partition a null key value belongs to.
pub const HIVE_DEFAULT_PARTITION: &str = "__HIVE_DEFAULT_PARTITION__";

/// How many objects one statement may write. The EMULATOR's own bound: one object per
/// partition the rows fall into, and the partition values are the query's own, so without
/// it one statement could make this process write an object per row.
const MAX_WRITTEN_OBJECTS: usize = 1_024;

impl OutputFormat {
    /// The format the `format` property value `name` names, or why it names none this
    /// emulator writes.
    pub fn named(name: &str) -> Result<Self, Failure> {
        FORMAT_NAMES
            .iter()
            .find(|(known, _)| name.trim().eq_ignore_ascii_case(known))
            .map(|(_, format)| *format)
            .ok_or_else(|| {
                Failure::emulator(format!(
                    "this emulator writes a query's output in the {} formats, so the \
                     {name} format this statement asked for is not one it can write",
                    FORMAT_NAMES
                        .iter()
                        .map(|(known, _)| *known)
                        .collect::<Vec<&str>>()
                        .join(", ")
                ))
            })
    }

    /// The extension the objects of this format carry, so an object says what it holds —
    /// including the compression it is written under, which is what a reader of such an
    /// object picks its decompressor by.
    fn extension(self) -> &'static str {
        match self {
            Self::Parquet => "parquet",
            Self::Json => "json.gz",
            Self::Text => "txt.gz",
        }
    }

    /// The media type the object store records for such an object.
    fn content_type(self) -> &'static str {
        match self {
            // Both are binary: a Parquet file is a columnar container, and the two text
            // formats are written as the gzip stream their extension names.
            Self::Parquet => "application/octet-stream",
            Self::Json | Self::Text => "application/gzip",
        }
    }

    /// The `STORED AS` storage format a catalog entry over objects of this format declares,
    /// which is what decides the input format, the output format and the SerDe the catalog
    /// records ([`super::ddl`]).
    pub fn storage(self) -> StorageFormat {
        StorageFormat::Named(
            match self {
                Self::Parquet => "PARQUET",
                // Hive's `STORED AS JSONFILE` is text read by the JSON SerDe, which is how
                // a table of one JSON document per line is stored.
                Self::Json => "JSONFILE",
                Self::Text => "TEXTFILE",
            }
            .to_string(),
        )
    }

    /// The bytes an object of this format holds for `sheet`.
    ///
    /// The two TEXT formats are written COMPRESSED: the compression a statement that writes
    /// its output applies by default is gzip, so the object holds a gzip stream and its key
    /// names that codec ([`Self::extension`]) — which is what a read of it decompresses by.
    /// The columnar format compresses its own pages instead ([`parquet_object`]).
    fn encode(self, sheet: &Sheet<'_>) -> Result<Vec<u8>, Failure> {
        match self {
            Self::Parquet => parquet_object(sheet),
            Self::Json => gzipped(json_object(sheet)),
            Self::Text => gzipped(text_object(sheet)),
        }
    }
}

/// One column of the values a statement writes: the name it is written under, and the
/// Athena type its values have.
#[derive(Debug, Clone, Copy)]
pub struct SheetColumn<'v> {
    /// The column name.
    pub name: &'v str,
    /// The type name the result set reports for it, which is what decides the leaf a
    /// columnar writer writes it as.
    pub data_type: &'v str,
}

/// The values one object holds: the columns, and the rows under them.
#[derive(Debug, Clone)]
pub struct Sheet<'v> {
    /// The columns, in the order they are written.
    pub columns: Vec<SheetColumn<'v>>,
    /// The rows, each with one value per column and `None` where that value is NULL.
    pub rows: Vec<Vec<Option<&'v str>>>,
}

/// Where one statement's output goes.
#[derive(Debug, Clone)]
pub struct Destination {
    /// The `s3://` prefix the objects land under, without the separator that closed it.
    pub location: String,
    /// That same location AS THE STATEMENT WROTE IT, which is how a refusal about the
    /// destination names it: the statement named a prefix, and the refusal reports the
    /// prefix it named.
    pub written: String,
    /// The bucket that prefix names.
    pub bucket: String,
    /// The key prefix inside the bucket.
    pub prefix: String,
    /// The format the objects are written in.
    pub format: OutputFormat,
    /// The name every object written here carries before its extension: the execution that
    /// wrote it, so an object says which query produced it.
    pub name: String,
}

impl Destination {
    /// The destination `location` names, or why it names none.
    ///
    /// A destination is an Amazon S3 location naming a PREFIX: the objects a statement
    /// writes land under it, so a location that is no object-store path at all names
    /// nowhere the output could go. `at` is where the statement that named it appears in the
    /// submitted text, which is where a refusal about it reports the mistake.
    pub fn of(
        location: &str,
        format: OutputFormat,
        name: &str,
        at: Position,
    ) -> Result<Self, Failure> {
        let location = location.trim();
        let Some((bucket, prefix)) = parse_s3_uri(location) else {
            return Err(Failure::invalid_destination(at, location));
        };
        Ok(Self {
            location: location.trim_end_matches('/').to_string(),
            written: location.to_string(),
            bucket,
            prefix,
            format,
            name: name.to_string(),
        })
    }

    /// Whether the object store holds the BUCKET this destination names at all.
    ///
    /// A destination in a bucket that is not there is nowhere a statement's output could go,
    /// so it is refused BEFORE anything is written or registered — leaving no catalog entry
    /// over a location nothing holds.
    pub fn bucket_is_absent<Eng: Send + Sync + 'static>(&self, engine: &Eng) -> bool {
        let Some(state) = s3_state::service_state(engine) else {
            return false;
        };
        let store = state.lock();
        store.bucket(&self.bucket).is_none()
    }

    /// Whether the object store already holds an object under this destination.
    ///
    /// A key that IS a folder — one the object store holds as the marker of a prefix
    /// nothing is under — is no object there: what decides is whether the destination holds
    /// data, which is what a write over it would destroy.
    pub fn is_occupied<Eng: Send + Sync + 'static>(&self, engine: &Eng) -> bool {
        let Some(state) = s3_state::service_state(engine) else {
            return false;
        };
        let store = state.lock();
        store.bucket(&self.bucket).is_some_and(|bucket| {
            bucket
                .objects()
                .any(|(key, _)| under_prefix(key, &self.prefix) && !key.ends_with('/'))
        })
    }
}

/// One object a statement wrote.
#[derive(Debug, Clone)]
pub struct WrittenObject {
    /// Its `s3://` URI, which is what the data manifest lists it as.
    pub uri: String,
    /// The bytes it holds.
    pub body: Vec<u8>,
    /// Its media type.
    pub content_type: &'static str,
}

/// One slice of the rows a statement writes: the partition they belong to, and where their
/// object goes.
#[derive(Debug, Clone)]
pub struct WrittenPartition {
    /// The partition's values, one per partition key and in key order; empty for the rows
    /// of a statement that partitions its output by nothing.
    pub values: Vec<String>,
    /// Which rows of the result set belong to it, in result order.
    pub rows: Vec<usize>,
}

/// The partitions the rows of `values` fall into, by the LAST `keys` values of each row.
///
/// Hive's own partitioning: a partitioned table's partition keys are the last columns of
/// its rows, and every row carrying the same values for them belongs to one partition,
/// which is one data directory. The partitions are answered in FIRST-APPEARANCE order, so
/// what is written is decided by the rows and not by a hash of them.
pub fn partitioned(rows: &[Vec<Option<&str>>], keys: usize) -> Result<Vec<WrittenPartition>, Failure> {
    let mut partitions: Vec<WrittenPartition> = Vec::new();
    if keys == 0 {
        partitions.push(WrittenPartition {
            values: Vec::new(),
            rows: (0..rows.len()).collect(),
        });
        return Ok(partitions);
    }
    let mut at: HashMap<Vec<String>, usize> = HashMap::new();
    for (position, row) in rows.iter().enumerate() {
        let values: Vec<String> = row
            .iter()
            .skip(row.len().saturating_sub(keys))
            .map(|value| value.unwrap_or(HIVE_DEFAULT_PARTITION).to_string())
            .collect();
        match at.get(&values) {
            Some(index) => {
                if let Some(partition) = partitions.get_mut(*index) {
                    partition.rows.push(position);
                }
            }
            None => {
                if partitions.len() >= MAX_WRITTEN_OBJECTS {
                    return Err(Failure::emulator(format!(
                        "this statement's output falls into more than the \
                         {MAX_WRITTEN_OBJECTS} objects this emulator writes for one query"
                    )));
                }
                at.insert(values.clone(), partitions.len());
                partitions.push(WrittenPartition {
                    values,
                    rows: vec![position],
                });
            }
        }
    }
    Ok(partitions)
}

/// Where the objects of ONE partition of a statement's output live: the destination itself
/// for output that is partitioned by nothing, and otherwise the Hive directory the
/// partition's values name under it ([`hive_partition_location`]).
pub fn partition_directory(
    destination: &Destination,
    keys: &[String],
    values: &[String],
) -> String {
    if values.is_empty() {
        destination.location.clone()
    } else {
        hive_partition_location(&destination.location, keys, values)
    }
}

/// The object one slice of a statement's output is written as: where it goes, and the bytes
/// it holds.
pub fn object_of(
    destination: &Destination,
    directory: &str,
    sheet: &Sheet<'_>,
) -> Result<WrittenObject, Failure> {
    Ok(WrittenObject {
        uri: format!(
            "{directory}/{}.{}",
            destination.name,
            destination.format.extension()
        ),
        body: destination.format.encode(sheet)?,
        content_type: destination.format.content_type(),
    })
}

/// Store `objects` in the object store on `engine`, and answer the URIs they were stored
/// under.
///
/// Each one is written through the ONE step every object of a query execution is stored by
/// ([`super::super::query_results::store_object`]), so the data a statement wrote, the
/// results of that execution and its data manifest are all stored under one rule — the
/// destination's own encryption among it — and read back by the object store's own
/// operations exactly like an object a client wrote itself.
///
/// A write the store would not take FAILS the statement, rather than being logged and
/// skipped the way a results object's is: the rows a statement was asked to write are its
/// whole result, so an execution cannot report success for objects nothing holds.
pub fn store<Eng: Send + Sync + 'static>(
    engine: &Eng,
    objects: Vec<WrittenObject>,
    encryption: &ResultEncryption,
) -> Result<Vec<String>, Failure> {
    let Some(state) = s3_state::service_state(engine) else {
        return Err(Failure::emulator(
            "the object store is not reachable on this engine, so no statement can write \
             its output",
        ));
    };
    let mut written: Vec<String> = Vec::with_capacity(objects.len());
    for object in objects {
        let Some((bucket, key)) = parse_s3_uri(&object.uri).filter(|(_, key)| !key.is_empty())
        else {
            return Err(Failure::destination_not_writable(&object.uri));
        };
        let uri = object.uri;
        let stored = query_results::store_object(
            &state,
            &bucket,
            &key,
            ResultObject {
                uri: uri.clone(),
                body: object.body,
                content_type: object.content_type,
            },
            encryption,
        );
        match stored {
            Some(s3_state::ObjectWrite::Created | s3_state::ObjectWrite::Replaced) => {
                written.push(uri);
            }
            _ => return Err(Failure::destination_not_writable(&uri)),
        }
    }
    Ok(written)
}

/// The gzip stream `body` is written as.
///
/// The compression a statement that writes its output applies to a TEXT object: gzip, the
/// default such a statement's `compression` (or `write_compression`) property takes for the
/// text formats. Written by the maintained gzip encoder, so what the object holds is a
/// stream any reader of that codec reads — including this crate's own
/// ([`super::data::text_codecs`]).
fn gzipped(body: Vec<u8>) -> Result<Vec<u8>, Failure> {
    let mut encoder =
        ::flate2::write::GzEncoder::new(Vec::new(), ::flate2::Compression::default());
    encoder
        .write_all(&body)
        .and_then(|()| encoder.finish())
        .map_err(|error| {
            Failure::emulator(format!(
                "this emulator could not compress the output of this statement: {error}"
            ))
        })
}

/// One object of delimited TEXT: one line per row, the fields separated by Hive's own
/// default separator and a NULL written as the text Hive writes one as.
fn text_object(sheet: &Sheet<'_>) -> Vec<u8> {
    let mut text = String::new();
    for row in &sheet.rows {
        for (position, value) in row.iter().enumerate() {
            if position > 0 {
                text.push(TEXT_SEPARATOR);
            }
            text.push_str(value.unwrap_or(TEXT_NULL));
        }
        text.push('\n');
    }
    text.into_bytes()
}

/// One object of JSON: one document per line, whose members are the columns.
///
/// A value is written as the JSON value its type denotes — a number as a number, a boolean
/// as a boolean, everything else as a string — and a NULL as the JSON null, which is what a
/// read of such an object reads back as a NULL.
fn json_object(sheet: &Sheet<'_>) -> Vec<u8> {
    let mut text = String::new();
    for row in &sheet.rows {
        let mut document = ::serde_json::Map::new();
        for (column, value) in sheet.columns.iter().zip(row) {
            document.insert(column.name.to_string(), json_value(column.data_type, *value));
        }
        text.push_str(&::serde_json::Value::Object(document).to_string());
        text.push('\n');
    }
    text.into_bytes()
}

/// One value as the JSON it is written as; see [`json_object`].
fn json_value(data_type: &str, value: Option<&str>) -> ::serde_json::Value {
    let Some(value) = value else {
        return ::serde_json::Value::Null;
    };
    match leaf_of(data_type) {
        Leaf::Int32 | Leaf::Int64 => value
            .parse::<i64>()
            .map(::serde_json::Value::from)
            .unwrap_or_else(|_| ::serde_json::Value::from(value)),
        Leaf::Double => ::serde_json::Number::from_f64(value.parse::<f64>().unwrap_or(f64::NAN))
            .map_or_else(
                || ::serde_json::Value::from(value),
                ::serde_json::Value::Number,
            ),
        Leaf::Boolean => value
            .parse::<bool>()
            .map(::serde_json::Value::from)
            .unwrap_or_else(|_| ::serde_json::Value::from(value)),
        Leaf::Text => ::serde_json::Value::from(value),
    }
}

/// The kind of PARQUET leaf one column's values are written as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leaf {
    /// A 32-bit integer.
    Int32,
    /// A 64-bit integer.
    Int64,
    /// A double-precision number.
    Double,
    /// A boolean.
    Boolean,
    /// UTF-8 text, which is what every other type is written as — the text the value
    /// carries, so nothing of it is lost.
    Text,
}

/// The leaf a column of the Athena type `data_type` is written as.
///
/// The numeric, integral and boolean types are written as the leaf that holds them, so a
/// read of the object gets a number back rather than the text of one; every other type —
/// character data, an exact-numeric value, a calendar value, a type this reader has no leaf
/// for — is written as the TEXT the result set reports it as, which is the form its column
/// reads it from either way ([`super::data::parquet_objects`]).
fn leaf_of(data_type: &str) -> Leaf {
    let named = data_type
        .split_once('(')
        .map_or(data_type, |(name, _)| name)
        .trim();
    match named.to_ascii_lowercase().as_str() {
        "tinyint" | "smallint" | "integer" | "int" => Leaf::Int32,
        "bigint" => Leaf::Int64,
        "double" | "real" | "float" => Leaf::Double,
        "boolean" => Leaf::Boolean,
        _ => Leaf::Text,
    }
}

/// The leaf a column is really written as: the one its type names, unless a value of it is
/// not a value of that leaf at all — in which case the column is written as text, so no
/// value is dropped for the sake of the declared type.
fn written_leaf(column: &SheetColumn<'_>, values: impl Iterator<Item = Option<String>>) -> Leaf {
    let leaf = leaf_of(column.data_type);
    let holds = values.flatten().all(|value| match leaf {
        Leaf::Int32 => value.parse::<i32>().is_ok(),
        Leaf::Int64 => value.parse::<i64>().is_ok(),
        Leaf::Double => value.parse::<f64>().is_ok(),
        Leaf::Boolean => value.parse::<bool>().is_ok(),
        Leaf::Text => true,
    });
    if holds {
        leaf
    } else {
        Leaf::Text
    }
}

/// One object of the columnar PARQUET format, written by the Apache implementation of it:
/// one leaf per column, each optional because a row need not carry a value for every
/// column.
fn parquet_object(sheet: &Sheet<'_>) -> Result<Vec<u8>, Failure> {
    use ::parquet::basic::{Compression, ConvertedType, GzipLevel, Repetition, Type as PhysicalType};
    use ::parquet::data_type::{
        BoolType, ByteArray, ByteArrayType, DoubleType, Int32Type, Int64Type,
    };
    use ::parquet::file::properties::WriterProperties;
    use ::parquet::file::writer::SerializedFileWriter;
    use ::parquet::schema::types::Type as SchemaType;

    let leaves: Vec<Leaf> = sheet
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            written_leaf(
                column,
                sheet
                    .rows
                    .iter()
                    .map(move |row| row.get(index).copied().flatten().map(str::to_string)),
            )
        })
        .collect();
    let mut fields = Vec::with_capacity(sheet.columns.len());
    for (column, leaf) in sheet.columns.iter().zip(&leaves) {
        let mut builder = SchemaType::primitive_type_builder(
            column.name,
            match leaf {
                Leaf::Int32 => PhysicalType::INT32,
                Leaf::Int64 => PhysicalType::INT64,
                Leaf::Double => PhysicalType::DOUBLE,
                Leaf::Boolean => PhysicalType::BOOLEAN,
                Leaf::Text => PhysicalType::BYTE_ARRAY,
            },
        )
        .with_repetition(Repetition::OPTIONAL);
        if *leaf == Leaf::Text {
            builder = builder.with_converted_type(ConvertedType::UTF8);
        }
        fields.push(::std::sync::Arc::new(
            builder.build().map_err(parquet_refusal)?,
        ));
    }
    let schema = SchemaType::group_type_builder("athena_schema")
        .with_fields(fields)
        .build()
        .map_err(parquet_refusal)?;
    let mut writer = SerializedFileWriter::new(
        Vec::new(),
        ::std::sync::Arc::new(schema),
        // The pages are COMPRESSED: gzip is the compression a statement that writes its
        // output applies to this format by default, and a columnar file carries its own
        // compression per page rather than around the whole object.
        ::std::sync::Arc::new(
            WriterProperties::builder()
                .set_compression(Compression::GZIP(GzipLevel::default()))
                .build(),
        ),
    )
    .map_err(parquet_refusal)?;
    if !sheet.rows.is_empty() {
        let mut group = writer.next_row_group().map_err(parquet_refusal)?;
        let mut index = 0usize;
        while let Some(mut column) = group.next_column().map_err(parquet_refusal)? {
            // The definition level of a value an OPTIONAL leaf carries is 1, and of one it
            // does not 0, which is how a NULL is written.
            let mut levels: Vec<i16> = Vec::with_capacity(sheet.rows.len());
            let mut present: Vec<&str> = Vec::new();
            for row in &sheet.rows {
                match row.get(index).copied().flatten() {
                    Some(value) => {
                        present.push(value);
                        levels.push(1);
                    }
                    None => levels.push(0),
                }
            }
            let leaf = leaves.get(index).copied().unwrap_or(Leaf::Text);
            let typed = &mut column;
            match leaf {
                Leaf::Int32 => {
                    let values: Vec<i32> = present
                        .iter()
                        .map(|value| value.parse().unwrap_or_default())
                        .collect();
                    typed
                        .typed::<Int32Type>()
                        .write_batch(&values, Some(&levels), None)
                        .map_err(parquet_refusal)?;
                }
                Leaf::Int64 => {
                    let values: Vec<i64> = present
                        .iter()
                        .map(|value| value.parse().unwrap_or_default())
                        .collect();
                    typed
                        .typed::<Int64Type>()
                        .write_batch(&values, Some(&levels), None)
                        .map_err(parquet_refusal)?;
                }
                Leaf::Double => {
                    let values: Vec<f64> = present
                        .iter()
                        .map(|value| value.parse().unwrap_or_default())
                        .collect();
                    typed
                        .typed::<DoubleType>()
                        .write_batch(&values, Some(&levels), None)
                        .map_err(parquet_refusal)?;
                }
                Leaf::Boolean => {
                    let values: Vec<bool> = present
                        .iter()
                        .map(|value| value.parse().unwrap_or_default())
                        .collect();
                    typed
                        .typed::<BoolType>()
                        .write_batch(&values, Some(&levels), None)
                        .map_err(parquet_refusal)?;
                }
                Leaf::Text => {
                    let values: Vec<ByteArray> = present
                        .iter()
                        .map(|value| ByteArray::from(value.as_bytes()))
                        .collect();
                    typed
                        .typed::<ByteArrayType>()
                        .write_batch(&values, Some(&levels), None)
                        .map_err(parquet_refusal)?;
                }
            }
            column.close().map_err(parquet_refusal)?;
            index += 1;
        }
        group.close().map_err(parquet_refusal)?;
    }
    writer.into_inner().map_err(parquet_refusal)
}

/// What the columnar writer refusing to write an object reports: the writer's own words,
/// reported as this emulator's doing — the rows are the query's, and what could not be
/// written is this writer's business and not the statement's.
fn parquet_refusal(error: ::parquet::errors::ParquetError) -> Failure {
    Failure::emulator(format!(
        "this emulator could not write the output of this statement as a Parquet object: \
         {error}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sheet of one column of each kind, with a NULL in each.
    fn sheet() -> Sheet<'static> {
        Sheet {
            columns: vec![
                SheetColumn {
                    name: "id",
                    data_type: "integer",
                },
                SheetColumn {
                    name: "size",
                    data_type: "bigint",
                },
                SheetColumn {
                    name: "rate",
                    data_type: "double",
                },
                SheetColumn {
                    name: "live",
                    data_type: "boolean",
                },
                SheetColumn {
                    name: "label",
                    data_type: "varchar",
                },
            ],
            rows: vec![
                vec![
                    Some("1"),
                    Some("9007199254740993"),
                    Some("1.5"),
                    Some("true"),
                    Some("north"),
                ],
                vec![None, None, None, None, None],
            ],
        }
    }

    /// RULE: the objects a statement writes hold the rows it produced, in the format the
    /// statement named — so every format writes every row, and a NULL stays a NULL. The two
    /// text formats are written as the gzip stream their keys name, so what carries the rows
    /// is what that stream decompresses to.
    #[test]
    fn every_format_writes_every_row() {
        use ::std::io::Read;

        // (case, the format, what the bytes must carry)
        let cases: Vec<(&str, OutputFormat, Vec<&str>)> = vec![
            (
                "delimited text writes Hive's own null text",
                OutputFormat::Text,
                vec!["north", "\\N", "9007199254740993"],
            ),
            (
                "JSON writes a number as a number and a NULL as null",
                OutputFormat::Json,
                vec!["\"id\":1", "\"label\":\"north\"", "\"live\":true", "null"],
            ),
        ];
        for (case, format, carried) in &cases {
            let written = format.encode(&sheet()).expect("the object is written");
            assert!(
                written.starts_with(&[0x1f, 0x8b]),
                "case {case:?}: expected a gzip stream, got {} bytes",
                written.len()
            );
            let mut text = String::new();
            ::flate2::read::GzDecoder::new(written.as_slice())
                .read_to_string(&mut text)
                .expect("the object holds the gzip stream its key names");
            for expected in carried {
                assert!(
                    text.contains(expected),
                    "case {case:?}: expected the object to carry {expected:?}, got {text:?}"
                );
            }
        }
        // A Parquet object is not text, so what is asserted of it is that the maintained
        // writer wrote one: a file carrying the format's own magic.
        let parquet = OutputFormat::Parquet
            .encode(&sheet())
            .expect("the object is written");
        assert!(
            parquet.starts_with(b"PAR1") && parquet.ends_with(b"PAR1"),
            "case \"parquet\": expected a Parquet file, got {} bytes",
            parquet.len()
        );
    }

    /// RULE: the rows of one statement's output are grouped by the LAST values of each row,
    /// one partition per distinct value tuple, in first-appearance order.
    #[test]
    fn the_rows_are_grouped_by_their_trailing_partition_values() {
        /// One partition expected: the values that address it, and the rows it holds.
        type Expected<'v> = (Vec<&'v str>, Vec<usize>);

        let rows: Vec<Vec<Option<&str>>> = vec![
            vec![Some("1"), Some("eu")],
            vec![Some("2"), Some("us")],
            vec![Some("3"), Some("eu")],
            vec![Some("4"), None],
        ];
        // (case, how many trailing columns are partition keys, the partitions expected)
        let cases: Vec<(&str, usize, Vec<Expected<'_>>)> = vec![
            (
                "no partition keys at all",
                0,
                vec![(Vec::new(), vec![0, 1, 2, 3])],
            ),
            (
                "one key, with a NULL value among them",
                1,
                vec![
                    (vec!["eu"], vec![0, 2]),
                    (vec!["us"], vec![1]),
                    (vec![HIVE_DEFAULT_PARTITION], vec![3]),
                ],
            ),
            (
                "every column a key",
                2,
                vec![
                    (vec!["1", "eu"], vec![0]),
                    (vec!["2", "us"], vec![1]),
                    (vec!["3", "eu"], vec![2]),
                    (vec!["4", HIVE_DEFAULT_PARTITION], vec![3]),
                ],
            ),
        ];
        for (case, keys, expected) in &cases {
            let actual = partitioned(&rows, *keys).expect("the rows are grouped");
            let reported: Vec<Expected<'_>> = actual
                .iter()
                .map(|partition| {
                    (
                        partition.values.iter().map(String::as_str).collect(),
                        partition.rows.clone(),
                    )
                })
                .collect();
            assert_eq!(
                &reported, expected,
                "case {case:?}: expected the partitions {expected:?}, got {reported:?}"
            );
        }
    }

    /// RULE: a `format` property names one of the formats this emulator writes, and
    /// anything else is reported rather than written as something else.
    #[test]
    fn the_format_property_names_the_format_written() {
        // (the property value, the format it names)
        let cases: Vec<(&str, Option<OutputFormat>)> = vec![
            ("PARQUET", Some(OutputFormat::Parquet)),
            ("parquet", Some(OutputFormat::Parquet)),
            (" JSON ", Some(OutputFormat::Json)),
            ("TEXTFILE", Some(OutputFormat::Text)),
            ("ORC", None),
            ("AVRO", None),
            ("", None),
        ];
        for (value, expected) in &cases {
            let actual = OutputFormat::named(value).ok();
            assert_eq!(
                actual, *expected,
                "case {value:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
