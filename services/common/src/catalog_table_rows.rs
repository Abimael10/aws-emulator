// SPDX-License-Identifier: CC0-1.0

//! The ROWS behind a DATA CATALOG table — the delimited-text objects at its location, read
//! as the rows its SerDe says they hold — and the LOAD of those rows into this workspace's
//! in-process SQL engine ([`crate::sql_wire_planes::sql_engine`]), for every service whose
//! data plane answers a query over such a table.
//!
//! A Data Catalog table says where its data lives (`StorageDescriptor$Location`) and how to
//! read it (`StorageDescriptor$SerdeInfo` plus the table's own properties), so the rows a
//! query over it reads are the BYTES the object store holds, decoded by the SerDe the
//! catalog recorded — never values synthesised from the query or from the table's
//! definition. Two readings are stated once here, because every service that answers such a
//! query has to perform both and two of them must not decode one table differently:
//!
//! * what the objects at a location HOLD ([`text_format`], [`object_rows`],
//!   [`is_hidden_key`]) — the field separator, the quoting, the text that stands for a NULL
//!   and the header lines to skip, all off the table's own parameters, and the records a
//!   maintained CSV reader decodes with them;
//! * how a relation is handed to the ENGINE ([`storage_class`], [`load_relation`]) — the
//!   table the engine holds it in, each column declared with the storage class the type the
//!   catalog records for it reads as, and the rows inserted into it.
//!
//! Each service keeps what its own API does with a refusal (the status, the error shape and
//! the wording) and which columns a caller may read at all; nothing about how a table's
//! bytes decode is decided per service.
//!
//! Services still carrying their own copy of one of these readings: the Athena query
//! engine's (`service-com-amazonaws-athena-amazonathena`, `state::catalog_sql::data` for
//! the text reading and `state::query_engine` for the engine load) and the Entity Resolution
//! matching run's (`service-com-amazonaws-entityresolution-awsveniceservice`,
//! `state::matching_run`). Both PREDATE this module and are to be MIGRATED onto it, each in
//! the change that owns its service — they are left in place only because a change does not
//! own another service's issues, and because Athena's copy carries a great deal more than
//! this module's surface (the columnar and JSON formats, the compression codecs, the
//! metadata columns and the Lake Formation column scope). Nothing NEW should restate either
//! reading — call this module.

use std::collections::HashMap;

use crate::sql_wire_planes::sql_engine::{DatabaseEngine, EngineError, EngineView};

/// The character the fields of a text object are separated by when nothing names one:
/// Hive's own default field separator, the ASCII unit separator.
pub const DEFAULT_FIELD_DELIMITER: char = '\u{1}';

/// The text a field holds when it carries no value, as Hive writes a NULL by default
/// (`serialization.null.format`).
pub const DEFAULT_NULL_TEXT: &str = "\\N";

/// The SerDe parameters that name the character fields are separated by, in the order a
/// reader of such an object reads them: the delimited-text SerDe's own parameter, the
/// quoted-CSV SerDe's, and the serialization format a table that names only that one falls
/// back to.
const DELIMITER_PARAMETERS: [&str; 3] = ["separatorChar", "field.delim", "serialization.format"];

/// The one of [`DELIMITER_PARAMETERS`] whose value Hive writes a character CODE in rather
/// than the character itself: the default field separator is recorded as the code `1`, which
/// is the unit separator and not the digit.
const CODED_PARAMETER: &str = "serialization.format";

/// The SerDe parameter the quoted-CSV SerDe carries its quote character in.
const QUOTE_PARAMETER: &str = "quoteChar";

/// The SerDe parameter that says what text stands for a NULL value.
const NULL_FORMAT_PARAMETER: &str = "serialization.null.format";

/// The table property that says how many lines at the head of each object are a header
/// rather than data, as Hive names it.
const SKIP_HEADER_PROPERTY: &str = "skip.header.line.count";

/// The table property a crawler or a client records the data format of a catalog table in.
const CLASSIFICATION_PROPERTY: &str = "classification";

/// The [`CLASSIFICATION_PROPERTY`] value of a comma-separated-values table, whose fields are
/// separated by a comma rather than by Hive's own default separator.
const CSV_CLASSIFICATION: &str = "csv";

/// How many rows one `INSERT` carries while a relation is loaded into the engine, so the
/// statement a load builds stays well inside the engine's own statement-length limit.
const ROWS_PER_INSERT: usize = 64;

/// How the objects at a catalog table's location decode into rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFormat {
    /// What separates two fields.
    pub delimiter: u8,
    /// What a field may be wrapped in, where the SerDe quotes its fields.
    pub quote: Option<u8>,
    /// The text a field carrying no value holds.
    pub null_text: String,
    /// How many records at the head of each object are a header rather than data.
    pub skip_header_lines: usize,
}

