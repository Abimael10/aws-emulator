// SPDX-License-Identifier: CC0-1.0

//! Where a query execution's RESULTS LAND: the objects a completed run materialises under
//! the output location its submission resolved.
//!
//! Athena's work on a query is not over when the engine has reported. Athena "stores query
//! results and metadata information for each query that runs in a query result location
//! that you can specify", and keys what it saved by the execution's own id —
//! `<query-result-location>/<QueryExecutionId>.csv` for a statement whose results are rows,
//! with its metadata object beside it — which is the same object
//! `GetQueryExecution` names in `ResultConfiguration.OutputLocation` — the record forms
//! that key in ONE place, so the object a read NAMES and the object a run WRITES can never
//! be two different keys.
//!
//! So the call that ran the statement writes what the run produced into the one object
//! store this emulator has ([`s3_state`]), and a client that reads its results off that
//! store — what the console, the drivers and most pipelines do, rather than paging
//! `GetQueryResults` — finds the object the API told it about.
//!
//! The DESTINATION is the location the submission resolved (the workgroup's, wherever its
//! configuration overrides the request's), and the BODY is the result set the run stored:
//! nothing here re-runs a statement, and nothing renders a value the record does not hold.
//! A location naming a bucket the object store does not hold is a write this service cannot
//! perform; it is logged and the execution is left exactly as the engine reported it, because
//! nothing about the destination is this service's own state to invent.
//!
//! HOW the objects are stored is the resolved configuration's own as well: its
//! `EncryptionConfiguration` "indicates the encryption option used […] and key information"
//! for the results, so each object of a run is written under the server-side encryption that
//! option names, with the key it names resolved against KMS ([`resolved_encryption`]) \u2014 which
//! is what a later read of a result object reports back.
//!
//! WHEN: the objects are saved by the call that ran the statement, as soon as the run has
//! reported — ahead of the instant the record's completion falls due, which is what makes the
//! execution terminal. So the saved results are already there when the query first reports a
//! terminal state, which is the order every client depends on; the alternative, saving them
//! when some later call first observes the completion, would leave a query nobody polls with
//! no results at all.
//!
//! The results object is written here rather than through a CSV writer crate because of the
//! ONE rule such a writer cannot express: a quote style is per writer, and these records
//! need it per FIELD — every present value quoted, and a NULL written as an unquoted empty
//! field, which is the only thing that tells a NULL from the empty string in the saved text.
//! Reading delimited text, where the records come from a caller's objects, is the maintained
//! reader's job and stays there ([`super::catalog_sql::data`]).
//!
//! That same rendering is what MEASURES how many bytes a run returned
//! ([`returned_bytes`]), so the size a read of the run's statistics reports is the size of
//! what the run actually saved rather than a second estimate of it.

use ::service_com_amazonaws_s3_amazons3::state as s3_state;

use super::catalog_sql::data::parse_s3_uri;
use super::query_engine::{Outcome, StoredResultSet};
use super::result_encryption::{ENCRYPTION_OPTION_SSE_KMS, ENCRYPTION_OPTION_SSE_S3};
use super::QueryExecutionRecord;
use crate::ops::types::{ColumnInfo, EncryptionConfiguration, Row};

/// The suffix of the METADATA object written beside a results object: Athena saves a
/// query's results and "metadata information" together, keyed
/// `<QueryExecutionId>.csv.metadata` at the same prefix as the results object itself.
pub const RESULT_METADATA_SUFFIX: &str = ".metadata";

/// The media type of the results object of a statement whose results are ROWS: Athena
/// saves those results as CSV.
const CSV_CONTENT_TYPE: &str = "text/csv";

/// The media type of the results object of a statement whose result is the engine's TEXT
/// output.
const TEXT_CONTENT_TYPE: &str = "text/plain";

/// The media type of the metadata object: the schema of the result set beside it, as the
/// document [`results_metadata`] renders.
const METADATA_CONTENT_TYPE: &str = "application/json";

