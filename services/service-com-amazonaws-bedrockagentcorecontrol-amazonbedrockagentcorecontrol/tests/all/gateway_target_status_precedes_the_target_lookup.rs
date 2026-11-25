// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a caller hears when a gateway that cannot serve a change to its targets is
//! asked for one anyway, and for how long that gateway status is observable.
//!
//! Each row drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers all answer.
//!
//! The rules under test, read off the model
//! (`smithy_query(query="operation",operation="DeleteGatewayTarget,UpdateGatewayTarget,\
//! SynchronizeGatewayTargets,GetGatewayTarget")`) and off the gateway's own stored state
//! rather than off any one recorded request:
//!
//! 1. An operation that MUTATES a gateway's targets is decided on the parent gateway's
//!    lifecycle status BEFORE the target the request names is resolved: while the gateway
//!    reports anything other than the status it settles into, the answer is the modelled
//!    `ValidationException` (HTTP 400) naming the operation and that status, whatever the
//!    `targetId` beside it names — a target the gateway holds, or one it never minted. The
//!    rows differ along the operation, the non-terminal status the gateway is in and how it
//!    got there, and whether the target named is one the gateway holds.
//! 2. The gateway's status is a PRECONDITION and not a lookup failure, so the target lookup
//!    — the modelled `ResourceNotFoundException` (HTTP 404) — is reached only once the
//!    gateway reports the status it settles into, where the same requests are answered on
//!    the target they name.
//! 3. The gate is on mutation, not on reading: a REPORTING operation on the same gateway
//!    answers from state while the gateway is still in its transient status.
//! 4. A transient status is observable to the calls a caller makes right after the one that
//!    armed it: a client that creates a gateway and keeps working meets the same
//!    precondition on its third call as on its first, so which refusal it hears does not
//!    depend on how fast its calls are dispatched.

use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The polls every gateway-target suite of this crate waits a clock-owed transition out by.
use crate::gateway_target_settling::{POLL_DEADLINE, POLL_PAUSE};

/// The Region the rows sign for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// A `targetId` inside `TargetId`'s pattern `^[0-9a-zA-Z]{10}$` that no row ever mints.
const ABSENT_TARGET_ID: &str = "Zz99QqAa00";

/// The status a gateway settles into, which is the one that admits a change to its targets
/// (`smithy_query(query="shape",shape="GatewayStatus")`).
const READY: &str = "READY";

/// How this service words a target of a stored gateway that is not there.
const TARGET_ABSENT_MESSAGE: &str = "Failed to retrieve target because it doesn't exist. \
                                     Retry the request with a different resource identifier.";

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header for this service, which is what the router routes on.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

/// One answer: the HTTP status and the parsed body.
struct Answer {
    status: u16,
    body: Value,
}

async fn send(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<&Value>) -> Answer {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.us-east-1.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        },
    }
}

/// The error code `restJson1` puts in the body's `__type`.
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

/// A target configuration union: an MCP target serving an inline OpenAPI schema.
fn openapi_configuration(payload: &str) -> Value {
    json!({"mcp": {"openApiSchema": {"inlinePayload": payload}}})
}

/// A target configuration union: an MCP SERVER target, whose tool definitions are
/// discovered from the endpoint it names rather than declared in its own configuration —
/// the one shape a synchronization has something to fetch for
/// (`smithy_query(query="doc",shape="SynchronizeGatewayTargets")`).
fn mcp_server_configuration() -> Value {
    json!({"mcp": {"mcpServer": {"endpoint": "https://tools.example.com/mcp"}}})
}

/// A `CreateGatewayTarget` body, with the gateway's own IAM role as the credential the
/// response `@require`s (`smithy_query(query="shape",shape="CredentialProviderType")`).
fn create_target_body(name: &str, configuration: Value) -> Value {
    json!({
        "name": name,
        "description": "the target the rows address",
        "targetConfiguration": configuration,
        "credentialProviderConfigurations": [{"credentialProviderType": "GATEWAY_IAM_ROLE"}],
    })
}

