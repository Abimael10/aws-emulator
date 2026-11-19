// SPDX-License-Identifier: CC0-1.0

//! WHICH RESOURCE a resource policy of this control plane may be attached to, driven end to
//! end over the wire path (a `restJson1` request through the multi-service router into the
//! mounted service), so what is asserted is what a client sees.
//!
//! THE RULE UNDER TEST: the resource-policy operations are decided by the ARN resource TYPE
//! the request addresses, because the conditions a client branches on have different
//! remedies. A type whose resources carry a policy plane — an AgentCore Runtime, a Gateway, a
//! Browser and a Code Interpreter, each under the ARN type its own `*Arn` pattern publishes —
//! resolves: the document is stored, reported and removed, and such a type naming no resource
//! this account holds is the modelled 404. Another resource type OF THIS SERVICE carries no
//! policy plane at all, which is the modelled `ValidationException` (400): the request is
//! what has to change. An ARN naming no resource type of this service addresses nothing of
//! this account, which is the modelled `AccessDeniedException` (403), worded in the sentence
//! this endpoint denies a call in — the principal that signed and the action asked for.
//!
//! The tables span what the rule generalises over: the resource TYPE (the four that carry a
//! policy, fifteen of this service's that do not, and ARNs naming no type of it at all), the
//! OPERATION asked (all three, so each refusal is the plane's and not one handler's), and —
//! for the denial — the shape of the unrecognised ARN (an invented AgentCore type, a
//! differently cased spelling of a real one, another service's namespace, a resource part
//! naming no type at all).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::state;
use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region every row signs for.
const REGION: &str = "us-east-1";

/// The access key id every row signs with: one neither identity store holds a record for, so
/// the principal a denial names is the account's own published credential.
const KEY_ID: &str = "AKIDEXAMPLE";

/// How a policy lookup that resolves no document is worded, for a resource this service
/// HOLDS that carries none.
const POLICY_MISSING: &str = "Resource Policy doesn't exist for requested arn";

/// How every resource-policy operation words a `resourceArn` of a policy-carrying type that
/// names no resource this account holds.
const RESOURCE_MISSING: &str = "ResourceArn provided in the request does not exist";

/// How every resource-policy operation words a `resourceArn` whose resource type carries no
/// policy plane: the member's own refusal, in the shape this service reports one refused
/// member in (`crate::state::resource_policy_unsupported_type`).
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
        "AWS4-HMAC-SHA256 Credential={KEY_ID}/20260101/{REGION}/bedrock-agentcore/aws4_request, \
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
fn encoded_label(value: &str) -> String {
    value.replace(':', "%3A").replace('/', "%2F")
}

/// The `/resourcepolicy/{resourceArn}` path of `arn`
/// (`smithy_query(query="operation", operation="PutResourcePolicy")`).
fn resource_policy_uri(arn: &str) -> String {
    format!("/resourcepolicy/{}", encoded_label(arn))
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

/// The sentence this endpoint denies a call in: the principal that signed — the account's own
/// published credential here, as `sts:GetCallerIdentity` reports it — and the action the
/// operation is performed under.
fn not_authorized(operation: &str) -> String {
    format!(
        "User: {} is not authorized to perform: bedrock-agentcore:{operation}",
        ::service_common::caller_principal::ambient_user_arn()
    )
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
        "setup: the agent runtime a policy case attaches to is created, got {status} ({created})"
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
        "setup: the gateway a policy case attaches to is created, got {status} ({created})"
    );
    text(&created, "gatewayArn")
}

/// Create a browser of this account's own and answer its ARN
/// (`CreateBrowser`: `PUT /browsers`, `BrowserNetworkMode` `PUBLIC`).
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
        "setup: the browser a policy case attaches to is created, got {status} ({created})"
    );
    text(&created, "browserArn")
}

/// Create a code interpreter of this account's own and answer its ARN
/// (`CreateCodeInterpreter`: `PUT /code-interpreters`, `CodeInterpreterNetworkMode`
/// `SANDBOX`).
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
        "setup: the code interpreter a policy case attaches to is created, got {status} \
         ({created})"
    );
    text(&created, "codeInterpreterArn")
}

