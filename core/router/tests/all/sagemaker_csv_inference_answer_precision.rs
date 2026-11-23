// SPDX-License-Identifier: CC0-1.0

//! The PRECISION a `text/csv` inference answer renders its predictions at.
//!
//! RULE: the inference an endpoint answers is the bytes the model behind it produced
//! ("includes the inference provided by the model", `InvokeEndpointOutput$Body`), so a
//! comma-separated answer carries each prediction as a decimal that reads back as exactly the
//! value the model computed — one field per input record on a single line, separated by `,`,
//! with no digit dropped to a fixed number of decimal places — declared under a content type
//! that names the character set the text is written in. A rendering that rounds answers a
//! different number than the model computed, and a renderer that collapses two predictions a
//! hair apart reports one value for two.
//!
//! Only a cross-service suite can ask this: the flow spans the services
//! `core/router/src/lib.rs` mounts on ONE engine — IAM for the execution role, Amazon S3 for
//! the deployed artifact, the SageMaker control plane to deploy, and the SageMaker runtime to
//! invoke. The emulator, the request builders, the role, the deployment and the
//! poll-until-settled reading are the serving suite's
//! ([`crate::sagemaker_endpoint_serves_a_trained_model`]) and the serialized booster is the
//! answer suite's ([`crate::sagemaker_endpoint_answer_declares_variant_and_content_type`]),
//! rather than a second copy of either.
//!
//! What is read here is what neither of those asks: how much of a prediction SURVIVES the
//! rendering. The first test holds every rendered field against the value the deployed
//! artifact's own parameters imply, computed in the suite rather than read off another run, and
//! requires the field to BE the shortest decimal that reads back as the value it denotes — so a
//! field that dropped digits fails whatever it carries; the second shifts one feature by ever
//! smaller amounts, so a rendering that keeps too few digits is caught by the two answers
//! coming back as one.
//!
//! The rows differ along what the rule generalises over: how many records one payload carries,
//! the form it is written in (comma-separated records, a JSON list), the media type the answer
//! is asked for (none, `text/csv`), the feature values a record carries, and — for the second
//! test — how far apart two records' predictions are.

use smithy_server_runtime::MultiServiceRouter;

use crate::sagemaker_endpoint_answer_declares_variant_and_content_type::{
    artifact_document, expected_prediction, payload, COEFFICIENTS,
};
use crate::sagemaker_endpoint_serves_a_trained_model::{
    deploy, emulator, given_bucket, given_object, given_role, invoke, BUCKET,
};

/// The content type a comma-separated inference answer is served under, spelled out here
/// rather than imported: the media type its bytes are rendered in and the character set they
/// are written in, which is the value `InvokeEndpointOutput$ContentType` has to carry for such
/// an answer, so a wrong media type or a missing parameter in the implementation fails these
/// tests. The sibling suite
/// ([`crate::sagemaker_endpoint_answer_declares_variant_and_content_type`]) asks the same of
/// every answer it drives, through the serving crate's own constant.
const CSV_ANSWER_CONTENT_TYPE: &str = "text/csv; charset=utf-8";

/// How far a rendered prediction may sit from the value the deployed booster's own parameters
/// imply: the two are the same arithmetic over the same doubles, so this leaves room for the
/// order a sum is accumulated in and for nothing a rendering could drop. Any rounding to a
/// fixed number of decimal places is orders of magnitude coarser than this.
///
/// It is taken against the magnitude of the value or one, whichever is larger
/// ([`tolerated`]), so a prediction near zero is held to the same absolute room rather than to
/// bit-exact equality.
const PRECISION_TOLERANCE: f64 = 1e-12;

/// The room a comparison of `wanted` leaves: `tolerance` of its own magnitude, and of one where
/// that magnitude is smaller, so no case demands more of the arithmetic than of the rendering.
fn tolerated(wanted: f64, tolerance: f64) -> f64 {
    wanted.abs().max(1.0) * tolerance
}

/// One row of the rendering table.
struct Row {
    case: &'static str,
    /// The records the payload carries, each as wide as the deployed booster's feature count.
    records: Vec<Vec<f64>>,
    /// Whether the payload is written as JSON rather than as comma-separated records.
    json_payload: bool,
    /// The request's `Accept`, when it asks for a media type.
    accept: Option<&'static str>,
}

