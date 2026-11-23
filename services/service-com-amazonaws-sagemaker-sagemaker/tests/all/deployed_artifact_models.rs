// SPDX-License-Identifier: CC0-1.0

//! What model a DEPLOYED ARTIFACT holds, at the one reading that takes it out of the artifact's
//! bytes (`state::model_from_artifact`) — the reading every invocation of a deployed model goes
//! through.
//!
//! RULE: the model an endpoint serves is the one its artifact carries, in whichever form the
//! artifact carries it: the model a training run of this emulator fitted and wrote into the
//! archive it uploaded, and a serialized gradient-boosted model, as the model file itself or as
//! an entry of the compressed archive a container uploads, in either of that format's two
//! encodings — text and binary. Bytes carrying no model's parameters hold no model at all, so
//! nothing is served from them.
//!
//! The rows differ along what the rule generalises over: which of the two readings the artifact
//! answers, how the artifact packages the model (the bare model file, an entry of a compressed
//! archive), which encoding the document is in (text, binary), the objective the learner
//! declares, and the shape of bytes that carry no model.
//!
//! The predictions asserted are computed from the parameters each row WRITES into its artifact,
//! so what is checked is that the served answer follows the artifact rather than the
//! implementation.

use serde_json::{json, Value};
use service_com_amazonaws_sagemaker_sagemaker::state::{
    fit, fit_epochs, model_archive, model_from_artifact, serve_inference, Record, ServedModel,
    CSV_MEDIA_TYPE, MODEL_ARCHIVE_ENTRY,
};

/// A serialized gradient-boosted model of one tree: the root splits feature 0 at `threshold`,
/// a record below it reaching the leaf `low` and one at or above it the leaf `high`.
fn booster_document(objective: &str, threshold: f64, low: f64, high: f64) -> Value {
    json!({
        "learner": {
            "learner_model_param": {
                "base_score": "5E-1",
                "num_feature": "2",
                "num_class": "0",
            },
            "objective": { "name": objective },
            "gradient_booster": {
                "name": "gbtree",
                "model": {
                    "trees": [{
                        "split_indices": [0, 0, 0],
                        "split_conditions": [threshold, low, high],
                        "left_children": [1, -1, -1],
                        "right_children": [2, -1, -1],
                        "base_weights": [0.0, low, high],
                        "default_left": [1, 0, 0],
                    }],
                    "tree_info": [0],
                },
            },
        },
    })
}

/// A serialized LINEAR booster: one coefficient per feature, then the bias.
fn linear_document(coefficients: &[f64], bias: f64) -> Value {
    let mut weights: Vec<f64> = coefficients.to_vec();
    weights.push(bias);
    json!({
        "learner": {
            "learner_model_param": {
                "base_score": "0",
                "num_feature": coefficients.len().to_string(),
                "num_class": "0",
            },
            "objective": { "name": "reg:squarederror" },
            "gradient_booster": { "name": "gblinear", "model": { "weights": weights } },
        },
    })
}

/// `document` in the BINARY encoding of the same format — the encoding the boosting library
/// writes by default, and the one the service's own reading decodes.
///
/// Written here in the format's plain forms (a marker, then the value's bytes, big-endian;
/// text and a member's name as an integer length then its bytes), so a row can drive the
/// binary side of the reading with the same document as the text side.
fn as_binary(document: &Value) -> Vec<u8> {
    match document {
        Value::Null => vec![b'Z'],
        Value::Bool(true) => vec![b'T'],
        Value::Bool(false) => vec![b'F'],
        Value::Number(number) => {
            let mut bytes = vec![b'D'];
            bytes.extend_from_slice(&number.as_f64().unwrap_or_default().to_be_bytes());
            bytes
        }
        Value::String(text) => {
            let mut bytes = vec![b'S'];
            bytes.extend(binary_length(text.len()));
            bytes.extend_from_slice(text.as_bytes());
            bytes
        }
        Value::Array(items) => {
            let mut bytes = vec![b'['];
            for item in items {
                bytes.extend(as_binary(item));
            }
            bytes.push(b']');
            bytes
        }
        Value::Object(members) => {
            let mut bytes = vec![b'{'];
            for (name, value) in members {
                bytes.extend(binary_length(name.len()));
                bytes.extend_from_slice(name.as_bytes());
                bytes.extend(as_binary(value));
            }
            bytes.push(b'}');
            bytes
        }
    }
}