/// How a NULL value is written into a results object: an EMPTY, unquoted field. Every
/// value that is present is quoted ([`csv_field`]), so a reader of the saved results tells
/// a NULL apart from the empty string by the quoting, which is the only thing that
/// distinguishes them in a text format.
const CSV_NULL_FIELD: &str = "";

/// The field separator of a results object, and the character a quoted field is wrapped
/// in. CSV, as [`CSV_CONTENT_TYPE`] says.
const CSV_SEPARATOR: char = ',';
/// See [`CSV_SEPARATOR`].
const CSV_QUOTE: char = '"';

/// What separates the fields of one row of a TEXT-output object: the output of a statement
/// Athena classifies as DDL or UTILITY is text rather than a delimited record, so its
/// values are written as the engine reported them, one row per line.
const TEXT_SEPARATOR: char = '\t';

/// One object a completed run materialises: where it goes, and what it holds.
#[derive(Debug, Clone)]
pub struct ResultObject {
    /// The `s3://` URI of the object, which for the results object itself is the one
    /// `ResultConfiguration.OutputLocation` reports once the query is terminal.
    pub uri: String,
    /// The bytes the object holds.
    pub body: Vec<u8>,
    /// Its media type.
    pub content_type: &'static str,
}

/// Every object the completed execution `record` materialises, in the order they are
/// written, or none at all for an execution that materialises nothing.
///
/// A run REPORTED the statement done is what saves results: a statement whose results are
/// rows saves them as the CSV its result object holds, with its metadata object beside it;
/// one whose result is the engine's text output saves that text. A run that reported a
/// failure, one a caller cancelled and one whose text parses as no statement at all
/// produced no results to save, and a submission that resolved no output location never
/// got this far (`StartQueryExecution` refuses it).
pub fn result_objects(record: &QueryExecutionRecord) -> Vec<ResultObject> {
    let Some(Outcome::Succeeded { result, .. }) = record.outcome.as_ref() else {
        return Vec::new();
    };
    // The DATA MANIFEST of a statement that WROTE objects, which goes beside the results
    // object: it lists exactly the objects that statement wrote, so a client reads back the
    // data it produced from the manifest the execution names.
    let manifest = record
        .data_manifest_uri()
        .map(|uri| ResultObject {
            uri,
            body: data_manifest(record.written_objects()),
            content_type: MANIFEST_CONTENT_TYPE,
        })
        .into_iter();
    let Some(uri) = record.result_object_uri() else {
        return manifest.collect();
    };
    if record.writes_text_output() {
        return ::std::iter::once(ResultObject {
            uri,
            body: text_output(result),
            content_type: TEXT_CONTENT_TYPE,
        })
        .chain(manifest)
        .collect();
    }
    vec![
        ResultObject {
            body: results_csv(result),
            content_type: CSV_CONTENT_TYPE,
            uri: uri.clone(),
        },
        ResultObject {
            uri: format!("{uri}{RESULT_METADATA_SUFFIX}"),
            body: results_metadata(result),
            content_type: METADATA_CONTENT_TYPE,
        },
    ]
    .into_iter()
    .chain(manifest)
    .collect()
}

/// The media type of a data manifest object: the one Athena's own manifest key names
/// ([`crate::state::DATA_MANIFEST_SUFFIX`]).
const MANIFEST_CONTENT_TYPE: &str = "text/csv";

/// The data manifest of a statement that wrote objects: the `s3://` URI of each object it
/// wrote, one per line and in the order they were written, each line terminated — so a
/// reader of the manifest gets exactly the objects the statement wrote and nothing else.
fn data_manifest(written: &[String]) -> Vec<u8> {
    let mut text = String::new();
    for object in written {
        text.push_str(object);
        text.push('\n');
    }
    text.into_bytes()
}

/// The server-side encryption the objects of one query's results are stored under: the
/// configuration the write RECORDS on each of them, or `None` when the resolved result
/// configuration asks the object store for no server-side encryption at all — the case the
/// destination bucket's own default encryption configuration decides, exactly as it does for
/// a client's own write that names none.
pub type ResultEncryption = Option<s3_state::ObjectEncryption>;