/// Rows differ along how many records one payload carries (one, two, five), the form it is
/// written in, the media type the answer is asked for, and the feature values a record carries
/// — integral ones, fractional ones whose products are not exact in binary, and negative ones.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one fractional record, no Accept",
            records: vec![vec![0.1, 0.2, 0.3]],
            json_payload: false,
            accept: None,
        },
        Row {
            case: "two fractional records, text/csv asked for",
            records: vec![vec![0.7, 1.3, 7.7], vec![-0.1, 2.9, 0.07]],
            json_payload: false,
            accept: Some("text/csv"),
        },
        Row {
            case: "five records, text/csv asked for",
            records: vec![
                vec![1.0, 2.0, 3.0],
                vec![0.001, 0.003, 0.007],
                vec![-4.25, 11.1, -0.9],
                vec![1e6, 1.0 / 3.0, 2.0 / 7.0],
                vec![0.0, 0.0, 0.0],
            ],
            json_payload: false,
            accept: Some("text/csv"),
        },
        Row {
            case: "a JSON list of fractional records, no Accept",
            records: vec![vec![0.9, 0.09, 0.009], vec![3.3, 6.6, 9.9]],
            json_payload: true,
            accept: None,
        },
        Row {
            case: "a JSON list of one record, text/csv asked for",
            records: vec![vec![1.0 / 7.0, 2.0 / 11.0, 3.0 / 13.0]],
            json_payload: true,
            accept: Some("text/csv"),
        },
    ]
}

/// The content type a payload of this form declares.
fn request_content_type(json_payload: bool) -> &'static str {
    if json_payload {
        "application/json"
    } else {
        "text/csv"
    }
}

/// The booster the whole suite is driven against, deployed behind `endpoint` on one variant,
/// answering once the endpoint reports `InService`.
async fn given_endpoint(router: &MultiServiceRouter, case: &str, name: &str, endpoint: &str) {
    given_bucket(router).await;
    given_role(router).await;
    let key = format!("artifacts/{name}-booster.json");
    given_object(router, &key, &artifact_document()).await;
    deploy(
        router,
        case,
        name,
        endpoint,
        "AllTraffic",
        Some(&format!("s3://{BUCKET}/{key}")),
    )
    .await;
}

/// RULE: every field of a comma-separated answer carries exactly the prediction the model
/// computed, as the shortest decimal that reads back as it.
#[tokio::test]
async fn a_csv_answer_renders_every_prediction_as_the_value_the_model_computed() {
    let router = emulator();
    let endpoint = "precision-endpoint";
    given_endpoint(&router, "the rendering model", "precision-model", endpoint).await;

    // A rendering rounded to a fixed number of decimal places is only CAUGHT by a prediction
    // that needs more than that many, so the table is held to carrying one.
    let mut beyond_one_decimal = false;
    for row in rows() {
        let case = row.case;
        let body = payload(&row.records, row.json_payload);
        let content_type = request_content_type(row.json_payload);
        let mut headers: Vec<(&str, &str)> = vec![("content-type", content_type)];
        if let Some(accept) = row.accept {
            headers.push(("accept", accept));
        }
        let answer = invoke(&router, endpoint, &headers, body.as_bytes()).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: the invocation must be served, got {} with {}",
            answer.status,
            answer.text()
        );
        let declared = answer.header("content-type");
        assert_eq!(
            declared, CSV_ANSWER_CONTENT_TYPE,
            "case {case:?}: the answer must declare the content type its bytes carry, got \
             {declared:?}"
        );
        assert!(
            declared.to_ascii_lowercase().contains("charset=utf-8"),
            "case {case:?}: a textual answer names the character set it is written in, got \
             {declared:?}"
        );
        let text = answer.text();
        assert!(
            !text.contains('\n') && !text.contains('\r'),
            "case {case:?}: a comma-separated answer carries no line terminator, got {text:?}"
        );
        let fields: Vec<&str> = text.split(',').collect();
        assert_eq!(
            fields.len(),
            row.records.len(),
            "case {case:?}: one field per record, got {text:?}"
        );

        for (index, field) in fields.iter().enumerate() {
            let record = &row.records[index];
            // The field is the decimal that reads back as the value it denotes and no longer:
            // neither padded to a fixed width nor cut to a fixed number of places.
            let read: f64 = field.parse().unwrap_or_else(|error| {
                panic!("case {case:?}: field {index} {field:?} is no number: {error}")
            });
            assert_eq!(
                *field,
                read.to_string().as_str(),
                "case {case:?}: record {record:?} must be rendered {:?}, got {field:?}",
                read.to_string()
            );
            // And the value it denotes is the prediction the artifact's OWN parameters imply,
            // computed here rather than read off another run: a rendering that dropped digits
            // lands further from it than the order a sum is accumulated in can.
            let wanted = expected_prediction(record);
            assert!(
                (read - wanted).abs() <= tolerated(wanted, PRECISION_TOLERANCE),
                "case {case:?}: record {record:?} must be answered {wanted}, and the field \
                 {field:?} reads back as {read}"
            );
            if field
                .split_once('.')
                .is_some_and(|(_, decimals)| decimals.len() > 1)
            {
                beyond_one_decimal = true;
            }
        }
    }
    assert!(
        beyond_one_decimal,
        "the table must carry a prediction a one-decimal rendering could not hold, or it asks \
         nothing about precision"
    );
}

