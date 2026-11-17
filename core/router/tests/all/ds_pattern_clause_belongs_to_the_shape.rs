// SPDX-License-Identifier: CC0-1.0

//! The expression a Directory Service constraint clause QUOTES belongs to the SHAPE the
//! refused member targets, not to the operation that carried the value.
//!
//! RULE: a value a member's `@pattern` does not admit is refused HTTP 400
//! `ValidationException`, and the requirement clause quotes the expression this service
//! publishes for the member's SHAPE — for an id space of this model, the model's `@pattern`
//! text verbatim, the `^` and `$` it wraps the grammar in included. The expression therefore
//! belongs to the shape: every member targeting it, in whatever operation, is answered with
//! one and the same text.
//!
//! What this suite adds to the ds crate's own `published_pattern_text` and
//! `validation_pattern_wording` suites — which assert the text a clause quotes, one operation
//! per shape — is the INVARIANCE across operations, which only exists between them: each
//! shape below is reached through several operations, the expression is read back out of the
//! wire message, and the set collected per shape is asserted to hold exactly one entry. A
//! rendering that differed between two operations, or that dropped an anchor on one of them,
//! fails here and nowhere else.
//!
//! The model's expressions are named on the constants below, each from
//! `smithy_query(query="shape",shape="<Shape>",model_path="services/service-com-amazonaws-directoryservice-directoryservice-20150416/smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json")`.
//!
//! The rows differ along what the rule generalises over: the operation, the member path the
//! value arrived under, how the value is malformed (a foreign prefix, non-hex digits, the
//! wrong width, the wrong case, an unrelated form), and whether another constrained member
//! sits beside the broken one.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The `X-Amz-Target` prefix Directory Service names an operation under: its service shape's
/// name.
const DS_TARGET_PREFIX: &str = "DirectoryService_20150416";

/// The expression the model declares for `CertificateId`, which a clause about a certificate
/// id publishes.
const CERTIFICATE_ID_PATTERN: &str = r"^c-[0-9a-f]{10}$";

/// The expression the model declares for `AssessmentId`.
const ASSESSMENT_ID_PATTERN: &str = r"^da-[0-9a-f]{18}$";

/// The expression the model declares for `DirectoryId`.
const DIRECTORY_ID_PATTERN: &str = r"^d-[0-9a-f]{10}$";

/// A `DirectoryId` the expression above admits and this account never creates, so a row whose
/// broken member is a different one is decided by that member alone.
const ADMITTED_DIRECTORY_ID: &str = "d-0123456789";

/// A `CertificateId` the expression above admits, for the row whose broken member is the
/// directory id beside it.
const ADMITTED_CERTIFICATE_ID: &str = "c-00112233aa";

/// A `SecretArn` the modelled
/// `^arn:aws:secretsmanager:[a-z0-9-]+:\d{12}:secret:[a-zA-Z0-9/_+=.@-]+-[a-zA-Z0-9]{6}$`
/// admits, so `CreateHybridAD` is refused for its assessment id and nothing else.
const ADMITTED_SECRET_ARN: &str =
    "arn:aws:secretsmanager:us-west-2:123456789012:secret:ds-hybrid-AbCdEf";

/// A `TopicName` and a `LogGroupName` their own expressions admit, for the rows whose broken
/// member is the directory id beside them.
const ADMITTED_TOPIC_NAME: &str = "sim2real-behav-topic";
const ADMITTED_LOG_GROUP_NAME: &str = "/aws/directoryservice/sim2real";

/// The marker a requirement clause states a regular expression after.
const PATTERN_REQUIREMENT: &str = "Member must satisfy regular expression pattern: ";

/// One row: the shape whose expression the clause must quote, the operation a refused value is
/// sent to, the member path the clause names it by, and the value that is refused.
struct Row {
    case: &'static str,
    shape: &'static str,
    published: &'static str,
    operation: &'static str,
    member: &'static str,
    value: &'static str,
    body: fn(&str) -> Value,
}

