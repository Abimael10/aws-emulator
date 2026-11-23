// SPDX-License-Identifier: CC0-1.0

//! `ANALYZE COMPRESSION`: the encoding each column of a table would be stored
//! under, and how much space storing it that way would save.
//!
//! The statement "performs compression analysis and produces a report with the
//! suggested compression encoding for the tables analyzed", one record per
//! analysed column, and "for each column, the report includes an estimate of
//! the potential reduction in disk space compared to the RAW encoding". That is
//! what this module computes — from the rows the table really holds, never from
//! the column's name or its declared type alone:
//!
//! * each column's sampled values are READ off the engine, one pass per column,
//!   bounded by the sample size `COMPROWS` names and by [`MAX_ANALYSED_CELLS`];
//! * the size the column takes RAW is the width its values occupy uncompressed,
//!   and the size under each candidate encoding is what that encoding's own
//!   published definition makes of those values — a dictionary and one index per
//!   row for `BYTEDICT`, a count and a value per run for `RUNLENGTH`, a byte per
//!   row for the `DELTA` of a value that fits one;
//! * the encoding reported is the candidate that comes out SMALLEST, with the
//!   reduction it achieves; a column no candidate shrinks is reported as `raw`
//!   with no reduction, which is what an incompressible column is.
//!
//! Only encodings whose size follows from the values themselves are candidates:
//! an encoding whose ratio could only be guessed at (the byte-stream
//! compressors) is not offered, because a reduction this emulator cannot compute
//! is not one it may report.
//!
//! Source for the candidate encodings and what each one stores: the Amazon
//! Redshift *Database Developer Guide*, "Compression encodings".

use std::collections::BTreeSet;

use super::copy_load::{self, TypeClass};
use super::redshift_sql::{self, SchemaSet};
use super::warehouse_statements::AnalyzeCompressionStatement;

/// One record of the report: the column analysed and what it would be stored
/// under.
#[derive(Debug, Clone, PartialEq)]
pub struct Recommendation {
    /// The table the column belongs to, as the catalogue holds it.
    pub table: String,
    /// The column's own name.
    pub column: String,
    /// The encoding recommended for it, in the spelling `ENCODE` takes.
    pub encoding: &'static str,
    /// "An estimate of the potential reduction in disk space compared to the
    /// RAW encoding", as a percentage.
    pub reduction_pct: f64,
}

/// The four columns the report is served as, exactly as Amazon Redshift names
/// them.
pub const REPORT_COLUMNS: [&str; 4] = ["Table", "Column", "Encoding", "Est_reduction_pct"];

/// `RAW`: "the data is stored as raw, uncompressed data" — what a column no
/// candidate encoding shrinks is reported as.
pub const RAW_ENCODING: &str = "raw";

/// `BYTEDICT`: a dictionary of the column's distinct values, with one index per
/// row — "very effective when a column contains a limited number of unique
/// values".
const BYTEDICT_ENCODING: &str = "bytedict";

/// `RUNLENGTH`: "replaces a value that is repeated consecutively with a token
/// that consists of the value and a count of the number of consecutive
/// occurrences".
const RUNLENGTH_ENCODING: &str = "runlength";

/// `DELTA`: "compress data by recording the difference between values that
/// follow each other in the column", one byte per difference that fits one.
const DELTA_ENCODING: &str = "delta";

/// How many distinct values a `BYTEDICT` dictionary holds: "a separate
/// dictionary of unique values is created for each block of column values on
/// disk… the dictionary contains up to 256 one-byte values". A column with more
/// than this many distinct values is not a candidate for it.
const BYTEDICT_VALUES: usize = 256;

/// How long a value kept in that dictionary may be, so the analysis of one
/// column holds a bounded dictionary whatever the column's values are. A column
/// carrying a longer value is not a `BYTEDICT` candidate.
const BYTEDICT_VALUE_BYTES: usize = 256;