/// How the objects at a table's location decode, off the table's own parameters.
///
/// The SerDe's parameters name the separator and the quoting where the table carries them;
/// a table that names none but records itself as comma-separated data
/// (`classification`) is read with a comma, and anything else with Hive's own default
/// separator.
#[must_use]
pub fn text_format(
    serde_parameters: &HashMap<String, String>,
    table_parameters: &HashMap<String, String>,
) -> TextFormat {
    TextFormat {
        delimiter: delimiter_of(serde_parameters, table_parameters),
        quote: serde_parameters
            .get(QUOTE_PARAMETER)
            .and_then(|quote| quote.chars().next())
            .and_then(character_byte),
        null_text: serde_parameters
            .get(NULL_FORMAT_PARAMETER)
            .cloned()
            .unwrap_or_else(|| DEFAULT_NULL_TEXT.to_string()),
        skip_header_lines: table_parameters
            .get(SKIP_HEADER_PROPERTY)
            .and_then(|lines| lines.parse::<usize>().ok())
            .unwrap_or(0),
    }
}

/// The separator [`text_format`] reads out of the parameters.
fn delimiter_of(
    serde_parameters: &HashMap<String, String>,
    table_parameters: &HashMap<String, String>,
) -> u8 {
    for parameter in DELIMITER_PARAMETERS {
        let Some(value) = serde_parameters
            .get(parameter)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if parameter == CODED_PARAMETER {
            if let Some(byte) = value
                .parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .and_then(character_byte)
            {
                return byte;
            }
        }
        if let Some(byte) = value.chars().next().and_then(character_byte) {
            return byte;
        }
    }
    if table_parameters
        .get(CLASSIFICATION_PROPERTY)
        .is_some_and(|classification| classification.eq_ignore_ascii_case(CSV_CLASSIFICATION))
    {
        return b',';
    }
    character_byte(DEFAULT_FIELD_DELIMITER).unwrap_or(b',')
}

/// One separator or quote character as the reader takes it: a byte, since the reader works
/// over bytes — so a character outside the single-byte range names none.
fn character_byte(character: char) -> Option<u8> {
    u8::try_from(u32::from(character)).ok()
}

/// Whether `key` names an object a reader of a table's location SKIPS: the markers a writer
/// leaves beside the data (`_SUCCESS`, a `.`-prefixed temporary), which Hive and every
/// engine over it leave out of a table's splits.
#[must_use]
pub fn is_hidden_key(key: &str) -> bool {
    key.rsplit('/')
        .next()
        .is_some_and(|name| name.starts_with('_') || name.starts_with('.'))
}

/// Why the rows of one object could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowsRefusal {
    /// The bytes are not the format the table's SerDe declares, so what decoded is a SLICE
    /// of the data rather than the data: a query answered from it would be answered from
    /// part of what the table holds.
    Undecodable,
    /// The object holds more rows than the caller reads, for the same reason.
    TooManyRows,
}

/// The rows one object at a table's location holds: one per record it carries past the
/// header, each with the `picked` fields in the order given, and `None` where a field
/// carries no value.
///
/// `room` bounds how many rows this may answer with — the caller's remaining allowance for
/// the relation — so a reader cannot be made to hold more of an object than it budgeted
/// for.
pub fn object_rows(
    body: &[u8],
    format: &TextFormat,
    picked: &[usize],
    room: usize,
) -> Result<Vec<Vec<Option<String>>>, RowsRefusal> {
    let mut reader = ::csv::ReaderBuilder::new()
        .delimiter(format.delimiter)
        .quote(format.quote.unwrap_or(b'"'))
        .quoting(format.quote.is_some())
        .has_headers(false)
        .flexible(true)
        .from_reader(body);
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    for (line, record) in reader.records().enumerate() {
        if line < format.skip_header_lines {
            continue;
        }
        let record = record.map_err(|_| RowsRefusal::Undecodable)?;
        if rows.len() >= room {
            return Err(RowsRefusal::TooManyRows);
        }
        rows.push(
            picked
                .iter()
                .map(|position| {
                    record
                        .get(*position)
                        .filter(|field| *field != format.null_text)
                        .map(str::to_string)
                })
                .collect(),
        );
    }
    Ok(rows)
}