/// RULE: every resource type whose resources carry a policy plane round-trips one — the write
/// reports the document it stored, a read reports exactly that document, and the delete
/// removes it — whichever of the four families the ARN names.
#[tokio::test]
async fn a_policy_round_trips_on_every_policy_carrying_resource_type() {
    let router = router();

    // (case, the resource the policy attaches to) — one per policy-carrying ARN type.
    let covered: Vec<(&str, String)> = vec![
        (
            "an agent runtime",
            create_runtime(&router, "scoped_runtime").await,
        ),
        ("a gateway", create_gateway(&router, "scopedgateway").await),
        ("a browser", create_browser(&router, "scopedBrowser").await),
        (
            "a code interpreter",
            create_code_interpreter(&router, "scopedInterpreter").await,
        ),
        (
            // The sandbox tools this service provides are resources of the account's plane
            // too, published under the bare spelling of their ARN type.
            "the service-provided browser",
            state::system_browser_arn(REGION),
        ),
        (
            "the service-provided code interpreter",
            state::system_code_interpreter_arn(REGION),
        ),
    ];

    for (case, arn) in covered {
        let document = policy_document(
            &format!("Sid{}", case.replace(' ', "")),
            "bedrock-agentcore:InvokeAgentRuntime",
        );
        let (status, answer) = put_policy(&router, &arn, &document).await;
        assert_eq!(
            (status, text(&answer, "policy")),
            (201, document.clone()),
            "case {case:?}: expected HTTP 201 echoing the submitted document, got {status} \
             ({answer})"
        );

        let (status, read_back) = get_policy(&router, &arn).await;
        assert_eq!(
            (status, text(&read_back, "policy")),
            (200, document.clone()),
            "case {case:?}: expected the stored document back, got {status} ({read_back})"
        );

        let (status, removed) = delete_policy(&router, &arn).await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected HTTP 204 from DeleteResourcePolicy, got {status} ({removed})"
        );
        let (status, gone) = get_policy(&router, &arn).await;
        assert_eq!(
            (status, error_code(&gone).as_str(), text(&gone, "message")),
            (404, "ResourceNotFoundException", POLICY_MISSING.to_string()),
            "case {case:?}: expected the removed document to be gone, got {status} ({gone})"
        );
    }
}

/// RULE: a policy-carrying resource type naming no resource this account holds in the Region
/// signed for is the modelled 404 — the ARN resolves to nothing, so the remedy is to address
/// a resource that exists.
#[tokio::test]
async fn a_policy_carrying_type_naming_no_held_resource_is_not_found() {
    let router = router();
    let browser = create_browser(&router, "heldBrowser").await;
    let browser_id = browser.rsplit('/').next().unwrap_or_default().to_string();
    let interpreter = create_code_interpreter(&router, "heldInterpreter").await;
    let interpreter_id = interpreter
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();

    // (case, the ARN addressed) — each carries a policy-carrying resource type, and each
    // names no resource of this account in the Region the request is signed for.
    let absent: Vec<(&str, String)> = vec![
        (
            "a browser ARN naming no stored browser",
            state::browser_arn(REGION, "absentBrowser-0123456789"),
        ),
        (
            "a code-interpreter ARN naming no stored interpreter",
            state::code_interpreter_arn(REGION, "absentInterpreter-0123456789"),
        ),
        (
            "a stored browser's id in another Region than the request",
            state::browser_arn("eu-west-2", &browser_id),
        ),
        (
            "a stored code interpreter's id under another account",
            state::code_interpreter_arn(REGION, &interpreter_id).replace(
                ::engine::emulator_account_id(),
                "210987654321",
            ),
        ),
        (
            // The service-provided sandboxes answer to one reserved id each, so any other id
            // under their bare ARN type names nothing.
            "the bare browser ARN type under an id nothing holds",
            format!(
                "arn:aws:bedrock-agentcore:{REGION}:{}:browser/notTheReservedId-0123456789",
                ::engine::emulator_account_id()
            ),
        ),
    ];

    for (case, arn) in absent {
        for (operation, (status, answer)) in [
            (
                "PutResourcePolicy",
                put_policy(&router, &arn, &policy_document("Absent", "bedrock-agentcore:Any")).await,
            ),
            ("GetResourcePolicy", get_policy(&router, &arn).await),
            ("DeleteResourcePolicy", delete_policy(&router, &arn).await),
        ] {
            assert_eq!(
                (status, error_code(&answer).as_str(), text(&answer, "message")),
                (
                    404,
                    "ResourceNotFoundException",
                    RESOURCE_MISSING.to_string()
                ),
                "case {case:?} ({operation}): expected HTTP 404 ResourceNotFoundException \
                 {RESOURCE_MISSING:?}, got {status} ({answer})"
            );
        }
    }
}

