// SPDX-License-Identifier: CC0-1.0

//! What this service writes in the `x-amzn-errortype` response header of a failure.
//!
//! The rule these suites assert: **a failure of this service names the error shape it is
//! refused with in `x-amzn-errortype` by that shape's NAME — the name the model declares
//! the error shape under — and the header carries nothing else**: no namespace before it,
//! and nothing appended after it, in particular no locator naming where a serving stack
//! declares the shape. A restJson1 client resolves the error shape from this header, so
//! the value is the shape name and the same shape the body names in `__type`; a success
//! names no error shape at all.
//!
//! The model declares nothing about this header (the service only `@cors`-exposes it), so
//! the rule above is the protocol's: restJson1 resolves an error shape from the header's
//! value, and the shape names available are exactly the error shapes this model declares
//! (`smithy_query(query="shape",
//! shape="ResourceNotFoundException,ValidationException,ConflictException")`).
//!
//! The rule generalises over the operation, over the error shape and over WHAT answered
//! the failure — a handler's modelled refusal, a service-rule refusal, a refusal the
//! framework raised before any handler ran, and a request naming no modelled operation at
//! all — so the table below spans all of those rather than the one recorded refusal. The
//! recorded cases, a `TagResource` and an `UntagResource` against an ARN no resource
//! carries, are two of its rows.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The region the suites sign with.
const REGION: &str = "eu-west-2";

/// The canonical placeholder account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The header a restJson1 client resolves an error shape from.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// A SigV4 `Authorization` header for this service in [`REGION`].
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/docdb-elastic/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=deadbeef"
    )
}