/// How many consecutive occurrences one `RUNLENGTH` token counts: the count is
/// one byte, so a longer run is stored as several tokens.
const RUNLENGTH_RUN: u64 = 255;

/// The difference a `DELTA` byte can hold, as "1-byte differences".
const DELTA_RANGE: std::ops::RangeInclusive<i64> = -127..=127;

/// The sample size the analysis reads per column when the statement names none:
/// "COMPROWS… the number of rows to be used as the sample size for compression
/// analysis… Default: 100,000 rows per slice".
const DEFAULT_COMPROWS: u64 = 100_000;

/// How many VALUES one `ANALYZE COMPRESSION` reads in all. The analysis costs
/// one pass per column over the sample, so the work one statement can ask for is
/// bounded here as every other thing a caller can multiply is.
pub const MAX_ANALYSED_CELLS: u64 = 2_000_000;

/// Why an analysis produced no report.
#[derive(Debug)]
pub enum AnalysisFailure {
    /// The engine refused or could not run one of the reads.
    Engine(rusqlite::Error),
    /// The statement names something this analysis cannot resolve, or asks for
    /// more work than one statement may. The message is the one the warehouse
    /// states for it.
    Refused(String),
}

impl From<rusqlite::Error> for AnalysisFailure {
    fn from(error: rusqlite::Error) -> Self {
        Self::Engine(error)
    }
}

/// Analyse what `statement` asks about, and report one record per analysed
/// column.
pub fn analyse(
    connection: &rusqlite::Connection,
    statement: &AnalyzeCompressionStatement,
    schemas: &SchemaSet,
) -> Result<Vec<Recommendation>, AnalysisFailure> {
    // The sample one column is read over is bounded whatever the statement
    // asks for, and the TOTAL read across the columns is bounded below, so
    // neither the rows one column holds nor the number of columns analysed can
    // grow the work past it.
    let sample = statement
        .comprows
        .unwrap_or(DEFAULT_COMPROWS)
        .min(MAX_ANALYSED_CELLS);
    let mut report: Vec<Recommendation> = Vec::new();
    let mut read: u64 = 0;
    for table in analysed_tables(connection, statement, schemas)? {
        let columns = copy_load::target_columns(connection, &table)?;
        let (_schema, name) = super::split_relation(&table);
        for column in selected_columns(&columns, &statement.columns)? {
            let profile = profile(connection, &table, &column, sample)?;
            read = read.saturating_add(profile.rows);
            if read > MAX_ANALYSED_CELLS {
                return Err(AnalysisFailure::Refused(format!(
                    "ERROR: one compression analysis of this emulator reads at most \
                     {MAX_ANALYSED_CELLS} values"
                )));
            }
            let (encoding, reduction_pct) = profile.recommended();
            report.push(Recommendation {
                table: name.to_string(),
                column: column.name.clone(),
                encoding,
                reduction_pct,
            });
        }
    }
    Ok(report)
}

/// The SQL the report is served by: the records as a result set whose columns
/// are the four Amazon Redshift names them.
///
/// Served as a QUERY on the engine, like every other result set this service
/// reports, so the report carries the column metadata any other query's result
/// does.
pub fn report_sql(report: &[Recommendation]) -> String {
    let [table, column, encoding, reduction] = REPORT_COLUMNS;
    if report.is_empty() {
        // Nothing was analysed, which is a result set of no rows rather than an
        // error.
        return format!(
            "SELECT NULL AS \"{table}\", NULL AS \"{column}\", NULL AS \"{encoding}\", \
             NULL AS \"{reduction}\" WHERE 0"
        );
    }
    let values: Vec<String> = report
        .iter()
        .map(|record| {
            format!(
                "('{}', '{}', '{}', {:.2})",
                quoted(&record.table),
                quoted(&record.column),
                record.encoding,
                record.reduction_pct
            )
        })
        .collect();
    format!(
        "WITH analysis(\"{table}\", \"{column}\", \"{encoding}\", \"{reduction}\") AS \
         (VALUES {}) SELECT \"{table}\", \"{column}\", \"{encoding}\", \"{reduction}\" \
         FROM analysis",
        values.join(", ")
    )
}