/// RULE: a resource type OF THIS SERVICE that carries no policy plane is refused with the
/// modelled `ValidationException`, and nothing is stored for it.
///
/// The rows are the service's other ARN resource types, each built by this crate's own
/// reading of that family's `*Arn` pattern, so the table is the catalog rather than one cited
/// ARN. Whether the named resource exists does not enter into it: no resource of these types
/// takes a policy.
#[tokio::test]
async fn a_resource_type_without_a_policy_plane_is_refused_as_a_validation_error() {
    let router = router();
    let gateway = create_gateway(&router, "untouchedgateway").await;
    let kept = policy_document("Kept", "bedrock-agentcore:InvokeGateway");
    let (status, _) = put_policy(&router, &gateway, &kept).await;
    assert_eq!(status, 201, "setup: the gateway's own policy is stored");

    let vault = "default";
    // (case, the ARN addressed). Source: each family's own `*Arn` pattern as this crate's
    // `crate::state` builders render it — the credential providers and the workload identity
    // are nested under the token vault and the workload identity directory, so the type their
    // ARN carries is the parent's.
    let out_of_scope: Vec<(&str, String)> = vec![
        (
            "a workload identity ARN",
            state::workload_identity_arn(REGION, "probe-identity"),
        ),
        (
            "an API key credential provider ARN",
            state::api_key_credential_provider_arn(REGION, vault, "probe-provider"),
        ),
        (
            "an OAuth2 credential provider ARN",
            state::oauth2_credential_provider_arn(REGION, vault, "probe-provider"),
        ),
        (
            "a payment credential provider ARN",
            state::payment_credential_provider_arn(REGION, vault, "probe-provider"),
        ),
        ("a memory ARN", state::memory_arn(REGION, "probe-0123456789")),
        (
            "a registry ARN",
            state::registry_arn(REGION, "aaaaaaaaaaaa"),
        ),
        (
            "a dataset ARN",
            state::dataset_arn(REGION, "probe-dataset"),
        ),
        (
            "a harness ARN",
            state::harness_arn(REGION, "probe-0123456789"),
        ),
        (
            "a configuration bundle ARN",
            state::configuration_bundle_arn(REGION, "probe-0123456789"),
        ),
        (
            "a browser profile ARN",
            state::browser_profile_arn(REGION, "probe-0123456789"),
        ),
        (
            "a capacity provider ARN",
            state::capacity_provider_arn(REGION, "probe-0123456789"),
        ),
        (
            "a payment manager ARN",
            state::payment_manager_arn(REGION, "probe-0123456789"),
        ),
        (
            "a policy engine ARN",
            state::policy_engine_arn(REGION, "probe-0123456789"),
        ),
        (
            "a consent portal ARN",
            state::consent_portal_arn(REGION, "probe-0123456789"),
        ),
        (
            "an online evaluation config ARN",
            state::online_evaluation_config_arn(REGION, "probe-0123456789"),
        ),
        (
            // `EvaluatorArn`'s own pattern, which this crate publishes no builder for.
            "an evaluator ARN",
            format!(
                "arn:aws:bedrock-agentcore:{REGION}:{}:evaluator/probe-0123456789",
                ::engine::emulator_account_id()
            ),
        ),
    ];

    for (case, arn) in out_of_scope {
        for (operation, (status, answer)) in [
            (
                "PutResourcePolicy",
                put_policy(
                    &router,
                    &arn,
                    &policy_document("OutOfScope", "bedrock-agentcore:Any"),
                )
                .await,
            ),
            ("GetResourcePolicy", get_policy(&router, &arn).await),
            ("DeleteResourcePolicy", delete_policy(&router, &arn).await),
        ] {
            assert_eq!(
                (status, error_code(&answer).as_str(), text(&answer, "message")),
                (400, "ValidationException", UNSUPPORTED_TYPE.to_string()),
                "case {case:?} ({operation}): expected HTTP 400 ValidationException \
                 {UNSUPPORTED_TYPE:?}, got {status} ({answer})"
            );
        }
    }

    // None of the refusals stored, replaced or removed anything: the one policy this account
    // holds is still the gateway's own document.
    let (status, still_there) = get_policy(&router, &gateway).await;
    assert_eq!(
        (status, text(&still_there, "policy")),
        (200, kept),
        "case \"the gateway's policy after every refusal\": expected it untouched, got {status} \
         ({still_there})"
    );
}

