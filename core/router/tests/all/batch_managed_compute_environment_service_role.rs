// SPDX-License-Identifier: CC0-1.0

//! The ROLE an AWS Batch managed compute environment runs as, read from BOTH sides on one
//! engine: the environment's own reader and the service that HOLDS the role.
//!
//! The RULE under test: **a MANAGED compute environment created with no `serviceRole` is
//! associated with this account's Batch service-linked role, and
//! `ComputeEnvironmentDetail$serviceRole` ("the service role that's associated with the
//! compute environment that allows Batch to make calls to Amazon Web Services API operations
//! on your behalf") reports that role's ARN — so a client that hands the ARN back to IAM finds
//! the role there.** `CreateComputeEnvironmentRequest$serviceRole` states the substitution ("if
//! your account already created the Batch service-linked role, that role is used by default for
//! your compute environment unless you specify a different role here... This automatic
//! service-linked role creation only applies to MANAGED compute environments. For UNMANAGED
//! compute environments, you must explicitly specify a serviceRole"), so an environment of any
//! other type is associated with the role the request named and with no role when it named
//! none.
//!
//! Sources:
//! `smithy_query(query="doc",shape="CreateComputeEnvironmentRequest",member="serviceRole")` and
//! `smithy_query(query="doc",shape="ComputeEnvironmentDetail",member="serviceRole")` for the
//! member under test, and IAM's `GetRole` for the reader the ARN is resolved through
//! (`Role$Arn`, `Role$Path`, `Role$RoleName`, `Role$AssumeRolePolicyDocument`).
//!
//! The rows differ along what the rule generalises over: the environment's type, the compute
//! type behind it, whether the request names a role of its own and in which form, and the
//! READER the role is read through (the environment named, and the unfiltered describe).
//!
//! What is NOT asserted here: the half that belongs to the owning crate's own suite —
//! `services/service-com-amazonaws-batch-awsbatchv20160810/tests/all/managed_compute_environment_service_role.rs`
//! spans every modelled compute type, the orchestration, and the update that replaces the role.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder that builds the form body of an awsQuery call and the reader that pulls
// one element's text out of the XML answer are the test crate's own, written once in
// `all/query_wire.rs`.
use crate::query_wire::{element, form_encode};

/// The Region every Batch request below is signed for.
const REGION: &str = "us-east-1";

/// The account this emulator mints its ARNs under.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The role name AWS Batch publishes for its service-linked role, spelled out here rather than
/// imported from either service crate, so a wrong constant in an implementation cannot make
/// this suite pass. Source: the name this service's own model states in
/// `UpdateComputeEnvironment` ("the AWSServiceRoleForBatch service-linked role").
const BATCH_SERVICE_LINKED_ROLE: &str = "AWSServiceRoleForBatch";

/// The service principal that role is linked to, which is also the path segment IAM files a
/// service-linked role under.
const BATCH_PRINCIPAL: &str = "batch.amazonaws.com";

/// AWS Batch and IAM over ONE shared engine, the way `core/router/src/lib.rs` mounts every
/// service: the service role of a compute environment is IAM's resource, so the ARN the
/// environment reports is answered for by that service's own reader. Each claims only requests
/// scoped to its own signing name, so no call below is routed any differently for the other's
/// presence.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_batch_awsbatchv20160810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `service`, which is what routes the request. The key id and
/// signature are not credentials: the router above runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One AWS Batch call: a `restJson1` service, so `POST <uri>` with a JSON body, which must
/// answer 200.
async fn batch(router: &MultiServiceRouter, case: &str, uri: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://batch.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("batch.{REGION}.amazonaws.com"))
        .header("authorization", authorization("batch"))
        .header("content-type", "application/json")
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&payload).into_owned()))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Batch {uri}, got {status} with {parsed}"
    );
    parsed
}

/// One IAM call: an `awsQuery` service, so a form body naming the action, answered with the
/// status and the XML text.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    (status, text)
}

/// The one record `DescribeComputeEnvironments` reports for `reference`.
async fn record_for(router: &MultiServiceRouter, case: &str, reference: &str) -> Value {
    let body = batch(
        router,
        case,
        "/v1/describecomputeenvironments",
        &json!({"computeEnvironments": [reference]}),
    )
    .await;
    let listed = body["computeEnvironments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected exactly one record for {reference}, got {body}"
    );
    listed[0].clone()
}

/// The compute resources a `MANAGED` environment is created with here.
fn managed_compute_resources(compute_type: &str) -> Value {
    json!({
        "type": compute_type,
        "maxvCpus": 4,
        "subnets": ["subnet-1111"],
        "securityGroupIds": ["sg-1111"],
    })
}