/// One text as a SQL string literal's contents.
fn quoted(value: &str) -> String {
    value.replace('\'', "''")
}

/// The ENGINE names of the tables the statement analyses: the one it names, or
/// every table the database holds when it names none ("ANALYZE COMPRESSION…
/// \[table_name\]").
fn analysed_tables(
    connection: &rusqlite::Connection,
    statement: &AnalyzeCompressionStatement,
    schemas: &SchemaSet,
) -> Result<Vec<String>, AnalysisFailure> {
    if statement.table.is_empty() {
        let mut listing = connection.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
             ORDER BY name",
        )?;
        let held = listing
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        return Ok(held);
    }
    let engine_name = redshift_sql::engine_table_name(&statement.table, schemas);
    let quoted_name = engine_name.replace('"', "\"\"");
    // The table has to be there to be analysed, and a relation that is not is
    // reported as the warehouse reports one.
    if connection
        .prepare(&format!("SELECT * FROM \"{quoted_name}\" LIMIT 0"))
        .is_err()
    {
        return Err(AnalysisFailure::Refused(format!(
            "ERROR: relation \"{}\" does not exist",
            statement.table.join(".")
        )));
    }
    Ok(vec![engine_name])
}

/// The columns the analysis reports on: the ones the statement names, or every
/// column of the table when it names none.
fn selected_columns(
    columns: &[copy_load::Column],
    named: &[String],
) -> Result<Vec<copy_load::Column>, AnalysisFailure> {
    if named.is_empty() {
        return Ok(columns.to_vec());
    }
    named
        .iter()
        .map(|name| {
            columns
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(name))
                .cloned()
                .ok_or_else(|| {
                    AnalysisFailure::Refused(format!("ERROR: column \"{name}\" does not exist"))
                })
        })
        .collect()
}

/// What one column's sampled values are like, accumulated in ONE pass over
/// them: what they take raw, and what each candidate encoding would take.
#[derive(Debug)]
struct Profile {
    /// How many values were read.
    rows: u64,
    /// What they take with no encoding at all.
    raw_bytes: u64,
    /// The distinct values read so far, while they still fit a block
    /// dictionary; `None` once they cannot, which is what rules `BYTEDICT` out.
    distinct: Option<BTreeSet<Option<Vec<u8>>>>,
    /// What a dictionary of those distinct values holds.
    dictionary_bytes: u64,
    /// What the runs read so far take as `RUNLENGTH` tokens.
    runlength_bytes: u64,
    /// The value the current run is of, how long that run is so far, and what
    /// one occurrence of it takes.
    run: Option<Run>,
    /// What the differences read so far take as `DELTA`.
    delta_bytes: u64,
    /// Whether every value read is an integer, which is what `DELTA` records a
    /// difference between.
    delta_applies: bool,
    /// The last integer read, which the next difference is from.
    previous: Option<i64>,
    /// The width one value of this column takes raw, for the types that store a
    /// fixed one.
    width: u64,
    /// Whether the column's values are variable-width, so what they take raw is
    /// the values' own lengths.
    variable: bool,
}

/// One run of equal values, as `RUNLENGTH` tokenises it.
#[derive(Debug)]
struct Run {
    /// The value the run is of.
    value: Option<Vec<u8>>,
    /// How many consecutive occurrences of it have been read.
    length: u64,
    /// What one occurrence takes stored.
    width: u64,
}

impl Profile {
    /// A profile of a column declared with this type, before any value is read.
    fn new(declared: Option<&str>) -> Self {
        let declared = declared.unwrap_or_default();
        let class = TypeClass::of(declared);
        Self {
            rows: 0,
            raw_bytes: 0,
            distinct: Some(BTreeSet::new()),
            dictionary_bytes: 0,
            runlength_bytes: 0,
            run: None,
            delta_bytes: 0,
            delta_applies: true,
            previous: None,
            width: stored_width(declared, class),
            variable: class == TypeClass::Character,
        }
    }