/// A `CreateGateway` body for `name`.
fn gateway_body(name: &str) -> Value {
    json!({"name": name, "roleArn": ROLE_ARN, "authorizerType": "AWS_IAM"})
}

/// Create a gateway and hand back its `gatewayId`, WITHOUT waiting the clock out: a new
/// gateway reports its transient status, which is what rules 1 and 4 are about.
async fn create_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let created = send(router, "POST", "/gateways/", Some(&gateway_body(name))).await;
    assert_eq!(
        created.status, 202,
        "setup {name:?}: CreateGateway must be accepted for this suite to say anything, got {} \
         ({})",
        created.status, created.body
    );
    assert_eq!(
        text(&created.body, "status"),
        "CREATING",
        "setup {name:?}: a gateway the clock still owes a transition reports CREATING, got {}",
        text(&created.body, "status")
    );
    text(&created.body, "gatewayId")
}

/// Read the gateway again until the clock has made its transition out of the transient
/// status due.
///
/// The transition is the clock's, so the wait looks for the CONDITION rather than for a
/// fixed time: a host slower than this one only reads again.
async fn wait_until_ready(router: &MultiServiceRouter, gateway_id: &str, case: &str) {
    let started = Instant::now();
    let settled = loop {
        let read = send(router, "GET", &format!("/gateways/{gateway_id}/"), None).await;
        if text(&read.body, "status") == READY || started.elapsed() > POLL_DEADLINE {
            break read;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    };
    assert_eq!(
        (settled.status, text(&settled.body, "status").as_str()),
        (200, READY),
        "case {case:?}: the gateway must settle before a target change is served, got {} {} ({})",
        settled.status,
        text(&settled.body, "status"),
        settled.body
    );
}

/// Create a gateway and wait the clock out, so it admits a change to its targets.
async fn settled_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let gateway_id = create_gateway(router, name).await;
    wait_until_ready(router, &gateway_id, name).await;
    gateway_id
}

/// Create one target on a settled gateway and hand back its `targetId`.
async fn create_target(
    router: &MultiServiceRouter,
    gateway_id: &str,
    name: &str,
    configuration: Value,
) -> String {
    let created = send(
        router,
        "POST",
        &format!("/gateways/{gateway_id}/targets/"),
        Some(&create_target_body(name, configuration)),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "setup {name:?}: a settled gateway must accept the target, got {} ({})",
        created.status, created.body
    );
    text(&created.body, "targetId")
}

/// One operation that MUTATES a gateway's targets and names one in its request.
struct Mutation {
    /// The operation's name, which the refusal names too.
    operation: &'static str,
    method: &'static str,
    /// The request this operation makes of `gateway` for `target`.
    request: fn(&str, &str) -> (String, Option<Value>),
    /// A configuration a target this operation SERVES carries: the synchronize applies
    /// only to a target whose tools live at its endpoint, while the delete and the update
    /// serve any target.
    served_target: fn() -> Value,
}

/// Every operation of this service that names a `targetId` and CHANGES what the gateway
/// serves: the delete, the update, and the synchronize (which carries the id in its body
/// rather than in its path, `PUT /gateways/{gatewayIdentifier}/synchronizeTargets`).
fn mutations() -> [Mutation; 3] {
    [
        Mutation {
            operation: "DeleteGatewayTarget",
            method: "DELETE",
            request: |gateway, target| (format!("/gateways/{gateway}/targets/{target}/"), None),
            served_target: || openapi_configuration("{\"openapi\":\"3.0.0\"}"),
        },
        Mutation {
            operation: "UpdateGatewayTarget",
            method: "PUT",
            request: |gateway, target| {
                (
                    format!("/gateways/{gateway}/targets/{target}/"),
                    Some(json!({
                        "name": "retargeted",
                        "targetConfiguration": openapi_configuration("{\"openapi\":\"3.0.3\"}"),
                        "credentialProviderConfigurations": [
                            {"credentialProviderType": "GATEWAY_IAM_ROLE"}
                        ],
                    })),
                )
            },
            served_target: || openapi_configuration("{\"openapi\":\"3.0.0\"}"),
        },
        Mutation {
            operation: "SynchronizeGatewayTargets",
            method: "PUT",
            request: |gateway, target| {
                (
                    format!("/gateways/{gateway}/synchronizeTargets"),
                    Some(json!({"targetIdList": [target]})),
                )
            },
            served_target: mcp_server_configuration,
        },
    ]
}