/// One emulator: the shared engine plus a router carrying only this service.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_docdbelastic_chimeradblionfishservicelambda::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Percent-encode one value the way an AWS SDK encodes a single path segment or one
/// query-parameter value: an ARN carries both `:` and `/`.
fn label(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

/// Send one request and return its status, the `x-amzn-errortype` header and its parsed
/// JSON body (`Value::Null` when the body is empty).
async fn answer(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Option<String>, Value) {
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://docdb-elastic.{REGION}.amazonaws.com{path}"))
        .header("host", format!("docdb-elastic.{REGION}.amazonaws.com"))
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get(ERROR_TYPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            panic!(
                "{method} {path}: body is not JSON ({e}): {}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, error_type, parsed)
}

/// The error shape a body names in `__type`, read whether the renderer wrote it bare or
/// namespaced.
fn error_code(body: &Value) -> String {
    let raw = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>");
    raw.rsplit('#').next().unwrap_or(raw).to_string()
}

/// A `CreateCluster` body with every required member and nothing else.
fn minimal_create(name: &str) -> Value {
    json!({
        "clusterName": name,
        "authType": "PLAIN_TEXT",
        "adminUserName": "admin",
        "adminUserPassword": "Secret123!",
        "shardCapacity": 2,
        "shardCount": 1,
    })
}

/// Create a cluster that must succeed and return its ARN.
async fn create_arn(router: &MultiServiceRouter, name: &str) -> String {
    let (status, _, response) = answer(router, "POST", "/cluster", Some(minimal_create(name))).await;
    assert_eq!(
        status, 200,
        "case {name:?}: the fixture cluster must be created, got {status} with {response}"
    );
    response["cluster"]["clusterArn"]
        .as_str()
        .expect("the created cluster carries its ARN")
        .to_string()
}

/// Assert `named` is a shape NAME and nothing else, and return it.
///
/// A name is what the model declares an error shape under: letters and digits, no
/// namespace before it and nothing appended after it — so none of the characters a
/// namespace (`#`), a locator (`:`, `/`) or a URL (`.`, whitespace) would bring.
fn shape_name_only(case: &str, named: &Option<String>) -> String {
    let named = named
        .clone()
        .unwrap_or_else(|| panic!("case {case:?}: a failure must name its error shape in {ERROR_TYPE_HEADER}"));
    assert!(
        !named.is_empty(),
        "case {case:?}: the named shape must not be empty"
    );
    for forbidden in ['#', ':', '/', '.', ' '] {
        assert!(
            !named.contains(forbidden),
            "case {case:?}: the header must carry the shape name alone, found {forbidden:?} \
             in {named:?}"
        );
    }
    assert!(
        named.chars().all(|c| c.is_ascii_alphanumeric()),
        "case {case:?}: a shape name is alphanumeric, got {named:?}"
    );
    named
}

/// One failure to drive, and the error shape the model says answers it.
struct Failure {
    case: &'static str,
    method: &'static str,
    path: String,
    body: Option<Value>,
    status: u16,
    /// The shape the model declares for this refusal, or `None` where the row asserts the
    /// FORM of whatever shape the framework chose rather than which shape that is.
    shape: Option<&'static str>,
}

#[tokio::test]
async fn every_failure_names_its_error_shape_by_name_alone() {
    let router = emulator();
    let held = create_arn(&router, "named").await;
    let absent_cluster = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster/00000000-0000-0000-0000-000000000000"
    );
    let absent_snapshot = format!(
        "arn:aws:docdb-elastic:{REGION}:{ACCOUNT}:cluster-snapshot/11111111-2222-3333-4444-555555555555"
    );

    // The rows differ along the operation, the HTTP method, the status, the error shape
    // and what answered the failure.
    let failures: Vec<Failure> = vec![
        Failure {
            case: "TagResource on an ARN no resource carries (404)",
            method: "POST",
            path: format!("/tags/{}", label(&absent_cluster)),
            body: Some(json!({"tags": {"Project": "lionfish"}})),
            status: 404,
            shape: Some("ResourceNotFoundException"),
        },
        Failure {
            case: "UntagResource on an ARN no resource carries (404)",
            method: "DELETE",
            path: format!("/tags/{}?tagKeys=Project", label(&absent_cluster)),
            body: None,
            status: 404,
            shape: Some("ResourceNotFoundException"),
        },
        Failure {
            case: "ListTagsForResource on an ARN no resource carries (404)",
            method: "GET",
            path: format!("/tags/{}", label(&absent_cluster)),
            body: None,
            status: 404,
            shape: Some("ResourceNotFoundException"),
        },
        Failure {
            case: "GetCluster of an absent cluster (404)",
            method: "GET",
            path: format!("/cluster/{}", label(&absent_cluster)),
            body: None,
            status: 404,
            shape: Some("ResourceNotFoundException"),
        },
        Failure {
            case: "DeleteCluster of an absent cluster (404)",
            method: "DELETE",
            path: format!("/cluster/{}", label(&absent_cluster)),
            body: None,
            status: 404,
            shape: Some("ResourceNotFoundException"),
        },
        Failure {
            case: "GetClusterSnapshot of an absent snapshot (404)",
            method: "GET",
            path: format!("/cluster-snapshot/{}", label(&absent_snapshot)),
            body: None,
            status: 404,
            shape: Some("ResourceNotFoundException"),
        },
        Failure {
            case: "CreateCluster with a name already in use (409)",
            method: "POST",
            path: "/cluster".to_string(),
            body: Some(minimal_create("named")),
            status: 409,
            shape: Some("ConflictException"),
        },
        Failure {
            case: "CreateCluster with a shardCapacity outside its domain (400)",
            method: "POST",
            path: "/cluster".to_string(),
            body: {
                let mut body = minimal_create("refused");
                body["shardCapacity"] = json!(3);
                Some(body)
            },
            status: 400,
            shape: Some("ValidationException"),
        },
        Failure {
            case: "ListClusters with a nextToken this service never issued (400)",
            method: "GET",
            path: "/clusters?nextToken=not-a-token".to_string(),
            body: None,
            status: 400,
            shape: Some("ValidationException"),
        },
        Failure {
            case: "UpdateCluster of an absent cluster (404)",
            method: "PUT",
            path: format!("/cluster/{}", label(&absent_cluster)),
            body: Some(json!({"shardCount": 2})),
            status: 404,
            shape: Some("ResourceNotFoundException"),
        },
        Failure {
            // Raised by the FRAMEWORK, before any handler ran: `tags` is @required on
            // TagResource (`smithy_query(query="operation", operation="TagResource")`).
            case: "TagResource omitting a required member (400)",
            method: "POST",
            path: format!("/tags/{}", label(&held)),
            body: Some(json!({})),
            status: 400,
            shape: None,
        },
    ];

    let mut named_shapes = BTreeSet::new();
    for failure in failures {
        let case = failure.case;
        let (status, error_type, body) =
            answer(&router, failure.method, &failure.path, failure.body).await;
        assert_eq!(
            status, failure.status,
            "case {case:?}: expected HTTP {}, got {status} with {body}",
            failure.status
        );
        let named = shape_name_only(case, &error_type);
        if let Some(expected) = failure.shape {
            assert_eq!(
                named, expected,
                "case {case:?}: expected the shape {expected:?}, got {named:?}"
            );
        }
        // The header and the body name ONE shape: a client that reads either resolves the
        // same error.
        assert_eq!(
            named,
            error_code(&body),
            "case {case:?}: the header and the body must name one shape, got {named:?} and \
             {body}"
        );
        named_shapes.insert(named);
    }

    // Every shape the table exercised came back as its own name, so the form is not keyed
    // to one error class.
    assert!(
        named_shapes.contains("ResourceNotFoundException")
            && named_shapes.contains("ConflictException")
            && named_shapes.contains("ValidationException"),
        "the table must span several error shapes, got {named_shapes:?}"
    );
}

#[tokio::test]
async fn a_success_names_no_error_shape() {
    // The scope of the rule: the header belongs to a failure. A success — a create, a
    // read, a list, a tag write — names no error shape at all.
    let router = emulator();
    let arn = create_arn(&router, "successes").await;

    let successes: Vec<(&str, &str, String, Option<Value>)> = vec![
        ("GetCluster", "GET", format!("/cluster/{}", label(&arn)), None),
        ("ListClusters", "GET", "/clusters".to_string(), None),
        (
            "TagResource",
            "POST",
            format!("/tags/{}", label(&arn)),
            Some(json!({"tags": {"Project": "lionfish"}})),
        ),
        ("ListTagsForResource", "GET", format!("/tags/{}", label(&arn)), None),
        (
            "UntagResource",
            "DELETE",
            format!("/tags/{}?tagKeys=Project", label(&arn)),
            None,
        ),
    ];

    for (case, method, path, body) in successes {
        let (status, error_type, answered) = answer(&router, method, &path, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {answered}"
        );
        assert_eq!(
            error_type, None,
            "case {case:?}: a success names no error shape, got {error_type:?}"
        );
    }
}

#[tokio::test]
async fn a_request_naming_no_modeled_operation_names_no_shape_beside_its_name() {
    // The other layer of the service: a request the routing table cannot name is answered
    // by the framework's routing-failure renderer, which this service's response path also
    // passes through. Whatever shape that names, it names it the same way.
    let router = emulator();

    for (case, method, path) in [
        ("a path no operation is bound to", "GET", "/not-an-operation"),
        ("a method no operation is bound to", "PATCH", "/clusters"),
    ] {
        let (status, error_type, body) = answer(&router, method, path, None).await;
        assert!(
            status >= 400,
            "case {case:?}: a request naming no operation is a failure, got {status} with \
             {body}"
        );
        if error_type.is_some() {
            let named = shape_name_only(case, &error_type);
            assert!(
                !named.is_empty(),
                "case {case:?}: the named shape must not be empty, got {named:?}"
            );
        }
    }
}