/// The storage class a column of `data_type` is held in.
///
/// The Data Catalog records a column's type as the Hive type text a client wrote
/// (`com.amazonaws.glue#Column$Type`), so the numeric types are held as numbers and
/// everything else as text — which is what makes a comparison or an aggregation over a
/// numeric column compute over numbers rather than over their digits. The parameters a type
/// carries (`decimal(10,2)`, `varchar(32)`) name a width, not another class.
#[must_use]
pub fn storage_class(data_type: &str) -> &'static str {
    let base = data_type
        .split_once('(')
        .map_or(data_type, |(base, _)| base)
        .trim()
        .to_ascii_lowercase();
    match base.as_str() {
        "tinyint" | "smallint" | "int" | "integer" | "bigint" => "INTEGER",
        "float" | "double" | "double precision" | "real" | "decimal" | "numeric" => "REAL",
        _ => "TEXT",
    }
}

/// Hold `rows` in the engine as the table `name`, with `columns` as `(name, the type the
/// catalog declares for it)`.
///
/// The table is DEFINED and the rows INSERTED through the engine itself, so what a statement
/// over the relation reads is what the engine holds; the engine's own bounds (its statement
/// deadline, its value and statement lengths) are what cap one load.
pub fn load_relation(
    engine: &DatabaseEngine,
    view: &mut EngineView,
    name: &str,
    columns: &[(String, String)],
    rows: &[Vec<Option<String>>],
) -> Result<(), EngineError> {
    let declarations: Vec<String> = columns
        .iter()
        .map(|(column, data_type)| {
            format!("{} {}", quote_identifier(column), storage_class(data_type))
        })
        .collect();
    engine.execute_as(
        view,
        &format!(
            "CREATE TABLE {} ({})",
            quote_identifier(name),
            declarations.join(", ")
        ),
    )?;
    let names: Vec<String> = columns
        .iter()
        .map(|(column, _)| quote_identifier(column))
        .collect();
    for chunk in rows.chunks(ROWS_PER_INSERT) {
        let tuples: Vec<String> = chunk
            .iter()
            .map(|row| {
                let values: Vec<String> =
                    row.iter().map(|value| quote_text(value.as_deref())).collect();
                format!("({})", values.join(", "))
            })
            .collect();
        engine.execute_as(
            view,
            &format!(
                "INSERT INTO {} ({}) VALUES {}",
                quote_identifier(name),
                names.join(", "),
                tuples.join(", ")
            ),
        )?;
    }
    Ok(())
}

