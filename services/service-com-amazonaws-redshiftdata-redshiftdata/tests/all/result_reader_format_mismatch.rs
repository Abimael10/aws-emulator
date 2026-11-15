// SPDX-License-Identifier: CC0-1.0

//! The refusal a result reader answers when it is asked for a statement whose
//! result is read in the OTHER format, driven over the wire.
//!
//! ONE rule, asserted over a TABLE of inputs rather than over the one request
//! that showed it: a statement's rows are read by the reader that serves the
//! format the statement was SUBMITTED with, and the other reader refuses it with
//! that reader's own fixed sentence — one naming the reader that was CALLED and
//! the reader the caller is to switch to, interpolating nothing from the request
//! or from state. The rows differ along what the rule generalises over: the
//! format submitted (either modelled token, and the absent member that takes the
//! documented JSON default), the reader addressed, the statement's shape (one
//! column, several columns and rows, a cell carrying the CSV separator) and
//! whether the statement is a single submission or one statement of a batch.

use serde_json::{json, Value};

use super::emulator::{error_code, id_of, Emulator};

/// The sentence `GetStatementResult` — the reader of the JSON form — refuses a
/// statement read in the other format with. Recorded fixed text; the model
/// states no message for either refusal.
const JSON_READER_REFUSAL: &str = "GetStatementResult can only return results that are in JSON \
                                   format. Try using GetStatementResultV2 for other result \
                                   formats";

/// The mirror of it, from the reader of the CSV form.
const CSV_READER_REFUSAL: &str = "GetStatementResultV2 can only return results that are in CSV \
                                  format. Try using GetStatementResult instead for JSON format";

/// The connection every statement in this suite runs through.
fn cluster() -> Value {
    json!({"ClusterIdentifier": "my-cluster", "Database": "dev", "DbUser": "awsuser"})
}

/// The `Message` a handler's own refusal carries.
fn message(response: &Value) -> String {
    response["Message"].as_str().unwrap_or_default().to_string()
}

/// The reader that serves the format `reader` does not.
fn other_reader(reader: &str) -> &'static str {
    if reader == "GetStatementResultV2" {
        "GetStatementResult"
    } else {
        "GetStatementResultV2"
    }
}

/// The sentence `reader` refuses a format mismatch with.
fn refusal_of(reader: &str) -> &'static str {
    if reader == "GetStatementResultV2" {
        CSV_READER_REFUSAL
    } else {
        JSON_READER_REFUSAL
    }
}

/// One row of the reader table: what the submit asked for, what it submitted,
/// the reader the caller addressed, and whether that reader serves the result.
struct ReaderCase {
    label: &'static str,
    /// The `ResultFormat` the submit sends, or `None` for a submit that sends
    /// none and so takes the documented JSON default.
    submitted: Option<&'static str>,
    /// The statements submitted: one for `ExecuteStatement`, more than one for
    /// `BatchExecuteStatement`.
    sqls: &'static [&'static str],
    /// For a batch, the 1-based position whose own `<id>:<n>` is read.
    position: Option<u32>,
    /// The reader the caller addresses.
    reader: &'static str,
    /// Whether that reader is the WRONG one for the statement's format.
    mismatch: bool,
}