/// A length in the binary encoding: a 32-bit integer, which every length this suite writes
/// fits in.
fn binary_length(length: usize) -> Vec<u8> {
    let mut bytes = vec![b'l'];
    bytes.extend_from_slice(&(length as i32).to_be_bytes());
    bytes
}

/// The archive a container uploads, with `document` as its single entry: the compressed tar the
/// service writes its own model into, which is also where a container's model file lives.
fn as_archive(document: &Value) -> Vec<u8> {
    let body = serde_json::to_vec_pretty(document).expect("the document serialises");
    model_archive(&[(MODEL_ARCHIVE_ENTRY.to_string(), body)]).expect("the archive is built")
}

/// The artifact a training run of this emulator writes, and the model it holds: a fit over
/// records whose target is a linear function of their features.
fn fitted_artifact() -> (Vec<u8>, f64) {
    let records: Vec<Record> = (0..40)
        .map(|row| {
            let features: Vec<f64> = (0..2).map(|column| (row + column + 1) as f64).collect();
            Record {
                target: 3.0 + features.iter().sum::<f64>(),
                features,
            }
        })
        .collect();
    let model = fit(&records, fit_epochs(records.len(), 2));
    let predicted = model.predict(&[1.0, 2.0]);
    let document = json!({
        "model": {
            "type": "linear",
            "intercept": model.intercept,
            "weights": model.weights,
            "epochs": model.epochs,
        },
    });
    (as_archive(&document), predicted)
}

/// One row of the artifact table.
struct Row {
    case: &'static str,
    /// The artifact's bytes, as Amazon S3 would hold them.
    artifact: Vec<u8>,
    /// The record one invocation carries.
    record: Vec<f64>,
    /// What the model the artifact holds answers for that record, computed from the parameters
    /// the row wrote.
    expected: f64,
}

/// The artifact table.
fn rows() -> Vec<Row> {
    let logistic = booster_document("binary:logistic", 5.0, -2.0, 3.0);
    let regression = booster_document("reg:squarederror", 5.0, -2.0, 3.0);
    let (fitted, fitted_prediction) = fitted_artifact();
    vec![
        Row {
            case: "a serialized booster as the model file itself",
            artifact: logistic.to_string().into_bytes(),
            record: vec![1.0, 2.0],
            expected: 1.0 / (1.0 + (2.0_f64).exp()),
        },
        Row {
            case: "the same booster as an entry of the uploaded archive",
            artifact: as_archive(&logistic),
            record: vec![9.0, 2.0],
            expected: 1.0 / (1.0 + (-3.0_f64).exp()),
        },
        Row {
            case: "a serialized booster in the binary encoding",
            artifact: as_binary(&logistic),
            record: vec![1.0, 2.0],
            expected: 1.0 / (1.0 + (2.0_f64).exp()),
        },
        Row {
            case: "a regression booster answers the margin itself",
            artifact: regression.to_string().into_bytes(),
            record: vec![9.0, 2.0],
            // The base score the margin starts from (one half, as the document states it) plus
            // the leaf the record reaches.
            expected: 0.5 + 3.0,
        },
        Row {
            case: "a linear booster's coefficients against the record",
            artifact: linear_document(&[2.0, -0.5], 1.0).to_string().into_bytes(),
            record: vec![3.0, 4.0],
            // 1 + 2*3 + (-0.5)*4
            expected: 5.0,
        },
        Row {
            case: "the model a training run of this emulator fitted",
            artifact: fitted,
            record: vec![1.0, 2.0],
            expected: fitted_prediction,
        },
    ]
}