    /// Read one value into the profile.
    fn observe(&mut self, value: rusqlite::types::ValueRef<'_>) {
        self.rows = self.rows.saturating_add(1);
        let key = key(value);
        let width = match (&key, self.variable) {
            // A NULL carries no value to store.
            (None, _) => 0,
            (Some(bytes), true) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            (Some(_), false) => self.width,
        };
        self.raw_bytes = self.raw_bytes.saturating_add(width);
        self.observe_dictionary(&key, width);
        self.observe_run(key, width);
        self.observe_delta(value);
    }

    /// Count the value towards a `BYTEDICT` dictionary of the column.
    fn observe_dictionary(&mut self, key: &Option<Vec<u8>>, width: u64) {
        let Some(distinct) = self.distinct.as_mut() else {
            return;
        };
        // A value a block dictionary cannot hold rules the encoding out for the
        // column, and the dictionary built so far is dropped with it.
        if key.as_ref().is_some_and(|bytes| bytes.len() > BYTEDICT_VALUE_BYTES) {
            self.distinct = None;
            self.dictionary_bytes = 0;
            return;
        }
        if distinct.contains(key) {
            return;
        }
        if distinct.len() >= BYTEDICT_VALUES {
            self.distinct = None;
            self.dictionary_bytes = 0;
            return;
        }
        distinct.insert(key.clone());
        self.dictionary_bytes = self.dictionary_bytes.saturating_add(width);
    }

    /// Count the value towards the column's `RUNLENGTH` tokens.
    fn observe_run(&mut self, key: Option<Vec<u8>>, width: u64) {
        match self.run.as_mut() {
            Some(run) if run.value == key => run.length += 1,
            _ => {
                self.close_run();
                self.run = Some(Run {
                    value: key,
                    length: 1,
                    width,
                });
                // One token's own value and its count byte, charged when the
                // run opens.
                self.runlength_bytes = self.runlength_bytes.saturating_add(width + 1);
            }
        }
    }

    /// Close the run being counted, charging a further token for every
    /// [`RUNLENGTH_RUN`] occurrences past the first token's.
    fn close_run(&mut self) {
        let Some(run) = self.run.take() else {
            return;
        };
        let tokens = run.length.div_ceil(RUNLENGTH_RUN).saturating_sub(1);
        self.runlength_bytes = self
            .runlength_bytes
            .saturating_add(tokens.saturating_mul(run.width + 1));
    }

    /// Count the value towards the column's `DELTA` differences.
    fn observe_delta(&mut self, value: rusqlite::types::ValueRef<'_>) {
        if !self.delta_applies {
            return;
        }
        let rusqlite::types::ValueRef::Integer(current) = value else {
            // The encoding records a difference between NUMBERS: a column
            // holding anything else is not a candidate for it.
            self.delta_applies = false;
            self.delta_bytes = 0;
            return;
        };
        let difference = self
            .previous
            .map(|previous| current.saturating_sub(previous));
        let stored = match difference {
            // "If the difference is… too large to store, the full value is
            // stored with a leading byte set to 0".
            Some(difference) if DELTA_RANGE.contains(&difference) => 1,
            _ => self.width.saturating_add(1),
        };
        self.delta_bytes = self.delta_bytes.saturating_add(stored);
        self.previous = Some(current);
    }