/// RULE: each result reader serves exactly the format it is the reader for, and
/// refuses a statement read in the other format with its own fixed sentence,
/// naming the reader called and the reader to switch to.
#[tokio::test]
async fn the_wrong_result_reader_is_refused_with_that_readers_own_sentence() {
    let emulator = Emulator::new();
    let cases = [
        ReaderCase {
            label: "a statement submitted with no format at all, read by the CSV reader",
            submitted: None,
            sqls: &["SELECT 1 AS n"],
            position: None,
            reader: "GetStatementResultV2",
            mismatch: true,
        },
        ReaderCase {
            label: "a JSON statement of several columns and rows, read by the CSV reader",
            submitted: Some("JSON"),
            sqls: &["SELECT 1 AS n, 'a' AS t UNION ALL SELECT 2 AS n, 'b' AS t"],
            position: None,
            reader: "GetStatementResultV2",
            mismatch: true,
        },
        ReaderCase {
            label: "a CSV statement read by the JSON reader",
            submitted: Some("CSV"),
            sqls: &["SELECT 1 AS n"],
            position: None,
            reader: "GetStatementResult",
            mismatch: true,
        },
        ReaderCase {
            label: "a CSV statement whose cell carries the separator, read by the JSON reader",
            submitted: Some("CSV"),
            sqls: &["SELECT 'has,comma' AS value, 1 AS tail"],
            position: None,
            reader: "GetStatementResult",
            mismatch: true,
        },
        ReaderCase {
            label: "one statement of a batch submitted with no format, read by the CSV reader",
            submitted: None,
            sqls: &["SELECT 1 AS n", "SELECT 2 AS n"],
            position: Some(2),
            reader: "GetStatementResultV2",
            mismatch: true,
        },
        ReaderCase {
            label: "one statement of a CSV batch, read by the JSON reader",
            submitted: Some("CSV"),
            sqls: &["SELECT 1 AS n", "SELECT 2 AS n"],
            position: Some(1),
            reader: "GetStatementResult",
            mismatch: true,
        },
        ReaderCase {
            label: "a CSV statement read by the CSV reader",
            submitted: Some("CSV"),
            sqls: &["SELECT 1 AS n"],
            position: None,
            reader: "GetStatementResultV2",
            mismatch: false,
        },
        ReaderCase {
            label: "a JSON statement read by the JSON reader",
            submitted: Some("JSON"),
            sqls: &["SELECT 1 AS n"],
            position: None,
            reader: "GetStatementResult",
            mismatch: false,
        },
        ReaderCase {
            label: "one statement of a JSON batch, read by the JSON reader",
            submitted: Some("JSON"),
            sqls: &["SELECT 1 AS n", "SELECT 2 AS n"],
            position: Some(2),
            reader: "GetStatementResult",
            mismatch: false,
        },
    ];

    // Every refusal the table collected, per reader: the sentence is FIXED, so
    // the rows of one reader must all carry the very same bytes.
    let mut collected: Vec<(&str, &str, String)> = Vec::new();

    for case in &cases {
        let mut request = cluster();
        if let Some(submitted) = case.submitted {
            request["ResultFormat"] = json!(submitted);
        }
        let submitted = if case.sqls.len() == 1 {
            request["Sql"] = json!(case.sqls[0]);
            emulator.execute_and_wait(&request).await
        } else {
            request["Sqls"] = json!(case.sqls);
            emulator.batch_and_wait(&request).await
        };
        let parent = id_of(&submitted);
        let id = match case.position {
            Some(position) => format!("{parent}:{position}"),
            None => parent.clone(),
        };

        let (status, response) = emulator.call(case.reader, &json!({"Id": id})).await;

        if !case.mismatch {
            assert_eq!(
                status, 200,
                "case {:?}: the reader of the statement's own format must serve it, got {status} \
                 {response}",
                case.label
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {:?}: expected 400, got {status} {response}",
            case.label
        );
        assert_eq!(
            error_code(&response),
            "ValidationException",
            "case {:?}: expected ValidationException, got {response}",
            case.label
        );
        let stated = message(&response);
        let expected = refusal_of(case.reader);
        assert_eq!(
            stated, expected,
            "case {:?}: expected {expected:?}, got {stated:?}",
            case.label
        );
        // The sentence names the reader that was CALLED, and the one the caller
        // is to switch to.
        assert!(
            stated.starts_with(case.reader),
            "case {:?}: the sentence must name the reader that was called, got {stated:?}",
            case.label
        );
        assert!(
            stated.contains(other_reader(case.reader)),
            "case {:?}: the sentence must name the reader to switch to, got {stated:?}",
            case.label
        );
        // And it interpolates nothing from the request or from state: not the
        // id it was asked for, not the statement's text, not the connection.
        for part in [id.as_str(), parent.as_str(), case.sqls[0], "my-cluster", "dev"] {
            assert!(
                !stated.contains(part),
                "case {:?}: the sentence carries no part of the request, found {part:?} in \
                 {stated:?}",
                case.label
            );
        }

        // The refused read changed nothing: the reader that DOES serve this
        // statement still answers it.
        let correct = other_reader(case.reader);
        let (served_status, served) = emulator.call(correct, &json!({"Id": id})).await;
        assert_eq!(
            served_status, 200,
            "case {:?}: {correct} must still serve the statement, got {served_status} {served}",
            case.label
        );

        collected.push((case.label, case.reader, stated));
    }

    for reader in ["GetStatementResult", "GetStatementResultV2"] {
        let rows: Vec<&(&str, &str, String)> = collected
            .iter()
            .filter(|(_, called, _)| *called == reader)
            .collect();
        assert!(
            rows.len() > 1,
            "the table must refuse {reader} on more than one statement, got {} row(s)",
            rows.len()
        );
        let first = rows[0].2.as_str();
        for (label, _, stated) in &rows {
            assert_eq!(
                stated.as_str(),
                first,
                "case {label:?}: {reader}'s refusal is fixed text, so every row of it carries the \
                 same sentence, got {stated:?} beside {first:?}"
            );
        }
    }
}