/// Which server-side encryption the resolved result `configuration` asks the object store
/// for, before the key it names is resolved.
///
/// Two of the three options are SERVER-side: `SSE_S3` is "Amazon S3 server-side encryption
/// with Amazon S3-managed keys", which is the object store's own
/// `ServerSideEncryption` value for that encryption
/// ([`s3_state::DEFAULT_SSE_ALGORITHM`]), and `SSE_KMS` is "server-side encryption with
/// KMS-managed keys" under the key the configuration names — "for `SSE_KMS` and `CSE_KMS`,
/// this is the KMS key ARN or ID" (`EncryptionConfiguration$KmsKey`), so the reference is
/// carried as given and resolved by [`resolved_encryption`].
///
/// `CSE_KMS` is CLIENT-side encryption, so the write itself asks the object store for no
/// server-side encryption of its own — which is what a configuration naming no encryption at
/// all asks for too, and the case the destination bucket's default encryption decides.
fn requested_encryption(configuration: Option<&EncryptionConfiguration>) -> ResultEncryption {
    let configuration = configuration?;
    // Neither option supplies an encryption context, an S3 Bucket Key or a customer-provided
    // key: a result configuration names an option and a key, and nothing else about the
    // encryption.
    match configuration.encryption_option.as_str() {
        // The S3-managed encryption is performed under a key of Amazon S3's own, which is
        // reported nowhere, so this write names the algorithm alone.
        ENCRYPTION_OPTION_SSE_S3 => Some(s3_state::ObjectEncryption {
            sse_algorithm: Some(s3_state::DEFAULT_SSE_ALGORITHM.to_string()),
            ..s3_state::ObjectEncryption::default()
        }),
        ENCRYPTION_OPTION_SSE_KMS => Some(s3_state::ObjectEncryption {
            sse_algorithm: Some(s3_state::KMS_SSE_ALGORITHMS[0].to_string()),
            // The reference as the configuration wrote it, which [`resolved_encryption`]
            // resolves to the key itself; a configuration naming none leaves the key to the
            // object store's own rule for a write that names none.
            kms_key_id: configuration
                .kms_key
                .as_deref()
                .map(str::trim)
                .filter(|key| !key.is_empty())
                .map(str::to_string),
            ..s3_state::ObjectEncryption::default()
        }),
        _ => None,
    }
}

/// The encryption one query's result objects are written under, with the key the resolved
/// result `configuration` names RESOLVED against KMS, or the sentence KMS refused that key
/// with.
///
/// Resolved through the object store's own resolution of a write's SSE-KMS key
/// ([`s3_state::sse_kms::write_key`]), so the value a read of a result object reports is the
/// key itself whichever of its three spellings the configuration named — "the KMS key ARN or
/// ID" (`EncryptionConfiguration$KmsKey`) — and a reference KMS mints no data key for
/// encrypts nothing, which is a write this service cannot make at all.
///
/// `region` is the Region the submission was signed for, which is where the key is resolved:
/// a KMS key is a regional resource, and the results and the key they are encrypted under
/// live at the same endpoint.
///
/// LOCKING: this takes the KMS store's lock and no other, and no store of this service is
/// held while it asks.
pub fn resolved_encryption<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    configuration: Option<&EncryptionConfiguration>,
) -> Result<ResultEncryption, String> {
    let Some(requested) = requested_encryption(configuration) else {
        return Ok(None);
    };
    match s3_state::sse_kms::write_key(engine, region, &requested) {
        // Nothing was asked of KMS — the encryption is not the KMS one, or the reference is
        // one KMS declines before resolving anything — so the write records what the
        // configuration named, unresolved.
        s3_state::sse_kms::WriteKey::Unasked => Ok(Some(requested)),
        // A reference KMS does resolve names ONE key whatever form it took, so what the
        // objects record is that key's ARN.
        s3_state::sse_kms::WriteKey::Resolved(key) => Ok(Some(s3_state::ObjectEncryption {
            kms_key_id: Some(key),
            ..requested
        })),
        // KMS mints no data key under that reference, so there is nothing to encrypt the
        // results with: the write cannot be made, in KMS's own words for the reference.
        s3_state::sse_kms::WriteKey::Refused(relay) => Err(relay.message),
    }
}