/// How a row's gateway got out of the status that admits a target change, and what it
/// reports once it is: every non-terminal `GatewayStatus` a caller of this emulator can
/// reach (`smithy_query(query="shape",shape="GatewayStatus")`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lifecycle {
    /// A gateway the clock has not finished creating.
    JustCreated,
    /// A gateway with an update in flight.
    UpdateInFlight,
    /// A gateway whose own deletion has been accepted.
    DeletionAccepted,
}

impl Lifecycle {
    /// The `GatewayStatus` token the gateway reports, which the refusal names.
    fn status(self) -> &'static str {
        match self {
            Lifecycle::JustCreated => "CREATING",
            Lifecycle::UpdateInFlight => "UPDATING",
            Lifecycle::DeletionAccepted => "DELETING",
        }
    }
}

/// Bring a gateway into `lifecycle`, with one target on it when `with_target`, and hand
/// back `(gatewayId, the targetId the row addresses)`.
///
/// A target can only be minted while the gateway admits one, so a row that wants the
/// gateway to HOLD the target mints it first and moves the gateway afterwards; a row that
/// does not addresses an id the gateway never minted.
async fn gateway_in(
    router: &MultiServiceRouter,
    name: &str,
    lifecycle: Lifecycle,
    with_target: bool,
    case: &str,
) -> (String, String) {
    let gateway_id = if lifecycle == Lifecycle::JustCreated {
        create_gateway(router, name).await
    } else {
        settled_gateway(router, name).await
    };

    let target_id = if with_target {
        create_target(
            router,
            &gateway_id,
            "gatedtarget",
            openapi_configuration("{\"openapi\":\"3.0.0\"}"),
        )
        .await
    } else {
        ABSENT_TARGET_ID.to_string()
    };

    match lifecycle {
        Lifecycle::JustCreated => {}
        Lifecycle::UpdateInFlight => {
            let updated = send(
                router,
                "PUT",
                &format!("/gateways/{gateway_id}/"),
                Some(&gateway_body(name)),
            )
            .await;
            assert_eq!(
                (updated.status, text(&updated.body, "status").as_str()),
                (202, lifecycle.status()),
                "setup {case:?}: UpdateGateway must leave the gateway in {}, got {} {} ({})",
                lifecycle.status(),
                updated.status,
                text(&updated.body, "status"),
                updated.body
            );
        }
        Lifecycle::DeletionAccepted => {
            // A gateway that still has a target ATTACHED serves no deletion of its own, so
            // a row holding a target asks for that target to go first. The record is still
            // there — on its way out on the clock — so what refuses the row below is the
            // GATEWAY's status.
            if with_target {
                let dropped = send(
                    router,
                    "DELETE",
                    &format!("/gateways/{gateway_id}/targets/{target_id}/"),
                    None,
                )
                .await;
                assert_eq!(
                    dropped.status, 202,
                    "setup {case:?}: the target must be asked to go first, got {} ({})",
                    dropped.status, dropped.body
                );
            }
            let deleted = send(router, "DELETE", &format!("/gateways/{gateway_id}/"), None).await;
            assert_eq!(
                (deleted.status, text(&deleted.body, "status").as_str()),
                (202, lifecycle.status()),
                "setup {case:?}: DeleteGateway must leave the gateway in {}, got {} {} ({})",
                lifecycle.status(),
                deleted.status,
                text(&deleted.body, "status"),
                deleted.body
            );
        }
    }

    (gateway_id, target_id)
}

