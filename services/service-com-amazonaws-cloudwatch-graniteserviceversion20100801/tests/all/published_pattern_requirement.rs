// SPDX-License-Identifier: CC0-1.0

//! The EXPRESSION a constraint refusal of this service quotes for a member's modelled
//! `@pattern`.
//!
//! The rule under test: **a refusal of a value against a modelled `@pattern` states the
//! expression the WHOLE value must satisfy, deciding the two ends independently — an end the
//! model ANCHORS carries its `^` / `$` away with it, because the anchor is how a Smithy
//! `@pattern` (a FIND) is made total at that end rather than part of the grammar a caller has
//! to satisfy, while an end the model leaves OPEN admits any run of characters and is spelled
//! with one (`.*`).** The requirement belongs to the member's SHAPE, so every member targeting
//! one shape quotes the same expression whichever operation carried the value and at whatever
//! depth of the request it sat; the rest of the refusal — the envelope, the clause, the member
//! path and the class — is the request-validation layer's.
//!
//! The rows differ along what the rule generalises over: WHICH end the model anchors (both ends
//! for the dataset identifier and the KMS key ARN, the opening one alone for a metric
//! namespace), which member and which shape, which operation carried the value, the DEPTH the
//! value sat at (a top-level member, and a member of a structure inside a list, at two
//! positions of two different lists), and the value itself — which is echoed, so each row
//! states its own.

use serde_json::{json, Value};

use crate::harness::{call, call_in_query_mode, build_router, error_code, error_message};

/// The clause a constraint refusal states a `@pattern` requirement in, as the framework words
/// it.
const PATTERN_CLAUSE: &str = "Member must satisfy regular expression pattern: ";

/// The error SHAPE the body of a request-validation refusal names, and the legacy query code
/// the `x-amzn-query-error` header reports it under for a client that asked for the report.
const VALIDATION_SHAPE: &str = "ValidationException";
const VALIDATION_QUERY_CODE: &str = "ValidationError;Sender";

/// The header that report travels in.
const QUERY_ERROR_HEADER: &str = "x-amzn-query-error";

/// The `@pattern` of `Namespace`, the shape every metric NAMESPACE member targets, as the
/// model declares it: anchored at its OPENING end only
/// (`smithy_query(query="shape",shape="Namespace")`).
const NAMESPACE_PATTERN: &str = "^[^:]";

/// The `@pattern` of the dataset identifier the dataset operations take, as the model declares
/// it: anchored at both ends
/// (`smithy_query(query="operation",operation="GetDataset,AssociateDatasetKmsKey")`).
const DATASET_IDENTIFIER_PATTERN: &str =
    r"^(default|arn:[a-zA-Z0-9-]+:cloudwatch:[a-zA-Z0-9-]*:\d{12}:dataset/default)$";

/// The `@pattern` of the KMS key ARN those operations take, as the model declares it: anchored
/// at both ends (the same `smithy_query` call).
const KMS_KEY_ARN_PATTERN: &str = r"^arn:[a-zA-Z0-9-]+:kms:[a-zA-Z0-9-]+:\d{12}:key/[a-f0-9-]+$";

/// A KMS key ARN the key member's own pattern admits, so a row about the dataset identifier is
/// refused for the identifier rather than for the key beside it.
const WELL_FORMED_KEY: &str =
    "arn:aws:kms:us-east-1:000000000000:key/1a2b3c4d-5e6f-4a7b-8c9d-0e1f2a3b4c5d";

/// The expression a refusal quotes for the declared pattern `declared`: the expression the
/// WHOLE value must satisfy, with each end decided on its own ([the module docs](self)).
fn whole_value_expression(declared: &str) -> String {
    let (body, anchored_at_start) = match declared.strip_prefix('^') {
        Some(rest) => (rest, true),
        None => (declared, false),
    };
    let (body, anchored_at_end) = match body.strip_suffix('$') {
        Some(inner) => (inner, true),
        None => (body, false),
    };
    let open = |anchored: bool| if anchored { "" } else { ".*" };
    format!(
        "{}{body}{}",
        open(anchored_at_start),
        open(anchored_at_end)
    )
}

/// One row: the call, the member the clause names, the value it echoes and the pattern that
/// member's shape declares.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The operation the request is sent to.
    operation: &'static str,
    /// The request itself.
    request: Value,
    /// The member path the clause names, as the validating walk spells one.
    member: &'static str,
    /// The value the clause echoes.
    submitted: &'static str,
    /// The `@pattern` the member's shape declares.
    declared: &'static str,
}