/// One identifier as SQL: quoted, with an embedded quote doubled, so a name the catalog
/// holds cannot be read as syntax.
#[must_use]
pub fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// One value as SQL: a quoted literal with an embedded quote doubled, or `NULL` for a field
/// that carries no value. The column's own storage class is what reads a numeric value out
/// of the text the object held.
#[must_use]
pub fn quote_text(value: Option<&str>) -> String {
    match value {
        None => "NULL".to_string(),
        Some(text) => format!("'{}'", text.replace('\'', "''")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parameters of one table, as the catalog holds them.
    fn parameters(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    /// One row of the format table below: the case, the SerDe's parameters, the table's
    /// parameters and the format they decide.
    type FormatRow = (
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static [(&'static str, &'static str)],
        TextFormat,
    );

    /// RULE: how a table's objects decode is read off the table's own parameters — the
    /// SerDe's separator, quote and NULL text, the header count, and the classification a
    /// table that names no separator falls back to.
    ///
    /// The rows span each parameter the readers consult (`separatorChar`, `field.delim`,
    /// `serialization.format` with its character CODE, `quoteChar`,
    /// `serialization.null.format`, `skip.header.line.count`, `classification`) and a table
    /// that carries none at all.
    #[test]
    fn a_tables_parameters_decide_how_its_objects_decode() {
        let rows: &[FormatRow] = &[
            (
                "a table that names nothing",
                &[],
                &[],
                TextFormat {
                    delimiter: 1,
                    quote: None,
                    null_text: DEFAULT_NULL_TEXT.to_string(),
                    skip_header_lines: 0,
                },
            ),
            (
                "comma-separated data, by classification alone",
                &[],
                &[("classification", "CSV")],
                TextFormat {
                    delimiter: b',',
                    quote: None,
                    null_text: DEFAULT_NULL_TEXT.to_string(),
                    skip_header_lines: 0,
                },
            ),
            (
                "the delimited-text SerDe's own separator, with a header line",
                &[("field.delim", "\t")],
                &[("skip.header.line.count", "1")],
                TextFormat {
                    delimiter: b'\t',
                    quote: None,
                    null_text: DEFAULT_NULL_TEXT.to_string(),
                    skip_header_lines: 1,
                },
            ),
            (
                "the quoted-CSV SerDe's separator and quote, and its own NULL text",
                &[
                    ("separatorChar", ";"),
                    ("quoteChar", "'"),
                    ("serialization.null.format", ""),
                ],
                &[],
                TextFormat {
                    delimiter: b';',
                    quote: Some(b'\''),
                    null_text: String::new(),
                    skip_header_lines: 0,
                },
            ),
            (
                "a serialization format carrying the separator's character CODE",
                &[("serialization.format", "124")],
                &[],
                TextFormat {
                    delimiter: b'|',
                    quote: None,
                    null_text: DEFAULT_NULL_TEXT.to_string(),
                    skip_header_lines: 0,
                },
            ),
        ];
        for (case, serde_parameters, table_parameters, expected) in rows {
            let actual = text_format(&parameters(serde_parameters), &parameters(table_parameters));
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the rows of an object are its records past the header, carrying the fields the
    /// caller picked, with the SerDe's NULL text read as no value — and a read is refused
    /// rather than truncated when the bytes do not decode or hold more rows than the caller
    /// reads.
    #[test]
    fn an_objects_rows_are_the_records_it_carries() {
        let format = text_format(
            &parameters(&[("field.delim", ","), ("serialization.null.format", "\\N")]),
            &parameters(&[("skip.header.line.count", "1")]),
        );
        let body = b"id,value,label\n1,10,\\N\n2,20,beta\n".to_vec();
        let rows = object_rows(&body, &format, &[0, 2], 10).expect("the object's rows");
        assert_eq!(
            rows,
            vec![
                vec![Some("1".to_string()), None],
                vec![Some("2".to_string()), Some("beta".to_string())],
            ],
            "the header is skipped, the picked fields are read and the NULL text is no value"
        );
        assert_eq!(
            object_rows(&body, &format, &[0], 1),
            Err(RowsRefusal::TooManyRows),
            "an object holding more rows than the caller reads is refused, not truncated"
        );
        let quoted = text_format(
            &parameters(&[("separatorChar", ","), ("quoteChar", "\"")]),
            &parameters(&[]),
        );
        assert_eq!(
            object_rows(b"\xff\xfe,1\n", &quoted, &[0], 10),
            Err(RowsRefusal::Undecodable),
            "bytes that are not the text the declared format is written in are refused"
        );
    }

    /// RULE: a numeric Hive type is held as a number and everything else as text, and the
    /// width a type carries names no other class.
    #[test]
    fn a_columns_type_decides_the_storage_class_it_is_held_in() {
        let rows: [(&str, &str); 9] = [
            ("int", "INTEGER"),
            ("bigint", "INTEGER"),
            ("tinyint", "INTEGER"),
            ("double", "REAL"),
            ("decimal(10,2)", "REAL"),
            ("string", "TEXT"),
            ("varchar(32)", "TEXT"),
            ("timestamp", "TEXT"),
            ("boolean", "TEXT"),
        ];
        for (data_type, expected) in rows {
            let actual = storage_class(data_type);
            assert_eq!(
                actual, expected,
                "case {data_type:?}: expected {expected}, got {actual}"
            );
        }
    }

    /// RULE: the relation the engine is handed holds the rows that were loaded into it, and
    /// a name or a value carrying a quote is still that name or value rather than syntax.
    #[test]
    fn a_loaded_relation_is_what_the_engine_answers_over() {
        let engine = DatabaseEngine::open().expect("the engine");
        let mut view = EngineView::default();
        let columns = vec![
            ("id".to_string(), "int".to_string()),
            ("the \"label\"".to_string(), "string".to_string()),
        ];
        let rows = vec![
            vec![Some("1".to_string()), Some("o'clock".to_string())],
            vec![Some("2".to_string()), None],
        ];
        load_relation(&engine, &mut view, "rows", &columns, &rows).expect("the load");
        let answered = engine
            .execute_as(
                &mut view,
                "SELECT SUM(\"id\"), COUNT(\"the \"\"label\"\"\") FROM \"rows\"",
            )
            .expect("the answer");
        assert_eq!(
            answered.rows.len(),
            1,
            "one aggregate row, got {:?}",
            answered.rows
        );
        assert_eq!(
            answered.rows[0],
            vec![
                crate::sql_wire_planes::sql_engine::Cell::Integer(3),
                crate::sql_wire_planes::sql_engine::Cell::Integer(1),
            ],
            "the engine sums the numeric column and counts the values that are there, got {:?}",
            answered.rows[0]
        );
    }

    /// RULE: a marker a writer leaves beside the data is not one of a table's splits.
    #[test]
    fn the_markers_beside_a_tables_data_are_skipped() {
        let rows: [(&str, bool); 5] = [
            ("rows/part-00000.csv", false),
            ("rows/_SUCCESS", true),
            ("rows/.hidden.csv", true),
            ("_temporary/0/part.csv", false),
            ("rows/data_1.csv", false),
        ];
        for (key, hidden) in rows {
            assert_eq!(
                is_hidden_key(key),
                hidden,
                "case {key:?}: expected hidden {hidden}"
            );
        }
    }
}