/// Rule 1: the gateway's status decides the answer before the target the request names is
/// resolved, for every operation that changes what a gateway serves.
#[tokio::test]
async fn the_gateway_status_is_decided_before_the_target_the_request_names() {
    // (lifecycle, whether the gateway HOLDS the target the row addresses). A gateway the
    // clock has not finished creating holds no target at all — it admits none — so its rows
    // address an id it never minted, which is exactly the shape of the refusal under test.
    let lifecycles: [(Lifecycle, bool); 5] = [
        (Lifecycle::JustCreated, false),
        (Lifecycle::UpdateInFlight, true),
        (Lifecycle::UpdateInFlight, false),
        (Lifecycle::DeletionAccepted, true),
        (Lifecycle::DeletionAccepted, false),
    ];

    for (index, (lifecycle, with_target)) in lifecycles.into_iter().enumerate() {
        for mutation in mutations() {
            let held = if with_target {
                "a target the gateway holds"
            } else {
                "a target id the gateway never minted"
            };
            let case = format!("{} on {:?} naming {held}", mutation.operation, lifecycle);
            let router = router();
            let (gateway_id, target_id) = gateway_in(
                &router,
                &format!("statusfirst{index}"),
                lifecycle,
                with_target,
                &case,
            )
            .await;

            let (uri, body) = (mutation.request)(&gateway_id, &target_id);
            let refused = send(&router, mutation.method, &uri, body.as_ref()).await;
            assert_eq!(
                (refused.status, error_code(&refused.body).as_str()),
                (400, "ValidationException"),
                "case {case:?}: a gateway that cannot serve the change must answer HTTP 400 \
                 ValidationException rather than a lookup failure, got {} {} ({})",
                refused.status,
                error_code(&refused.body),
                refused.body
            );
            assert_eq!(
                text(&refused.body, "message"),
                format!(
                    "Cannot perform operation {} when gateway is in {} status",
                    mutation.operation,
                    lifecycle.status()
                ),
                "case {case:?}: the refusal names the operation asked for and the status the \
                 gateway is in, got {}",
                text(&refused.body, "message")
            );
            assert!(
                !text(&refused.body, "message").contains(&target_id),
                "case {case:?}: the precondition is the gateway's, so it says nothing of the \
                 target id, got {}",
                text(&refused.body, "message")
            );
        }
    }
}