/// One row: the create request (everything but the name) and the role the environment must be
/// reported with — `None` meaning the member is absent.
struct Row {
    case: &'static str,
    create: Value,
    expected: Option<String>,
}

/// The ARN of this account's Batch service-linked role, assembled from the fixed parts: the
/// `aws` partition, IAM (global, so no Region), the reserved service-linked-role path under the
/// service principal, and the published role name.
fn service_linked_role_arn() -> String {
    format!(
        "arn:aws:iam::{ACCOUNT}:role/aws-service-role/{BATCH_PRINCIPAL}/{BATCH_SERVICE_LINKED_ROLE}"
    )
}

#[tokio::test]
async fn a_managed_compute_environment_reports_a_service_role_iam_resolves() {
    let substituted = service_linked_role_arn();
    let named = format!("arn:aws:iam::{ACCOUNT}:role/service-role/AWSBatchServiceRole");
    let rows = [
        Row {
            case: "MANAGED over Fargate, no serviceRole",
            create: json!({
                "type": "MANAGED",
                "state": "ENABLED",
                "computeResources": managed_compute_resources("FARGATE"),
            }),
            expected: Some(substituted.clone()),
        },
        Row {
            case: "MANAGED over EC2, no serviceRole",
            create: json!({
                "type": "MANAGED",
                "state": "ENABLED",
                "computeResources": managed_compute_resources("EC2"),
            }),
            expected: Some(substituted.clone()),
        },
        Row {
            case: "MANAGED over EC2, with a serviceRole of its own",
            create: json!({
                "type": "MANAGED",
                "state": "ENABLED",
                "computeResources": managed_compute_resources("EC2"),
                "serviceRole": named,
            }),
            expected: Some(named.clone()),
        },
        Row {
            case: "UNMANAGED, no serviceRole",
            create: json!({"type": "UNMANAGED", "state": "ENABLED"}),
            expected: None,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: what a row asserts is this environment's own state.
        let router = router();
        let name = format!("ce-service-role-{index}");
        let case = row.case;
        let mut request = row.create.clone();
        request["computeEnvironmentName"] = json!(name);
        batch(&router, case, "/v1/createcomputeenvironment", &request).await;

        let record = record_for(&router, case, &name).await;
        let reported = record.get("serviceRole").and_then(Value::as_str);
        assert_eq!(
            reported,
            row.expected.as_deref(),
            "case {case:?}: expected serviceRole {:?}, got {reported:?} in {record}",
            row.expected
        );
        // The unfiltered reader reports the same role: it is the environment's, not the
        // reader's.
        let unfiltered = batch(&router, case, "/v1/describecomputeenvironments", &json!({})).await;
        let listed = unfiltered["computeEnvironments"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .find(|held| held["computeEnvironmentName"].as_str() == Some(name.as_str()))
            .unwrap_or_else(|| panic!("case {case:?}: expected {name} in {unfiltered}"));
        assert_eq!(
            listed.get("serviceRole"),
            record.get("serviceRole"),
            "case {case:?}: every reader reports one role, got {listed}"
        );

        // The substituted role is IAM's resource, so IAM answers for it: the ARN the
        // environment reports resolves to the role AWS Batch's service-linked role IS.
        if reported == Some(substituted.as_str()) {
            let (status, answer) = iam(
                &router,
                "GetRole",
                &[("RoleName", BATCH_SERVICE_LINKED_ROLE)],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected IAM to hold {BATCH_SERVICE_LINKED_ROLE}, got {status} \
                 with {answer}"
            );
            assert_eq!(
                element(&answer, "Arn"),
                substituted,
                "case {case:?}: expected IAM to report the same ARN, got {answer}"
            );
            assert_eq!(
                element(&answer, "Path"),
                format!("/aws-service-role/{BATCH_PRINCIPAL}/"),
                "case {case:?}: expected the reserved service-linked-role path, got {answer}"
            );
            assert_eq!(
                element(&answer, "RoleName"),
                BATCH_SERVICE_LINKED_ROLE,
                "case {case:?}: expected the published role name, got {answer}"
            );
            // The role is linked to THIS service: its trust policy names that principal.
            let trust = element(&answer, "AssumeRolePolicyDocument");
            assert!(
                trust.contains(BATCH_PRINCIPAL),
                "case {case:?}: expected the trust policy to name {BATCH_PRINCIPAL}, got {trust}"
            );
        }
    }
}