/// Every row: three shapes, each reached through several operations, with a differently
/// malformed value at each one.
fn rows() -> Vec<Row> {
    vec![
        // `CertificateId`, reached through both operations whose input carries one.
        Row {
            case: "a certificate id with the wrong prefix, on the certificate read",
            shape: "CertificateId",
            published: CERTIFICATE_ID_PATTERN,
            operation: "DescribeCertificate",
            member: "certificateId",
            value: "cert-00112233aa",
            body: |value| json!({ "DirectoryId": ADMITTED_DIRECTORY_ID, "CertificateId": value }),
        },
        Row {
            case: "a certificate id with non-hex digits, on the deregistration",
            shape: "CertificateId",
            published: CERTIFICATE_ID_PATTERN,
            operation: "DeregisterCertificate",
            member: "certificateId",
            value: "c-00112233zz",
            body: |value| json!({ "DirectoryId": ADMITTED_DIRECTORY_ID, "CertificateId": value }),
        },
        // `AssessmentId`, reached through the read, the delete and the create that names one.
        Row {
            case: "an assessment id too short for its id space, on the assessment read",
            shape: "AssessmentId",
            published: ASSESSMENT_ID_PATTERN,
            operation: "DescribeADAssessment",
            member: "assessmentId",
            value: "da-0123456789",
            body: |value| json!({ "AssessmentId": value }),
        },
        Row {
            case: "an assessment id with the wrong prefix, on the assessment delete",
            shape: "AssessmentId",
            published: ASSESSMENT_ID_PATTERN,
            operation: "DeleteADAssessment",
            member: "assessmentId",
            value: "d-0123456789abcdef01",
            body: |value| json!({ "AssessmentId": value }),
        },
        Row {
            case: "an assessment id with non-hex digits, on the hybrid create",
            shape: "AssessmentId",
            published: ASSESSMENT_ID_PATTERN,
            operation: "CreateHybridAD",
            member: "assessmentId",
            value: "da-0123456789abcdefzz",
            body: |value| {
                json!({ "SecretArn": ADMITTED_SECRET_ARN, "AssessmentId": value })
            },
        },
        // `DirectoryId`, reached through operations that differ in what else they carry: one
        // member alone, one beside another constrained id, and two beside a constrained name.
        Row {
            case: "a directory id of a foreign form, on the LDAPS read",
            shape: "DirectoryId",
            published: DIRECTORY_ID_PATTERN,
            operation: "DescribeLDAPSSettings",
            member: "directoryId",
            value: "not-a-directory-id",
            body: |value| json!({ "DirectoryId": value }),
        },
        Row {
            case: "an upper-case directory id, on the data-access read",
            shape: "DirectoryId",
            published: DIRECTORY_ID_PATTERN,
            operation: "DescribeDirectoryDataAccess",
            member: "directoryId",
            value: "D-0123456789",
            body: |value| json!({ "DirectoryId": value }),
        },
        Row {
            case: "a directory id beside a well-formed certificate id",
            shape: "DirectoryId",
            published: DIRECTORY_ID_PATTERN,
            operation: "DeregisterCertificate",
            member: "directoryId",
            value: "d-012345678",
            body: |value| {
                json!({ "DirectoryId": value, "CertificateId": ADMITTED_CERTIFICATE_ID })
            },
        },
        Row {
            case: "a directory id beside a well-formed topic name",
            shape: "DirectoryId",
            published: DIRECTORY_ID_PATTERN,
            operation: "RegisterEventTopic",
            member: "directoryId",
            value: "d-0123456789ab",
            body: |value| {
                json!({ "DirectoryId": value, "TopicName": ADMITTED_TOPIC_NAME })
            },
        },
        Row {
            case: "a directory id beside a well-formed log group name",
            shape: "DirectoryId",
            published: DIRECTORY_ID_PATTERN,
            operation: "CreateLogSubscription",
            member: "directoryId",
            value: "directory",
            body: |value| {
                json!({ "DirectoryId": value, "LogGroupName": ADMITTED_LOG_GROUP_NAME })
            },
        },
    ]
}

/// Directory Service alone on a fresh engine, reached the way the production router reaches
/// it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_directoryservice_directoryservice_20150416::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One Directory Service call: its status and its parsed body.
async fn ds(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{DS_TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/ds/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// RULE: the expression a clause quotes is the shape's, so every operation that carries a
/// value of that shape is answered with the same one — the model's text, anchors included.
#[tokio::test]
async fn a_pattern_clause_quotes_its_shapes_expression_whatever_operation_carried_it() {
    let router = emulator();

    let mut per_shape: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut reached: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for row in rows() {
        // The expression the model wraps in a matched pair describes the WHOLE value, which is
        // what makes quoting it intact the requirement a caller can act on.
        assert!(
            row.published.starts_with('^') && row.published.ends_with('$'),
            "case {:?}: expected the modelled expression to be anchored at both ends, got {:?}",
            row.case,
            row.published
        );
        let (status, answered) = ds(&router, row.operation, (row.body)(row.value)).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected the value to be refused, got {status} with {answered}",
            row.case
        );
        assert_eq!(
            answered["__type"],
            json!("ValidationException"),
            "case {:?}: expected the boundary's validation envelope, got {answered}",
            row.case
        );
        let message = answered["message"].as_str().unwrap_or_default();
        assert_eq!(
            message,
            format!(
                "1 validation error detected: Value '{}' at '{}' failed to satisfy constraint: \
                 {PATTERN_REQUIREMENT}{}",
                row.value, row.member, row.published
            ),
            "case {:?}: expected the published expression in the clause, got {message:?}",
            row.case
        );
        // Read back out of the sentence, so the invariance below is asserted on what the WIRE
        // carried rather than on the row's own literal.
        let (_, carried) = message.split_once(PATTERN_REQUIREMENT).unwrap_or_else(|| {
            panic!(
                "case {:?}: expected a pattern clause, got {message:?}",
                row.case
            )
        });
        per_shape
            .entry(row.shape)
            .or_default()
            .insert(carried.to_string());
        reached.entry(row.shape).or_default().insert(row.operation);
    }

    for (shape, quoted) in &per_shape {
        let operations = reached.get(shape).cloned().unwrap_or_default();
        assert!(
            operations.len() > 1,
            "case {shape:?}: the invariance is only asserted across operations, but only \
             {operations:?} reached it"
        );
        assert_eq!(
            quoted.len(),
            1,
            "case {shape:?}: expected one expression for the shape whatever operation of \
             {operations:?} carried its value, got {quoted:?}"
        );
    }
}