/// Rule 2: once the gateway reports the status it settles into, the SAME requests are
/// answered on the target they name — the absent one with the modelled 404, the stored one
/// with acceptance.
#[tokio::test]
async fn a_settled_gateway_answers_on_the_target_the_request_names() {
    for (index, mutation) in mutations().into_iter().enumerate() {
        let router = router();
        let case_absent = format!("{} naming a target id the gateway never minted", mutation.operation);
        let gateway_id = settled_gateway(&router, &format!("lookupnext{index}")).await;

        let (uri, body) = (mutation.request)(&gateway_id, ABSENT_TARGET_ID);
        let refused = send(&router, mutation.method, &uri, body.as_ref()).await;
        assert_eq!(
            (refused.status, error_code(&refused.body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case_absent:?}: a settled gateway reaches the target lookup, got {} {} ({})",
            refused.status,
            error_code(&refused.body),
            refused.body
        );
        assert_eq!(
            text(&refused.body, "message"),
            TARGET_ABSENT_MESSAGE,
            "case {case_absent:?}: the lookup failure is worded as this service words a missing \
             target, got {}",
            text(&refused.body, "message")
        );

        let case_present = format!("{} naming a target the gateway holds", mutation.operation);
        let target_id =
            create_target(&router, &gateway_id, "servedtarget", (mutation.served_target)()).await;
        let (uri, body) = (mutation.request)(&gateway_id, &target_id);
        let served = send(&router, mutation.method, &uri, body.as_ref()).await;
        assert_eq!(
            served.status, 202,
            "case {case_present:?}: a settled gateway serves the change, got {} ({})",
            served.status, served.body
        );
    }
}

/// Rule 3: the gate is on CHANGING what the gateway serves. A reporting operation answers
/// from state while the gateway is still in its transient status — including the lookup
/// failure for a target it does not hold.
#[tokio::test]
async fn a_read_is_not_gated_by_the_gateways_transient_status() {
    let router = router();
    let gateway_id = create_gateway(&router, "readsstillanswer").await;

    let read = send(
        &router,
        "GET",
        &format!("/gateways/{gateway_id}/targets/{ABSENT_TARGET_ID}/"),
        None,
    )
    .await;
    assert_eq!(
        (read.status, error_code(&read.body).as_str()),
        (404, "ResourceNotFoundException"),
        "case \"GetGatewayTarget on a gateway the clock still owes a transition\": a read \
         reports, so it answers the lookup rather than the gateway's status, got {} {} ({})",
        read.status,
        error_code(&read.body),
        read.body
    );
    assert_eq!(
        text(&read.body, "message"),
        TARGET_ABSENT_MESSAGE,
        "case \"GetGatewayTarget on a gateway the clock still owes a transition\": the refusal is \
         the missing target's, got {}",
        text(&read.body, "message")
    );

    let listed = send(&router, "GET", &format!("/gateways/{gateway_id}/targets/"), None).await;
    assert_eq!(
        listed.status, 200,
        "case \"ListGatewayTargets on a gateway the clock still owes a transition\": a listing \
         reports what the gateway serves, got {} ({})",
        listed.status, listed.body
    );
    assert_eq!(
        listed
            .body
            .get("items")
            .and_then(Value::as_array)
            .map(Vec::len),
        Some(0),
        "case \"ListGatewayTargets on a gateway the clock still owes a transition\": a gateway \
         that admits no target yet serves none, got {}",
        listed.body
    );
}

/// Rule 4: the transient status outlives the calls a caller makes right after the one that
/// armed it, so the precondition a client meets does not depend on how many calls it has
/// made in between.
#[tokio::test]
async fn the_transient_status_outlives_the_callers_next_calls() {
    let router = router();
    let gateway_id = create_gateway(&router, "stillcreating").await;

    // The calls an ordinary create-then-configure script makes before it comes back to the
    // gateway's targets. Each one is answered, and none of them is what moves the gateway.
    let interleaved: [(&str, String); 4] = [
        ("GetGateway", format!("/gateways/{gateway_id}/")),
        ("ListGateways", "/gateways/".to_string()),
        (
            "ListGatewayTargets",
            format!("/gateways/{gateway_id}/targets/"),
        ),
        ("GetGateway again", format!("/gateways/{gateway_id}/")),
    ];
    for (case, uri) in interleaved {
        let read = send(&router, "GET", &uri, None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the read must answer, got {} ({})",
            read.status, read.body
        );
        if case.starts_with("GetGateway") {
            assert_eq!(
                text(&read.body, "status"),
                Lifecycle::JustCreated.status(),
                "case {case:?}: the gateway still reports the status the clock owes a transition \
                 out of, got {} ({})",
                text(&read.body, "status"),
                read.body
            );
        }
    }

    // And the precondition is the same one the caller's FIRST call would have met.
    let refused = send(
        &router,
        "DELETE",
        &format!("/gateways/{gateway_id}/targets/{ABSENT_TARGET_ID}/"),
        None,
    )
    .await;
    assert_eq!(
        (refused.status, error_code(&refused.body).as_str()),
        (400, "ValidationException"),
        "case \"a target delete several calls after the create\": the gateway's status still \
         decides the answer, got {} {} ({})",
        refused.status,
        error_code(&refused.body),
        refused.body
    );
    assert_eq!(
        text(&refused.body, "message"),
        format!(
            "Cannot perform operation DeleteGatewayTarget when gateway is in {} status",
            Lifecycle::JustCreated.status()
        ),
        "case \"a target delete several calls after the create\": the refusal names the status \
         the gateway is in, got {}",
        text(&refused.body, "message")
    );

    // The transition is still the clock's: once it comes due the same gateway is READY.
    wait_until_ready(&router, &gateway_id, "the clock-owed transition").await;
}