#[tokio::test]
async fn a_pattern_requirement_states_the_expression_the_whole_value_must_satisfy() {
    // The published rendering of the namespace pattern, pinned on its own: the model's
    // expression is anchored at one end only, so the quoted one is neither the model's text
    // nor that text with both anchors dropped.
    assert_eq!(
        whole_value_expression(NAMESPACE_PATTERN),
        "[^:].*",
        "the namespace requirement must be quoted as the expression the whole value satisfies"
    );

    let rows: Vec<Row> = vec![
        Row {
            case: "the namespace a metric's alarms are looked up by",
            operation: "DescribeAlarmsForMetric",
            request: json!({"Namespace": ":Svc/Api", "MetricName": "Requests"}),
            member: "namespace",
            submitted: ":Svc/Api",
            declared: NAMESPACE_PATTERN,
        },
        Row {
            case: "a namespace of the forbidden character alone",
            operation: "DescribeAlarmsForMetric",
            request: json!({"Namespace": ":", "MetricName": "Requests"}),
            member: "namespace",
            submitted: ":",
            declared: NAMESPACE_PATTERN,
        },
        Row {
            case: "the namespace of an enrichment filter being stored",
            operation: "StartOTelEnrichment",
            request: json!({"IncludeFilters": [{"Namespace": ":Svc"}]}),
            member: "includeFilters.1.member.namespace",
            submitted: ":Svc",
            declared: NAMESPACE_PATTERN,
        },
        Row {
            case: "the namespace of a filter the enrichment update replaces with",
            operation: "UpdateOTelEnrichment",
            request: json!({"IncludeFilters": [{"Namespace": ":AWS"}]}),
            member: "includeFilters.1.member.namespace",
            submitted: ":AWS",
            declared: NAMESPACE_PATTERN,
        },
        Row {
            case: "the namespace of the SECOND filter of the other list",
            operation: "UpdateOTelEnrichment",
            request: json!({
                "ExcludeFilters": [{"Namespace": "Svc/Fine"}, {"Namespace": ":Svc/Bad"}],
            }),
            member: "excludeFilters.2.member.namespace",
            submitted: ":Svc/Bad",
            declared: NAMESPACE_PATTERN,
        },
        Row {
            case: "a dataset identifier that is a bare name",
            operation: "GetDataset",
            request: json!({"DatasetIdentifier": "my-dataset"}),
            member: "datasetIdentifier",
            submitted: "my-dataset",
            declared: DATASET_IDENTIFIER_PATTERN,
        },
        Row {
            case: "a KMS key ARN of the wrong form",
            operation: "AssociateDatasetKmsKey",
            request: json!({
                "DatasetIdentifier": "default",
                "KmsKeyArn": "arn:aws:kms:us-east-1:000000000000:alias/not-a-key-id",
            }),
            member: "kmsKeyArn",
            submitted: "arn:aws:kms:us-east-1:000000000000:alias/not-a-key-id",
            declared: KMS_KEY_ARN_PATTERN,
        },
        Row {
            case: "a dataset identifier naming another resource, beside a well-formed key",
            operation: "AssociateDatasetKmsKey",
            request: json!({
                "DatasetIdentifier": "arn:aws:cloudwatch:us-east-1:000000000000:dataset/other",
                "KmsKeyArn": WELL_FORMED_KEY,
            }),
            member: "datasetIdentifier",
            submitted: "arn:aws:cloudwatch:us-east-1:000000000000:dataset/other",
            declared: DATASET_IDENTIFIER_PATTERN,
        },
    ];

    let router = build_router();
    for Row {
        case,
        operation,
        request,
        member,
        submitted,
        declared,
    } in rows
    {
        let (status, body) = call(&router, operation, &request).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, VALIDATION_SHAPE),
            "case {case:?}: expected HTTP 400 {VALIDATION_SHAPE}, got {status} with {body}"
        );
        let expected = format!(
            "1 validation error detected: Value '{submitted}' at '{member}' failed to satisfy \
             constraint: {PATTERN_CLAUSE}{}",
            whole_value_expression(declared)
        );
        let stated = error_message(&body);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected the refusal to read {expected:?}, got {stated:?}"
        );
        // Stated separately, because the rendering is the whole point: whatever else the clause
        // carries, the expression it quotes is not the model's own text.
        for quoted in stated.split(PATTERN_CLAUSE).skip(1) {
            assert!(
                !quoted.starts_with('^') && !quoted.ends_with('$'),
                "case {case:?}: the quoted expression must carry no anchor of the model's, got \
                 {quoted:?}"
            );
        }
    }
}

#[tokio::test]
async fn such_a_refusal_reports_the_request_validation_class_to_a_query_client() {
    // The class is the request-validation layer's whichever vocabulary the client speaks: the
    // shape id in the body, and the legacy query code in the header a query-compatible SDK
    // opts into.
    let router = build_router();
    let (status, headers, body) = call_in_query_mode(
        &router,
        "DescribeAlarmsForMetric",
        &json!({"Namespace": ":Svc", "MetricName": "Requests"}),
    )
    .await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (400, VALIDATION_SHAPE),
        "expected HTTP 400 {VALIDATION_SHAPE}, got {status} with {body}"
    );
    let reported = headers
        .get(QUERY_ERROR_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    assert_eq!(
        reported, VALIDATION_QUERY_CODE,
        "expected {QUERY_ERROR_HEADER} to read {VALIDATION_QUERY_CODE:?}, got {reported:?}"
    );
}