/// RULE: an ARN naming no resource type of this service addresses nothing of this account, so
/// it is denied — the modelled `AccessDeniedException` (403) in the sentence this endpoint
/// words a denial in, naming the principal that signed and the action the operation asked
/// for.
#[tokio::test]
async fn an_arn_naming_no_resource_type_of_this_service_is_denied() {
    let router = router();
    let gateway = create_gateway(&router, "deniedprobegateway").await;
    let gateway_id = gateway.rsplit('/').next().unwrap_or_default().to_string();
    let account = ::engine::emulator_account_id();

    // (case, the ARN addressed) — each is a well-formed `BedrockAgentcoreResourceArn`
    // (`len 20..1011`, no pattern), so every answer below is the handler's and not the
    // boundary's.
    let unrecognized: Vec<(&str, String)> = vec![
        (
            "an invented AgentCore resource type",
            format!(
                "arn:aws:bedrock-agentcore:{REGION}:{account}:totally-bogus-type/does-not-exist-xyz"
            ),
        ),
        (
            // The resource type is matched as this service publishes it, so a differently
            // cased spelling names no type of it.
            "a stored gateway's id under an upper-cased resource type",
            format!("arn:aws:bedrock-agentcore:{REGION}:{account}:GATEWAY/{gateway_id}"),
        ),
        (
            // No separator in the resource part at all: nothing names a type and an id.
            "an ARN whose resource part names no type",
            format!("arn:aws:bedrock-agentcore:{REGION}:{account}:{gateway_id}"),
        ),
        (
            "an ARN of another service's namespace entirely",
            "arn:aws:s3:::a-bucket/that-is-not-agentcore".to_string(),
        ),
        (
            "a value that is no ARN at all",
            "not-an-arn-at-all/anything".to_string(),
        ),
    ];

    for (case, arn) in unrecognized {
        for (operation, (status, answer)) in [
            (
                "PutResourcePolicy",
                put_policy(
                    &router,
                    &arn,
                    &policy_document("Denied", "bedrock-agentcore:Any"),
                )
                .await,
            ),
            ("GetResourcePolicy", get_policy(&router, &arn).await),
            ("DeleteResourcePolicy", delete_policy(&router, &arn).await),
        ] {
            let expected = not_authorized(operation);
            assert_eq!(
                (status, error_code(&answer).as_str(), text(&answer, "message")),
                (403, "AccessDeniedException", expected.clone()),
                "case {case:?} ({operation}): expected HTTP 403 AccessDeniedException \
                 {expected:?}, got {status} ({answer})"
            );
        }
    }

    // A denied call stored nothing either: the gateway still carries no policy.
    let (status, answer) = get_policy(&router, &gateway).await;
    assert_eq!(
        (status, error_code(&answer).as_str(), text(&answer, "message")),
        (404, "ResourceNotFoundException", POLICY_MISSING.to_string()),
        "case \"the gateway after every denial\": expected it to carry no policy, got {status} \
         ({answer})"
    );
}
