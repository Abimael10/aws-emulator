// SPDX-License-Identifier: CC0-1.0

//! Reading a resource-based policy that is NOT there, driven end to end over the wire path
//! (a `restJson1` request through the multi-service router into the mounted service), so
//! what is asserted is what a client sees.
//!
//! THE RULE UNDER TEST: a reporting operation answers for the resource it addresses, so a
//! resource this service holds that carries no document is REPORTED — HTTP 200 with the
//! optional output member simply absent — and never refused; only the operation whose whole
//! effect is to REMOVE the document is refused when there is none, in its own fixed sentence.
//! So a client probing "is there a policy?" can tell "no policy yet" from "no such resource",
//! which is a different refusal again.
//!
//! The table spans what the rule generalises over: the resource TYPE the ARN names (every
//! family that carries a policy plane, customer-created and service-provided), and the STAGE
//! the resource is at when the read happens (never written, written then removed, read twice
//! over). The three conditions a read can be in are kept apart in one place: a held resource
//! carrying nothing (answered), an ARN of a policy-carrying type naming no held resource
//! (404, worded for the ARN), and a type carrying no policy plane at all (400).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::state;
use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region every row signs for.
const REGION: &str = "us-east-1";

/// How the REMOVAL of a document that is not there is worded — the one condition in this
/// plane that the read does not share.
const POLICY_MISSING: &str = "Resource Policy doesn't exist for requested arn";

/// How every resource-policy operation words a `resourceArn` of a policy-carrying type that
/// names no resource this account holds.
const RESOURCE_MISSING: &str = "ResourceArn provided in the request does not exist";

/// How every resource-policy operation words a `resourceArn` whose resource type carries no
/// policy plane.
const UNSUPPORTED_TYPE: &str =
    "resourceArn: resource policies are not supported for this resource type";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The ARN form, percent-encoded as a client encodes a non-greedy `@httpLabel`.
fn resource_policy_uri(arn: &str) -> String {
    format!(
        "/resourcepolicy/{}",
        arn.replace(':', "%3A").replace('/', "%2F")
    )
}

async fn put_policy(router: &MultiServiceRouter, arn: &str, policy: &str) -> (u16, Value) {
    send(
        router,
        "PUT",
        &resource_policy_uri(arn),
        Some(&json!({"policy": policy})),
    )
    .await
}

async fn get_policy(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    send(router, "GET", &resource_policy_uri(arn), None).await
}

async fn delete_policy(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    send(router, "DELETE", &resource_policy_uri(arn), None).await
}

/// The error code `restJson1` puts in `__type` / `x-amzn-errortype`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A resource policy document granting `action` to the account's root principal.
fn policy_document(sid: &str, action: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": sid,
            "Effect": "Allow",
            "Principal": {"AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id())},
            "Action": action,
            "Resource": "*",
        }],
    })
    .to_string()
}