    /// The encoding to recommend for the column, and the reduction it achieves
    /// against the raw size.
    fn recommended(mut self) -> (&'static str, f64) {
        self.close_run();
        if self.raw_bytes == 0 {
            // Nothing is stored, so nothing can be saved: the column is
            // reported as it stands.
            return (RAW_ENCODING, 0.0);
        }
        let mut candidates: Vec<(&'static str, u64)> =
            vec![(RUNLENGTH_ENCODING, self.runlength_bytes)];
        if self.distinct.is_some() {
            // One index per row beside the dictionary itself.
            candidates.push((
                BYTEDICT_ENCODING,
                self.dictionary_bytes.saturating_add(self.rows),
            ));
        }
        if self.delta_applies {
            candidates.push((DELTA_ENCODING, self.delta_bytes));
        }
        let Some((encoding, encoded)) = candidates
            .into_iter()
            .filter(|(_encoding, encoded)| *encoded < self.raw_bytes)
            .min_by_key(|(_encoding, encoded)| *encoded)
        else {
            return (RAW_ENCODING, 0.0);
        };
        let saved = self.raw_bytes.saturating_sub(encoded) as f64 * 100.0 / self.raw_bytes as f64;
        // Reported to the two decimals the column carries.
        (encoding, (saved * 100.0).round() / 100.0)
    }
}

/// Read one column's sample and profile it.
fn profile(
    connection: &rusqlite::Connection,
    table: &str,
    column: &copy_load::Column,
    sample: u64,
) -> rusqlite::Result<Profile> {
    let quoted_table = table.replace('"', "\"\"");
    let quoted_column = column.name.replace('"', "\"\"");
    let mut prepared = connection.prepare(&format!(
        "SELECT \"{quoted_column}\" FROM \"{quoted_table}\" LIMIT {sample}"
    ))?;
    let mut rows = prepared.query([])?;
    let mut profile = Profile::new(column.declared.as_deref());
    while let Some(row) = rows.next()? {
        profile.observe(row.get_ref(0)?);
    }
    Ok(profile)
}

/// The bytes one value of a column declared with this type takes with no
/// encoding.
///
/// Source: the Amazon Redshift *Data types* reference, which states the storage
/// of each type — 2, 4 and 8 bytes for the integer family, 4 for `REAL` and 8
/// for `DOUBLE PRECISION`, 8 or 16 for `DECIMAL` by its precision, 1 for
/// `BOOLEAN`, 4 for `DATE` and 8 for the timestamp types. A character value is
/// variable-width, so what it takes is its own length.
fn stored_width(declared: &str, class: TypeClass) -> u64 {
    match class {
        TypeClass::Integer => match copy_load::type_head(declared).as_str() {
            "smallint" | "int2" => 2,
            "bigint" | "int8" => 8,
            _ => 4,
        },
        // "DECIMAL… 128-bit signed integers with up to 38 digits of precision",
        // held in 8 bytes while the precision fits a 64-bit integer.
        TypeClass::Decimal => {
            if declared_precision(declared).is_some_and(|precision| precision > 18) {
                16
            } else {
                8
            }
        }
        TypeClass::Float => match copy_load::type_head(declared).as_str() {
            "real" | "float4" => 4,
            _ => 8,
        },
        TypeClass::Boolean => 1,
        TypeClass::Date => 4,
        TypeClass::Timestamp => 8,
        // A character column's values are their own width; the fixed width
        // declared for a `CHAR(n)` is what one of its values takes when it is
        // read as one.
        TypeClass::Character => copy_load::declared_length(declared)
            .and_then(|length| u64::try_from(length).ok())
            .unwrap_or(0),
    }
}

/// The precision a numeric type declares — `DECIMAL(38,2)` has 38 — or `None`
/// for one that declares none.
fn declared_precision(declared: &str) -> Option<usize> {
    let (_, parameters) = declared.split_once('(')?;
    let (parameters, _) = parameters.split_once(')')?;
    let precision = parameters.split(',').next()?;
    precision.trim().parse().ok()
}

/// The bytes one value is identified by, for deciding whether two values are the
/// same one: a NULL is not a value at all, and every other value is identified
/// by its own bytes.
fn key(value: rusqlite::types::ValueRef<'_>) -> Option<Vec<u8>> {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(number) => Some(number.to_string().into_bytes()),
        ValueRef::Real(number) => Some(number.to_string().into_bytes()),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => Some(bytes.to_vec()),
    }
}