/// Save `objects` into the object store on `engine`, each under `encryption`.
///
/// Each one is stored at the key its URI names, in the bucket its URI names, through the
/// one way an object is recorded there — so a saved result counts against the object
/// store's own budgets and is read back by that store's own operations, exactly like an
/// object a client wrote itself.
///
/// `encryption` is the server-side encryption the execution's resolved result configuration
/// settled on ([`resolved_encryption`]), which EVERY object of that execution is stored
/// under: the results and the metadata information beside them are one query's results, so
/// a read of either reports the encryption that query's configuration asked for.
pub fn write<Eng: Send + Sync + 'static>(
    engine: &Eng,
    objects: Vec<ResultObject>,
    encryption: &ResultEncryption,
) {
    if objects.is_empty() {
        return;
    }
    let Some(state) = s3_state::service_state(engine) else {
        ::tracing::warn!(
            "no object store is reachable from this engine, so a query saved no results"
        );
        return;
    };
    for object in objects {
        // The location is an Amazon S3 URI naming an object: a bucket and the key under
        // it. Anything else names nowhere this service can save results to.
        let Some((bucket, key)) = parse_s3_uri(&object.uri).filter(|(_, key)| !key.is_empty())
        else {
            ::tracing::warn!(
                location = %object.uri,
                "a query's results location names no object, so its results were not saved"
            );
            continue;
        };
        let written = store_object(&state, &bucket, &key, object, encryption);
        match written {
            Some(s3_state::ObjectWrite::Created | s3_state::ObjectWrite::Replaced) => {}
            // The object store holds no such bucket, or refused the write for a condition
            // of its own (a key, or a budget of the destination): the results were not
            // saved, and this service invents nothing about a destination it does not own.
            outcome => ::tracing::warn!(
                bucket = %bucket,
                key = %key,
                ?outcome,
                "the results location did not take a query's results"
            ),
        }
    }
}

/// Store ONE object of an execution at `key` of `bucket`, and answer what the object store
/// made of the write.
///
/// The one place this service writes an object of a query: the results, the metadata
/// information and the data manifest beside them, and the DATA a statement that writes its
/// output stores ([`super::catalog_sql::query_output`]). So every object an execution leaves
/// in the store is written under one rule — the encryption the execution's resolved result
/// configuration settled on, the destination bucket's own default encryption where it asks
/// for none, and the entity tag the object store gives an object stored that way — and the
/// callers differ only in what they make of a write the store would not take.
pub(crate) fn store_object(
    state: &::engine::ServiceState<s3_state::ComAmazonawsS3Amazons3State>,
    bucket: &str,
    key: &str,
    object: ResultObject,
    encryption: &ResultEncryption,
) -> Option<s3_state::ObjectWrite> {
    let mut store = state.lock();
    // HOW the object is encrypted at rest: the configuration this execution's result
    // configuration settled on, and the destination bucket's own default encryption
    // configuration where it asks for none — the object store's own rule for every write
    // another service makes into a bucket, so an object of a query and a client's own object
    // in that bucket are stored under one record of it.
    let encryption = match encryption {
        Some(encryption) => encryption.clone(),
        None => s3_state::written_object_encryption(&store, bucket, None),
    };
    let mut stored = s3_state::Object::stored(object.body, Some(object.content_type.to_string()));
    // An object sealed under a key carries an entity tag that is NOT the digest of its
    // bytes, which is the object store's own rule for the tag a stored object takes
    // ([`s3_state::stored_object_etag`]).
    stored.etag = s3_state::stored_object_etag(&stored.etag, &encryption);
    stored.encryption = encryption;
    store.put_object(bucket, key, stored)
}

/// The CSV a statement whose results are ROWS saves them as: the column labels first and
/// the data rows under them, which is the same rendering a read of those results pages
/// (`GetQueryResults` puts the label row in front of a DML query's rows precisely because
/// that is the first line of the saved data file).
///
/// A result set with no columns at all — a write, which returns no rows — saves an empty
/// object: there is a results object, and nothing in it.
fn results_csv(result: &StoredResultSet) -> Vec<u8> {
    let mut text = String::new();
    if !result.column_info.is_empty() {
        push_csv_row(
            &mut text,
            result.column_info.iter().map(|column| Some(label_of(column))),
        );
    }
    push_csv_rows(&mut text, result);
    text.into_bytes()
}