/// Create an agent runtime and answer its ARN.
async fn create_runtime(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "PUT",
        "/runtimes/",
        Some(&json!({
            "agentRuntimeName": name,
            "agentRuntimeArtifact": {"containerConfiguration": {
                "containerUri": "public.ecr.aws/agentcore/demo:v1"
            }},
            "roleArn": "arn:aws:iam::000000000000:role/AgentRole",
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: the agent runtime a read case addresses is created, got {status} ({created})"
    );
    text(&created, "agentRuntimeArn")
}

/// Create a gateway and answer its ARN.
async fn create_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "POST",
        "/gateways/",
        Some(&json!({
            "name": name,
            "roleArn": "arn:aws:iam::000000000000:role/GatewayRole",
            "authorizerType": "AWS_IAM",
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: the gateway a read case addresses is created, got {status} ({created})"
    );
    text(&created, "gatewayArn")
}

/// Create a browser of this account's own and answer its ARN.
async fn create_browser(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "PUT",
        "/browsers",
        Some(&json!({
            "name": name,
            "networkConfiguration": {"networkMode": "PUBLIC"},
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: the browser a read case addresses is created, got {status} ({created})"
    );
    text(&created, "browserArn")
}

/// Create a code interpreter of this account's own and answer its ARN.
async fn create_code_interpreter(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "PUT",
        "/code-interpreters",
        Some(&json!({
            "name": name,
            "networkConfiguration": {"networkMode": "SANDBOX"},
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: the code interpreter a read case addresses is created, got {status} ({created})"
    );
    text(&created, "codeInterpreterArn")
}

/// Every resource whose type carries a policy plane, as a `(case, ARN)` row. The four
/// customer-created families plus the two the service provides, each addressed under the ARN
/// type its own `*Arn` pattern publishes.
async fn policy_carrying_resources(router: &MultiServiceRouter) -> Vec<(&'static str, String)> {
    vec![
        (
            "an agent runtime",
            create_runtime(router, "absent_read_runtime").await,
        ),
        (
            "a gateway",
            create_gateway(router, "absentreadgateway").await,
        ),
        (
            "a browser",
            create_browser(router, "absentReadBrowser").await,
        ),
        (
            "a code interpreter",
            create_code_interpreter(router, "absentReadInterpreter").await,
        ),
        (
            "the service-provided browser",
            state::system_browser_arn(REGION),
        ),
        (
            "the service-provided code interpreter",
            state::system_code_interpreter_arn(REGION),
        ),
    ]
}

/// RULE: for a resource this service HOLDS that carries no document, the read answers 200
/// with its optional `policy` member absent — before anything was ever written, and again
/// after a written document was removed — whichever policy-carrying family the ARN names.
#[tokio::test]
async fn a_read_of_a_resource_carrying_no_policy_answers_with_no_policy_member() {
    let router = router();

    for (case, arn) in policy_carrying_resources(&router).await {
        // Stage 1: nothing was ever written for this resource.
        let (status, answer) = get_policy(&router, &arn).await;
        assert_eq!(
            (status, answer.get("policy").cloned()),
            (200, None),
            "case {case:?} (never written): expected HTTP 200 with no policy member, got \
             {status} ({answer})"
        );

        // Stage 2: the same read again — a report changes nothing a later caller sees.
        let (status, again) = get_policy(&router, &arn).await;
        assert_eq!(
            (status, again.get("policy").cloned()),
            (200, None),
            "case {case:?} (read twice over): expected the same answer, got {status} ({again})"
        );

        // Stage 3: a document is written, so the read reports exactly it.
        let document = policy_document("AbsentReadProbe", "bedrock-agentcore:InvokeAgentRuntime");
        let (status, written) = put_policy(&router, &arn, &document).await;
        assert_eq!(
            (status, text(&written, "policy")),
            (201, document.clone()),
            "case {case:?}: expected HTTP 201 echoing the submitted document, got {status} \
             ({written})"
        );
        let (status, read) = get_policy(&router, &arn).await;
        assert_eq!(
            (status, text(&read, "policy")),
            (200, document.clone()),
            "case {case:?}: expected the stored document back, got {status} ({read})"
        );

        // Stage 4: the document is removed, so the resource is back to carrying none — and
        // the read is back to answering rather than refusing.
        let (status, removed) = delete_policy(&router, &arn).await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected HTTP 204 from the removal, got {status} ({removed})"
        );
        let (status, after) = get_policy(&router, &arn).await;
        assert_eq!(
            (status, after.get("policy").cloned()),
            (200, None),
            "case {case:?} (after the removal): expected HTTP 200 with no policy member, got \
             {status} ({after})"
        );
    }
}

/// RULE: the REMOVAL of a document that is not there is the one refusal this condition earns
/// — the modelled 404 in its own fixed sentence — so the read and the removal of the very
/// same resource are distinguishable answers.
#[tokio::test]
async fn only_the_removal_refuses_a_resource_carrying_no_policy() {
    let router = router();

    for (case, arn) in policy_carrying_resources(&router).await {
        let (read_status, read) = get_policy(&router, &arn).await;
        let (delete_status, refused) = delete_policy(&router, &arn).await;
        assert_eq!(
            (read_status, read.get("policy").cloned()),
            (200, None),
            "case {case:?}: the read answers for a resource carrying no policy, got \
             {read_status} ({read})"
        );
        assert_eq!(
            (
                delete_status,
                error_code(&refused).as_str(),
                text(&refused, "message")
            ),
            (404, "ResourceNotFoundException", POLICY_MISSING.to_string()),
            "case {case:?}: the removal is refused {POLICY_MISSING:?}, got {delete_status} {} \
             ({refused})",
            error_code(&refused)
        );

        // The refused removal left the resource exactly as it was: still readable, still
        // carrying nothing.
        let (status, after) = get_policy(&router, &arn).await;
        assert_eq!(
            (status, after.get("policy").cloned()),
            (200, None),
            "case {case:?}: the refused removal changed nothing, got {status} ({after})"
        );
    }
}

/// RULE: "carries no document" is a different condition from "names no resource" and from
/// "takes no policy at all", so the read keeps the three apart instead of collapsing them
/// onto one answer.
#[tokio::test]
async fn a_read_keeps_the_three_conditions_apart() {
    let router = router();
    let held = create_gateway(&router, "threeconditionsgateway").await;
    let account = ::engine::emulator_account_id();

    // (case, the ARN addressed, the expected status, the expected error code, the expected
    // message) — the empty code marks the row that is answered rather than refused.
    let conditions: Vec<(&str, String, u16, &str, String)> = vec![
        (
            "a held resource carrying no document",
            held.clone(),
            200,
            "",
            String::new(),
        ),
        (
            // A policy-carrying type naming nothing this account holds.
            "a gateway ARN naming no stored gateway",
            state::gateway_arn(REGION, "absentgateway-0123456789"),
            404,
            "ResourceNotFoundException",
            RESOURCE_MISSING.to_string(),
        ),
        (
            // A resource type of this service whose resources take no policy.
            "a memory ARN, whose type carries no policy plane",
            format!("arn:aws:bedrock-agentcore:{REGION}:{account}:memory/probe-0123456789"),
            400,
            "ValidationException",
            UNSUPPORTED_TYPE.to_string(),
        ),
    ];

    for (case, arn, expected_status, expected_code, expected_message) in conditions {
        let (status, answer) = get_policy(&router, &arn).await;
        if expected_code.is_empty() {
            assert_eq!(
                (status, answer.get("policy").cloned()),
                (expected_status, None),
                "case {case:?}: expected HTTP {expected_status} with no policy member, got \
                 {status} ({answer})"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&answer).as_str(), text(&answer, "message")),
            (expected_status, expected_code, expected_message.clone()),
            "case {case:?}: expected HTTP {expected_status} {expected_code} \
             {expected_message:?}, got {status} {} ({answer})",
            error_code(&answer)
        );
    }
}