/// THE RULE: the model served is the one the artifact carries, whichever form it is in.
#[test]
fn the_model_served_is_the_one_the_artifact_carries() {
    for row in rows() {
        let case = row.case;
        let model = model_from_artifact(&row.artifact)
            .unwrap_or_else(|| panic!("case {case:?}: the artifact holds a model"));
        let served = model.predict(&row.record);
        assert!(
            (served - row.expected).abs() < 1e-9,
            "case {case:?}: expected {}, got {served}",
            row.expected
        );

        // And the same model answers one invocation of that record: one prediction, rendered as
        // the comma-separated record an endpoint answers a payload of them with.
        let payload = row
            .record
            .iter()
            .map(f64::to_string)
            .collect::<Vec<String>>()
            .join(",");
        let answer = serve_inference(&model, payload.as_bytes(), Some(CSV_MEDIA_TYPE), None)
            .unwrap_or_else(|refusal| panic!("case {case:?}: the payload is served, got {refusal:?}"));
        let rendered = String::from_utf8(answer.body)
            .unwrap_or_else(|error| panic!("case {case:?}: the answer is text: {error}"));
        let read = rendered
            .parse::<f64>()
            .unwrap_or_else(|error| panic!("case {case:?}: {rendered:?} is no number: {error}"));
        assert!(
            (read - row.expected).abs() < 1e-9,
            "case {case:?}: the invocation is answered what the model computes, expected {}, got \
             {read}",
            row.expected
        );
    }
}

/// RULE: a model a RUN of this emulator fitted is read as that model, and a serialized booster
/// as a booster — the two readings do not take each other's artifacts.
#[test]
fn each_artifact_is_read_as_the_model_it_holds() {
    let (fitted, _) = fitted_artifact();
    let booster = booster_document("binary:logistic", 5.0, -2.0, 3.0);
    for (case, artifact, is_fitted) in [
        ("the artifact a training run wrote", fitted, true),
        (
            "a serialized booster",
            booster.to_string().into_bytes(),
            false,
        ),
        ("a serialized booster in an archive", as_archive(&booster), false),
    ] {
        let model = model_from_artifact(&artifact)
            .unwrap_or_else(|| panic!("case {case:?}: the artifact holds a model"));
        let read_as_fitted = matches!(model, ServedModel::Fitted(_));
        assert_eq!(
            read_as_fitted, is_fitted,
            "case {case:?}: expected fitted={is_fitted}, got {model:?}"
        );
        // A model fitted HERE was fitted on a fixed set of feature columns, so a record of
        // another width cannot be applied to it; a booster routes a feature a record does not
        // carry down the node's own default direction, so every width is one it applies to.
        assert_eq!(
            model.fitted_width().is_some(),
            is_fitted,
            "case {case:?}: only a model fitted here has a feature width to match"
        );
    }
}

/// RULE: bytes carrying no model's parameters hold no model, so nothing is served from them.
///
/// The rows differ along how the bytes fall short: empty, text that is no document, a document
/// of another kind, a compressed archive whose entry holds neither model, and bytes that are
/// not an archive at all.
#[test]
fn bytes_carrying_no_parameters_hold_no_model() {
    let rows: Vec<(&str, Vec<u8>)> = vec![
        ("no bytes at all", Vec::new()),
        ("text that is no document", b"not a model".to_vec()),
        (
            "a document of another kind",
            json!({ "hyperParameters": { "objective": "binary:logistic" } })
                .to_string()
                .into_bytes(),
        ),
        (
            "an archive whose entry holds neither model",
            as_archive(&json!({ "notes": "no parameters here" })),
        ),
        (
            "bytes that are not an archive",
            vec![0x1f, 0x8b, 0x08, 0x00, 0x01, 0x02, 0x03],
        ),
    ];
    for (case, artifact) in rows {
        let read = model_from_artifact(&artifact);
        assert!(
            read.is_none(),
            "case {case:?}: bytes carrying no parameters must hold no model, got {read:?}"
        );
    }
}