/// One row of the neighbouring-records table: how far one feature is shifted, and the name the
/// failure reports it by.
const NEIGHBOURS: [(&str, f64); 4] = [
    ("a tenth", 1e-1),
    ("a thousandth", 1e-3),
    ("a millionth", 1e-6),
    ("a billionth", 1e-9),
];

/// How far the difference the two answers report may sit from the difference the booster's own
/// coefficient implies: the two are the same arithmetic over the same doubles, so this leaves
/// room for the order a sum is accumulated in and nothing else.
///
/// It is taken against the magnitude of that difference ALONE, with no floor under it: the
/// difference a row asks about is itself tiny, and room of a fixed size would admit a pair of
/// answers that differ by nothing at all — which is the very collapse this test is here to
/// catch.
const DIFFERENCE_TOLERANCE: f64 = 1e-3;

/// The one prediction a single-record `text/csv` invocation answers, as the answer rendered it.
async fn served_field(
    router: &MultiServiceRouter,
    endpoint: &str,
    case: &str,
    record: &[f64],
) -> String {
    let body = payload(&[record.to_vec()], false);
    let answer = invoke(
        router,
        endpoint,
        &[("content-type", "text/csv")],
        body.as_bytes(),
    )
    .await;
    assert_eq!(
        answer.status,
        200,
        "case {case:?}: record {record:?} must be served, got {} with {}",
        answer.status,
        answer.text()
    );
    let declared = answer.header("content-type");
    assert_eq!(
        declared, CSV_ANSWER_CONTENT_TYPE,
        "case {case:?}: record {record:?} must be answered as comma-separated text, got \
         {declared:?}"
    );
    assert!(
        declared.to_ascii_lowercase().contains("charset=utf-8"),
        "case {case:?}: a textual answer names the character set it is written in, got \
         {declared:?}"
    );
    answer.text()
}

/// RULE: a rendering that keeps every digit of a prediction reports two records whose
/// predictions differ at all as two values — however little they differ.
#[tokio::test]
async fn two_records_a_hair_apart_are_rendered_as_two_values() {
    let router = emulator();
    let endpoint = "neighbour-endpoint";
    given_endpoint(&router, "the neighbouring model", "neighbour-model", endpoint).await;

    let base = [0.5, 1.25, 2.75];
    let base_field = served_field(&router, endpoint, "the unshifted record", &base).await;
    let base_read: f64 = base_field
        .parse()
        .unwrap_or_else(|error| panic!("{base_field:?} is no number: {error}"));

    for (case, shift) in NEIGHBOURS {
        let mut shifted = base;
        shifted[0] += shift;
        let field = served_field(&router, endpoint, case, &shifted).await;
        assert_ne!(
            field, base_field,
            "case {case:?}: shifting a feature by {shift} moves the prediction, so the two \
             answers cannot be the same text, got {field:?} twice"
        );
        let read: f64 = field
            .parse()
            .unwrap_or_else(|error| panic!("case {case:?}: {field:?} is no number: {error}"));
        // The booster is linear, so shifting the first feature moves the prediction by that
        // feature's own coefficient times the shift — and the rendered answers differ by it.
        let wanted = COEFFICIENTS[0] * shift;
        let observed = read - base_read;
        assert!(
            (observed - wanted).abs() <= wanted.abs() * DIFFERENCE_TOLERANCE,
            "case {case:?}: shifting the first feature by {shift} must move the prediction by \
             {wanted}, and the answers {base_field:?} and {field:?} differ by {observed}"
        );
    }
}
