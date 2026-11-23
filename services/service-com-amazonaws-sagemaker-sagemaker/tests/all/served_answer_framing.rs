// SPDX-License-Identifier: CC0-1.0

//! How an endpoint's inference ANSWER is framed, at the reading that renders it
//! (`state::serve_inference`) — the one place every invocation of a model this emulator
//! fitted is answered from.
//!
//! RULE: an answer carries one prediction per input record, in the order the payload carried
//! them. Rendered as `text/csv` it is ONE record of comma-separated values with no line
//! terminator — the framing a client reads by splitting the answer on commas — and rendered
//! as `application/json` it is the `predictions` list of `score` objects. Either way the answer
//! declares the content type its bytes really carry: a textual rendering names the character
//! set they are written in, and `application/json`, whose own definition fixes the encoding,
//! takes no such parameter.
//!
//! The rows differ along what the rule generalises over: how many records a payload carries
//! (one, two, more than the model's own feature count, the widest batch this suite drives),
//! the form the payload is written in (comma-separated records, a JSON list of rows, a JSON
//! `instances` list, one bare JSON row) and the media type the answer is asked for (none,
//! `text/csv`, `application/json`, a type carrying parameters).
//!
//! The model under test is FITTED by this crate's own reading rather than hand-built, so the
//! values asserted are the ones an endpoint really serves.

use serde_json::Value;
use service_com_amazonaws_sagemaker_sagemaker::state::{
    fit, fit_epochs, serve_inference, Record, ServedModel, CSV_ANSWER_CONTENT_TYPE, CSV_MEDIA_TYPE,
    JSON_MEDIA_TYPE,
};

/// How many feature columns the fitted model takes.
const FEATURES: usize = 3;

/// A model fitted on records whose target is a linear function of their features, so every
/// prediction below is a measurement of a real fit rather than a constant.
fn fitted() -> ServedModel {
    let records: Vec<Record> = (0..40)
        .map(|row| {
            let features: Vec<f64> = (0..FEATURES)
                .map(|column| (row + column + 1) as f64)
                .collect();
            Record {
                target: 3.0 + features.iter().sum::<f64>(),
                features,
            }
        })
        .collect();
    let epochs = fit_epochs(records.len(), FEATURES);
    ServedModel::Fitted(fit(&records, epochs))
}

/// The record at `index` of a payload: `FEATURES` values offset by the index, so no two
/// records are the same.
fn record(index: usize) -> Vec<f64> {
    (0..FEATURES)
        .map(|column| (index * 10 + column + 1) as f64)
        .collect()
}

/// `records` records as comma-separated rows, one per line.
fn csv_payload(records: usize) -> String {
    (0..records)
        .map(|index| {
            let row: Vec<String> = record(index).iter().map(f64::to_string).collect();
            format!("{}\n", row.join(","))
        })
        .collect()
}

/// `records` records as a JSON list of rows.
fn json_rows(records: usize) -> Vec<Vec<f64>> {
    (0..records).map(record).collect()
}

/// One row of the table: the payload, how many records it carries, and the media type the
/// answer is asked for.
struct Row {
    case: &'static str,
    payload: String,
    records: usize,
    accept: Option<&'static str>,
    /// The content type the answer must be served under.
    answered_as: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one comma-separated record, no Accept",
            payload: csv_payload(1),
            records: 1,
            accept: None,
            answered_as: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "two comma-separated records, text/csv asked for",
            payload: csv_payload(2),
            records: 2,
            accept: Some(CSV_MEDIA_TYPE),
            answered_as: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "more records than the model has features",
            payload: csv_payload(FEATURES + 5),
            records: FEATURES + 5,
            accept: Some(CSV_MEDIA_TYPE),
            answered_as: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "a wider batch, no Accept",
            payload: csv_payload(32),
            records: 32,
            accept: None,
            answered_as: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "a JSON list of rows, no Accept",
            payload: Value::from(json_rows(3)).to_string(),
            records: 3,
            accept: None,
            answered_as: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "a JSON instances list, text/csv asked for",
            payload: serde_json::json!({ "instances": json_rows(4) }).to_string(),
            records: 4,
            accept: Some(CSV_MEDIA_TYPE),
            answered_as: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "one bare JSON row, no Accept",
            payload: Value::from(record(7)).to_string(),
            records: 1,
            accept: None,
            answered_as: CSV_ANSWER_CONTENT_TYPE,
        },
        Row {
            case: "two comma-separated records, application/json asked for",
            payload: csv_payload(2),
            records: 2,
            accept: Some(JSON_MEDIA_TYPE),
            answered_as: JSON_MEDIA_TYPE,
        },
        Row {
            case: "a JSON answer asked for with parameters",
            payload: csv_payload(3),
            records: 3,
            accept: Some("application/json; charset=utf-8"),
            answered_as: JSON_MEDIA_TYPE,
        },
    ]
}

