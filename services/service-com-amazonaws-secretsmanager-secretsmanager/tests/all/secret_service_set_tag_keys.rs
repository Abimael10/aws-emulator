// SPDX-License-Identifier: CC0-1.0

//! The OTHER half of the reserved tag-key rule: the tags AWS sets from inside itself.
//!
//! RULE: a tag key in the reserved `aws:` namespace is AWS's own — so a request a CLIENT
//! makes may neither attach nor remove one
//! (`tests/all/secret_refusal_classes_and_sentences.rs` pins that half), while the service
//! that owns such a tag sets it through no public API at all. In this emulator one service
//! reaches another only over the in-process callback, so a request that arrives on that path
//! may carry a reserved key, and every later read of the secret reports it in `Tags` beside
//! the client's own. That is what lets a secret an `AWS::SecretsManager::Secret` declaration
//! provisions carry the three `aws:cloudformation:*` records CloudFormation keeps about every
//! resource it provisions — the secret-to-stack reverse lookup a client holding only a secret
//! id reads.
//!
//! The rows differ along what the rule generalises over: the reserved NAMESPACE the key sits
//! in (the stack provenance a CloudFormation deployment stamps, and another service's own
//! namespace), the bare prefix as the whole key, a key that merely CONTAINS the prefix without
//! being in the namespace, and a client's own key. Every row is driven on both paths and
//! through both tag-writing calls, so each one pins the refusal and the admission with one
//! expectation.
//!
//! Every case drives real HTTP through the production front door
//! (`MultiServiceRouter::dispatch`); the in-process path is the one the framework's callback
//! transport stamps (`smithy_server_runtime::CallbackRecursionDepth`), which is a request
//! EXTENSION and therefore cannot arrive from a socket.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_secretsmanager_secretsmanager::state::system_tag_message;
use service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService;
use smithy_server_runtime::{CallbackRecursionDepth, MultiServiceRouter};

/// The Region every request below signs for.
const REGION: &str = "us-west-2";

/// The wire error code this service refuses a tag rule with
/// (`InvalidParameterException`, its own modelled refusal of a bad member value).
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// A version id of the modelled length (`SecretVersionIdType` is 32..64): a create that
/// stores a value stakes a version the caller has to name.
const VERSION_ID: &str = "tag-origin-version-0000000000000";

/// The value every row writes: the rule is about the KEY, so one value serves every row.
const VALUE: &str = "probe-value";

/// A key a client owns, written beside the row's key so each case also shows the other half
/// of the request travelled.
const CLIENT_KEY: &str = "team";
const CLIENT_VALUE: &str = "platform";

/// Which path a request arrives on, which is what decides whether a reserved tag key is
/// admitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Path {
    /// A client's own request, arriving from a socket: it carries no in-process stamp.
    Caller,
    /// A request another emulated service made in-process, which the framework's callback
    /// transport stamps.
    InProcess,
}

/// A fresh emulator holding only this service, over its own engine.
fn emulator() -> MultiServiceRouter {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(EmulatedService::new(engine));
    router
}

/// Send one `awsJson1_1` request on `path`, and return its status and JSON body.
///
/// The same request either way but for the stamp: an in-process call carries the
/// [`CallbackRecursionDepth`] extension the callback transport puts on it, which is the one
/// thing about a request a client cannot choose for itself.
async fn call_on(
    router: &MultiServiceRouter,
    path: Path,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("secretsmanager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/secretsmanager/\
                 aws4_request, SignedHeaders=host;x-amz-date, Signature=deadbeef"
            ),
        );
    if path == Path::InProcess {
        request = request.extension(CallbackRecursionDepth(1));
    }
    let payload = bytes::Bytes::from(
        serde_json::to_vec(body).expect("the request body is serializable"),
    );
    let response = router
        .dispatch(request.body(payload).expect("a valid request"))
        .await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The `__type` discriminator of an awsJson error body, without its namespace.
fn error_code(case: &str, parsed: &Value) -> String {
    let raw = parsed
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no __type in error body {parsed}"));
    raw.split('#').next_back().unwrap_or(raw).to_string()
}