/// Just the DATA rows of a result set, rendered as the results object writes them —
/// everything [`results_csv`] saves except the label line in front of them.
fn data_rows_csv(result: &StoredResultSet) -> Vec<u8> {
    let mut text = String::new();
    push_csv_rows(&mut text, result);
    text.into_bytes()
}

/// Append every data row of `result` to `text`, one CSV record each.
fn push_csv_rows(text: &mut String, result: &StoredResultSet) {
    for row in &result.rows {
        push_csv_row(text, values_of(row));
    }
}

/// How many BYTES the rows `record`'s run RETURNED hold, as the results it saved write
/// them: the rendering of the data rows, without the column-label line a results object
/// puts in front of them — that line is the result set's schema rather than a row the
/// query returned.
///
/// Zero for a run that returned nothing: one that failed, one a caller cancelled and one
/// that has not reported wrote no rows to measure.
pub fn returned_bytes(record: &QueryExecutionRecord) -> i64 {
    let Some(Outcome::Succeeded { result, .. }) = record.outcome.as_ref() else {
        return 0;
    };
    // Rendered exactly as the object the run saved renders them, so the bytes a read
    // reports and the bytes a client fetches are one measure of one result set.
    let rendered = if record.writes_text_output() {
        text_output(result)
    } else {
        data_rows_csv(result)
    };
    i64::try_from(rendered.len()).unwrap_or(i64::MAX)
}

/// The text a statement whose result is the engine's TEXT output saves: the LINES of that
/// output, with the last one terminated as well, because every line a line-oriented text
/// object holds ends with its separator. A statement that produced no rows produced no
/// lines, so it saves an empty object rather than a lone separator.
fn text_output(result: &StoredResultSet) -> Vec<u8> {
    let lines = text_output_lines(result);
    if lines.is_empty() {
        return Vec::new();
    }
    format!("{lines}\n").into_bytes()
}

/// The LINES of that same output, joined by the separator between them: one line per row of
/// what the run produced, with a row's values separated by [`TEXT_SEPARATOR`] and a NULL
/// written as nothing.
///
/// This is the rendering a READ of the execution's results reports as the statement's result
/// output, and the one the saved object is written from — so the text a client reads over the
/// API and the object the run saved can never be two different renderings of one run.
pub(crate) fn text_output_lines(result: &StoredResultSet) -> String {
    let mut text = String::new();
    for (position, row) in result.rows.iter().enumerate() {
        if position > 0 {
            text.push('\n');
        }
        for (column, value) in values_of(row).enumerate() {
            if column > 0 {
                text.push(TEXT_SEPARATOR);
            }
            text.push_str(value.unwrap_or_default());
        }
    }
    text
}

/// The metadata object's document: the SCHEMA of the result set saved beside it, one entry
/// per column in projection order, carrying exactly what the result set's own
/// `ColumnInfo` holds.
///
/// Athena's own metadata object is an opaque binary encoding of that schema, which no
/// published format describes, so this emulator saves the same schema in a form a reader
/// can decode. Nothing is added to it: every member comes off the stored result set.
fn results_metadata(result: &StoredResultSet) -> Vec<u8> {
    let columns: Vec<::serde_json::Value> = result
        .column_info
        .iter()
        .map(|column| {
            ::serde_json::json!({
                "CatalogName": column.catalog_name,
                "SchemaName": column.schema_name,
                "TableName": column.table_name,
                "Name": column.name,
                "Label": column.label,
                "Type": column.r#type,
                "Precision": column.precision,
                "Scale": column.scale,
                "Nullable": column.nullable,
                "CaseSensitive": column.case_sensitive,
            })
        })
        .collect();
    let document = ::serde_json::json!({ "ColumnInfo": columns });
    // The document is built from owned values only, so serialising it cannot fail; an
    // empty object is what a failure would leave rather than a panic in a query's path.
    ::serde_json::to_vec(&document).unwrap_or_default()
}