/// The predictions an answer carries, read the way a client of that media type reads it.
/// `media_type` is the answer's own ([`ServedInference::media_type`]), so there is one reading of
/// what a content type's parameters leave out.
fn answered(body: &[u8], media_type: &str, case: &str) -> Vec<f64> {
    let text = String::from_utf8(body.to_vec())
        .unwrap_or_else(|error| panic!("case {case:?}: the answer is not text: {error}"));
    if media_type == JSON_MEDIA_TYPE {
        let document: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("case {case:?}: {text:?} is no JSON: {error}"));
        return document
            .get("predictions")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: no predictions in {text}"))
            .iter()
            .map(|entry| {
                entry
                    .get("score")
                    .and_then(Value::as_f64)
                    .unwrap_or_else(|| panic!("case {case:?}: no score in {entry}"))
            })
            .collect();
    }
    text.split(',')
        .map(|field| {
            field
                .parse::<f64>()
                .unwrap_or_else(|error| panic!("case {case:?}: {field:?} is no number: {error}"))
        })
        .collect()
}

#[test]
fn a_csv_answer_is_one_comma_separated_record_of_predictions() {
    let model = fitted();
    for row in rows() {
        let served = serve_inference(&model, row.payload.as_bytes(), None, row.accept)
            .unwrap_or_else(|refusal| {
                panic!("case {:?}: the payload must be served, got {refusal:?}", row.case)
            });
        assert_eq!(
            served.content_type, row.answered_as,
            "case {:?}: the answer's content type, got {:?}",
            row.case, served.content_type
        );
        let text = String::from_utf8(served.body.clone())
            .unwrap_or_else(|error| panic!("case {:?}: the answer is not text: {error}", row.case));
        if served.media_type() == CSV_MEDIA_TYPE {
            assert!(
                !text.contains('\n') && !text.contains('\r'),
                "case {:?}: a comma-separated answer carries no line terminator, got {text:?}",
                row.case
            );
            assert_eq!(
                text.split(',').count(),
                row.records,
                "case {:?}: one comma-separated field per record, got {text:?}",
                row.case
            );
        }
        let predictions = answered(&served.body, served.media_type(), row.case);
        assert_eq!(
            predictions.len(),
            row.records,
            "case {:?}: one prediction per record, got {predictions:?}",
            row.case
        );
        for value in &predictions {
            assert!(
                value.is_finite(),
                "case {:?}: every prediction is a computed number, got {predictions:?}",
                row.case
            );
        }
    }
}

#[test]
fn every_prediction_is_the_one_its_own_record_is_answered_with() {
    let model = fitted();
    for row in rows() {
        let served = serve_inference(&model, row.payload.as_bytes(), None, row.accept)
            .unwrap_or_else(|refusal| {
                panic!("case {:?}: the payload must be served, got {refusal:?}", row.case)
            });
        let batched = answered(&served.body, served.media_type(), row.case);
        // The payload's records are the rows `record(index)` builds, in that order, except
        // the bare-JSON row, which carries exactly one record of its own.
        let expected: Vec<f64> = if row.case == "one bare JSON row, no Accept" {
            vec![model.predict(&record(7))]
        } else {
            (0..row.records).map(|index| model.predict(&record(index))).collect()
        };
        assert_eq!(
            batched, expected,
            "case {:?}: the answer carries each record's own prediction in the order the \
             payload carried them, got {batched:?}",
            row.case
        );
    }
}