/// The `Message` an error body carries.
fn message(parsed: &Value) -> String {
    parsed
        .get("Message")
        .or_else(|| parsed.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The tags `DescribeSecret` reports for `secret_id`, as the map a client reads them as.
async fn described_tags(
    router: &MultiServiceRouter,
    case: &str,
    secret_id: &str,
) -> BTreeMap<String, String> {
    let (status, answered) = call_on(
        router,
        Path::Caller,
        "DescribeSecret",
        &json!({"SecretId": secret_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeSecret({secret_id}) must answer 200, got {status} {answered}"
    );
    answered
        .get("Tags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    let key = tag.get("Key").and_then(Value::as_str)?;
                    Some((
                        key.to_string(),
                        tag.get("Value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One row: a tag key, and whether it is in the namespace AWS keeps for itself.
struct Row {
    case: &'static str,
    key: &'static str,
    /// Whether the key is in the reserved namespace, so a client's request may not name it
    /// and an in-process one may.
    reserved: bool,
}

/// The rows the rule generalises over.
///
/// The three stack-provenance keys are the ones CloudFormation records on every resource it
/// provisions; the Service Catalog key is another service's own namespace. The last three
/// rows are the boundary: the bare prefix is the namespace, a key that merely contains `aws:`
/// is not in it at all, and a client's own key never was.
const ROWS: &[Row] = &[
    Row {
        case: "the stack a resource belongs to",
        key: "aws:cloudformation:stack-name",
        reserved: true,
    },
    Row {
        case: "that stack's id",
        key: "aws:cloudformation:stack-id",
        reserved: true,
    },
    Row {
        case: "the resource's logical id in the template",
        key: "aws:cloudformation:logical-id",
        reserved: true,
    },
    Row {
        case: "another service's own namespace",
        key: "aws:servicecatalog:provisioningPrincipalArn",
        reserved: true,
    },
    Row {
        case: "the bare reserved prefix as the whole key",
        key: "aws:",
        reserved: true,
    },
    Row {
        case: "a key that merely contains the prefix",
        key: "not-aws:prefixed",
        reserved: false,
    },
    Row {
        case: "a client's own key",
        key: "env",
        reserved: false,
    },
];

/// The whole tag set a served write leaves on the secret.
fn expected_set(row: &Row) -> BTreeMap<String, String> {
    [
        (row.key.to_string(), VALUE.to_string()),
        (CLIENT_KEY.to_string(), CLIENT_VALUE.to_string()),
    ]
    .into_iter()
    .collect()
}

/// `CreateSecret$Tags`: a reserved key is refused on a client's create — which then creates
/// nothing — and stored on an in-process one, where `DescribeSecret` reports it in `Tags`
/// beside the client's own tag.
#[tokio::test]
async fn a_create_takes_a_reserved_tag_key_only_on_the_in_process_path() {
    for row in ROWS {
        let case = row.case;
        for path in [Path::Caller, Path::InProcess] {
            let admitted = !row.reserved || path == Path::InProcess;
            let router = emulator();
            let name = "tag-origin-created";
            let (status, answered) = call_on(
                &router,
                path,
                "CreateSecret",
                &json!({
                    "Name": name,
                    "ClientRequestToken": VERSION_ID,
                    "SecretString": "a-value",
                    "Tags": [
                        {"Key": row.key, "Value": VALUE},
                        {"Key": CLIENT_KEY, "Value": CLIENT_VALUE},
                    ],
                }),
            )
            .await;
            if admitted {
                assert_eq!(
                    status, 200,
                    "case {case:?} on {path:?}: CreateSecret must store the key, got {status} \
                     {answered}"
                );
                let held = described_tags(&router, case, name).await;
                let expected = expected_set(row);
                assert_eq!(
                    held, expected,
                    "case {case:?} on {path:?}: expected the whole set on the created secret, \
                     got {held:?}"
                );
            } else {
                assert_eq!(
                    (status, error_code(case, &answered).as_str()),
                    (400, INVALID_PARAMETER),
                    "case {case:?} on {path:?}: CreateSecret must refuse the key, got {status} \
                     {answered}"
                );
                let expected = system_tag_message(row.key);
                assert_eq!(
                    message(&answered),
                    expected,
                    "case {case:?} on {path:?}: expected the refusal to state {expected:?}, got \
                     {answered}"
                );
                let (status, refused) = call_on(
                    &router,
                    Path::Caller,
                    "DescribeSecret",
                    &json!({"SecretId": name}),
                )
                .await;
                assert_eq!(
                    (status, error_code(case, &refused).as_str()),
                    (400, "ResourceNotFoundException"),
                    "case {case:?} on {path:?}: a refused create must create nothing, got \
                     {status} {refused}"
                );
            }
        }
    }
}

/// `TagResource` and `UntagResource` on a secret that already exists: the same rule, on the
/// calls that ATTACH a tag to one and REMOVE it again.
#[tokio::test]
async fn an_attach_and_a_detach_take_a_reserved_tag_key_only_on_the_in_process_path() {
    for row in ROWS {
        let case = row.case;
        for path in [Path::Caller, Path::InProcess] {
            let admitted = !row.reserved || path == Path::InProcess;
            let router = emulator();
            let name = "tag-origin-attached";
            let (status, created) = call_on(
                &router,
                Path::Caller,
                "CreateSecret",
                &json!({
                    "Name": name,
                    "ClientRequestToken": VERSION_ID,
                    "SecretString": "a-value",
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} on {path:?}: the secret under test must be created, got {status} \
                 {created}"
            );

            let (status, attached) = call_on(
                &router,
                path,
                "TagResource",
                &json!({
                    "SecretId": name,
                    "Tags": [
                        {"Key": row.key, "Value": VALUE},
                        {"Key": CLIENT_KEY, "Value": CLIENT_VALUE},
                    ],
                }),
            )
            .await;
            if !admitted {
                assert_eq!(
                    (status, error_code(case, &attached).as_str()),
                    (400, INVALID_PARAMETER),
                    "case {case:?} on {path:?}: TagResource must refuse the key, got {status} \
                     {attached}"
                );
                let expected = system_tag_message(row.key);
                assert_eq!(
                    message(&attached),
                    expected,
                    "case {case:?} on {path:?}: expected the refusal to state {expected:?}, got \
                     {attached}"
                );
                let held = described_tags(&router, case, name).await;
                assert!(
                    held.is_empty(),
                    "case {case:?} on {path:?}: a refused attach must attach nothing, got \
                     {held:?}"
                );
                // The REMOVAL half of the same rule: a client may not take a reserved key off
                // either, so the detach is refused the same way.
                let (status, detached) = call_on(
                    &router,
                    path,
                    "UntagResource",
                    &json!({"SecretId": name, "TagKeys": [row.key]}),
                )
                .await;
                assert_eq!(
                    (status, error_code(case, &detached).as_str()),
                    (400, INVALID_PARAMETER),
                    "case {case:?} on {path:?}: UntagResource must refuse the key, got {status} \
                     {detached}"
                );
                continue;
            }

            assert_eq!(
                status, 200,
                "case {case:?} on {path:?}: TagResource must store the key, got {status} \
                 {attached}"
            );
            let held = described_tags(&router, case, name).await;
            let expected = expected_set(row);
            assert_eq!(
                held, expected,
                "case {case:?} on {path:?}: expected the whole set on the tagged secret, got \
                 {held:?}"
            );

            let (status, detached) = call_on(
                &router,
                path,
                "UntagResource",
                &json!({"SecretId": name, "TagKeys": [row.key]}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?} on {path:?}: UntagResource must remove the key, got {status} \
                 {detached}"
            );
            let held = described_tags(&router, case, name).await;
            let expected: BTreeMap<String, String> =
                [(CLIENT_KEY.to_string(), CLIENT_VALUE.to_string())]
                    .into_iter()
                    .collect();
            assert_eq!(
                held, expected,
                "case {case:?} on {path:?}: expected only the client's own key to remain, got \
                 {held:?}"
            );
        }
    }
}