/// The label a column is written under, which is what a read of the results reports for it
/// too: its label, and its name where it carries none.
fn label_of(column: &ColumnInfo) -> &str {
    column.label.as_deref().unwrap_or(column.name.as_str())
}

/// One stored row's values, in column order: `None` for a NULL.
fn values_of(row: &Row) -> impl Iterator<Item = Option<&str>> + '_ {
    row.data
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|datum| datum.var_char_value.as_deref())
}

/// Append one CSV record for `values`, ending it with a line feed.
fn push_csv_row<'v>(text: &mut String, values: impl Iterator<Item = Option<&'v str>>) {
    for (position, value) in values.enumerate() {
        if position > 0 {
            text.push(CSV_SEPARATOR);
        }
        text.push_str(&csv_field(value));
    }
    text.push('\n');
}

/// One value as the field a results object holds it as: a present value QUOTED, with every
/// quote character inside it doubled so the field reads back as the value itself, and a
/// NULL as [`CSV_NULL_FIELD`].
fn csv_field(value: Option<&str>) -> String {
    match value {
        None => CSV_NULL_FIELD.to_string(),
        // The escape of the quote character is the character itself, twice, so both come
        // from [`CSV_QUOTE`] rather than from a literal beside it.
        Some(value) => {
            let doubled = value.replace(CSV_QUOTE, &format!("{CSV_QUOTE}{CSV_QUOTE}"));
            format!("{CSV_QUOTE}{doubled}{CSV_QUOTE}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::Datum;
    use crate::state::query_engine::hive_string_column;

    /// One stored row of `values`, a `None` standing for a NULL.
    fn row(values: &[Option<&str>]) -> Row {
        Row {
            data: Some(
                values
                    .iter()
                    .map(|value| Datum {
                        var_char_value: value.map(str::to_string),
                    })
                    .collect(),
            ),
        }
    }

    /// A results object holds the column labels and then the data rows, every present
    /// value quoted and every NULL written as an empty field — so the quoting is what
    /// tells a NULL from the empty string.
    #[test]
    fn a_results_object_quotes_every_present_value_and_leaves_a_null_empty() {
        // (case, the stored result set, the CSV it is saved as). The rows differ along
        // what the values are: plain text, a value carrying the separator, a value
        // carrying the quote character, a NULL beside the empty string, no rows at all,
        // and no columns at all.
        let cases: Vec<(&str, StoredResultSet, &str)> = vec![
            (
                "one column of plain values",
                StoredResultSet {
                    column_info: vec![hive_string_column("marker")],
                    rows: vec![row(&[Some("north")]), row(&[Some("south")])],
                },
                "\"marker\"\n\"north\"\n\"south\"\n",
            ),
            (
                "a value carrying the field separator",
                StoredResultSet {
                    column_info: vec![hive_string_column("place")],
                    rows: vec![row(&[Some("Paris, France")])],
                },
                "\"place\"\n\"Paris, France\"\n",
            ),
            (
                "a value carrying the quote character",
                StoredResultSet {
                    column_info: vec![hive_string_column("quoted")],
                    rows: vec![row(&[Some("a \"b\" c")])],
                },
                "\"quoted\"\n\"a \"\"b\"\" c\"\n",
            ),
            (
                "a NULL beside the empty string",
                StoredResultSet {
                    column_info: vec![hive_string_column("left"), hive_string_column("right")],
                    rows: vec![row(&[None, Some("")])],
                },
                "\"left\",\"right\"\n,\"\"\n",
            ),
            (
                "a result set with no rows",
                StoredResultSet {
                    column_info: vec![hive_string_column("empty")],
                    rows: Vec::new(),
                },
                "\"empty\"\n",
            ),
            (
                "a result set with no columns",
                StoredResultSet::default(),
                "",
            ),
        ];

        for (case, result, expected) in &cases {
            let actual = String::from_utf8(results_csv(result)).expect("the CSV is text");
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the results object {expected:?}, got {actual:?}"
            );
        }
    }
}
