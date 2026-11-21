// SPDX-License-Identifier: CC0-1.0

//! The two refusals the gateway-RULE surface words for itself, end to end over HTTP.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! RULE 1, over every lifecycle status a gateway reports: a rule decides how its gateway
//! routes and processes the requests it fronts, so a rule is written only once that gateway
//! has reached its terminal `READY` status. A rule write addressed to a gateway in any other
//! status of `GatewayStatus` (`CREATING | UPDATING | UPDATE_UNSUCCESSFUL | DELETING | READY |
//! FAILED`, `smithy_query(query="shape",shape="GatewayStatus")`) is refused the modelled
//! `ValidationException` (HTTP 400,
//! `smithy_query(query="operation",operation="CreateGatewayRule")`), naming the gateway the
//! request's path addressed and the status that gateway is in, and writes nothing. The rows
//! differ along what the rule generalises over: the status the gateway is in and how it got
//! there (fresh from a create, mid-update, on its way out), and the length of the identifier
//! the path carried. The other half of the rule is asserted beside them: the identical
//! request against the same gateway once it reports `READY` is served.
//!
//! RULE 2, over the whole 36-character `GatewayRuleId` space: a read of a rule its gateway
//! does not hold is the modelled `ResourceNotFoundException` (HTTP 404,
//! `smithy_query(query="operation",operation="GetGatewayRule")`) naming the resource type and
//! the `ruleId` the request asked for, so a caller that fans a read out over several rule ids
//! can attribute the failure to one of them. The rows differ along what the rule generalises
//! over: the id's digits (the all-zero id, every digit at its maximum, a mixed one) and
//! whether the id names a rule this service holds on ANOTHER gateway or no rule at all. The
//! boundaries are asserted beside them: an id OUTSIDE the label's published pattern never
//! reaches the lookup, a gateway that is not there is reported by the gateway family's own
//! sentence, and a rule the gateway DOES hold is served.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// The terminal status the clock owes a gateway
/// (`smithy_query(query="shape",shape="GatewayStatus")`).
const READY: &str = "READY";

/// A `clientToken` inside `ClientToken`'s 33..256 length and its pattern
/// `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
const CLIENT_TOKEN: &str = "gatewayrulestategatetokenaaaaaaaaa";

/// A `gatewayIdentifier` inside `GatewayIdentifier`'s pattern
/// `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$` that nothing here ever creates.
const ABSENT_GATEWAY_ID: &str = "no-such-gateway-9876543210";

/// The sentence this service answers a `gatewayIdentifier` naming no gateway with, which is
/// the GATEWAY family's and not the rule family's.
const GATEWAY_ABSENT: &str = "Failed to retrieve gateway because it doesn't exist. Retry the \
                              request with a different resource identifier.";

/// How long a poll loop keeps looking for a clock-owed transition before giving up. Generous
/// on purpose: the loop waits for the CONDITION, and the span this service gives the gateway
/// family is far shorter than this on any host.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

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

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> Answer {
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

/// One rule this service will store: the smallest body `CreateGatewayRule` declares as
/// required (`priority` and one `action`).
fn one_rule(priority: i64) -> Value {
    json!({
        "priority": priority,
        "actions": [{"routeToTarget": {"staticRoute": {"targetName": "orders"}}}],
    })
}

async fn create_rule(router: &MultiServiceRouter, gateway: &str, body: &Value) -> Answer {
    send(router, "POST", &format!("/gateways/{gateway}/rules"), Some(body)).await
}

async fn get_rule(router: &MultiServiceRouter, gateway: &str, rule: &str) -> Answer {
    send(router, "GET", &format!("/gateways/{gateway}/rules/{rule}"), None).await
}

/// Create a gateway and hand back its `gatewayId`, while the clock still owes it its
/// transition out of `CREATING`.
async fn creating_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let created = send(
        router,
        "POST",
        "/gateways/",
        Some(&json!({"name": name, "roleArn": ROLE_ARN, "authorizerType": "AWS_IAM"})),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "setup {name:?}: expected HTTP 202 from CreateGateway, got {} ({})",
        created.status, created.body
    );
    text(&created.body, "gatewayId")
}

/// Read the gateway again until it reports `READY`, or the deadline passes — the transition
/// is the clock's, so the loop looks for the CONDITION rather than waiting a fixed time.
async fn gateway_ready(router: &MultiServiceRouter, gateway_id: &str) {
    let started = Instant::now();
    loop {
        let answer = send(router, "GET", &format!("/gateways/{gateway_id}/"), None).await;
        if text(&answer.body, "status") == READY || started.elapsed() > POLL_DEADLINE {
            return;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Create a gateway and hand back its `gatewayId` once it reports `READY`.
async fn ready_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let gateway_id = creating_gateway(router, name).await;
    gateway_ready(router, &gateway_id).await;
    gateway_id
}

/// What a row drives its gateway into before it writes a rule. Each is a status of
/// `GatewayStatus` this service's own operations can put a gateway in.
#[derive(Clone, Copy, Debug)]
enum Lifecycle {
    /// Fresh from the create: the clock still owes the transition out of `CREATING`.
    Creating,
    /// An accepted `UpdateGateway`: the clock still owes the transition back to `READY`.
    Updating,
    /// An accepted `DeleteGateway`: the gateway is on its way out.
    Deleting,
}

/// Put `gateway_id` into `lifecycle`, through the operations that drive it there.
async fn drive_to(router: &MultiServiceRouter, gateway_id: &str, name: &str, lifecycle: Lifecycle) {
    match lifecycle {
        // The create already left it there.
        Lifecycle::Creating => {}
        Lifecycle::Updating => {
            gateway_ready(router, gateway_id).await;
            // `name` must be the one the gateway was created with, and `roleArn` and
            // `authorizerType` are `@required`
            // (`smithy_query(query="operation",operation="UpdateGateway")`).
            let updated = send(
                router,
                "PUT",
                &format!("/gateways/{gateway_id}/"),
                Some(&json!({"name": name, "roleArn": ROLE_ARN, "authorizerType": "AWS_IAM"})),
            )
            .await;
            assert_eq!(
                updated.status, 202,
                "setup {gateway_id}: the update must be accepted, got {} ({})",
                updated.status, updated.body
            );
        }
        Lifecycle::Deleting => {
            gateway_ready(router, gateway_id).await;
            let deleted = send(
                router,
                "DELETE",
                &format!("/gateways/{gateway_id}/"),
                None,
            )
            .await;
            assert_eq!(
                deleted.status, 202,
                "setup {gateway_id}: the deletion must be accepted, got {} ({})",
                deleted.status, deleted.body
            );
        }
    }
}

/// One row of the lifecycle table.
struct StateRow {
    case: &'static str,
    /// The gateway name the row creates (`GatewayName` is `^([0-9a-zA-Z][-]?){1,48}$`), which
    /// is what the identifier the refusal names is derived from.
    name: &'static str,
    lifecycle: Lifecycle,
    /// The status word the refusal reports, which is the enum token rendered for prose.
    status_word: &'static str,
    /// The rule the row submits, so no two rows submit the same body.
    priority: i64,
}

/// The rows: one per status a rule write can find its gateway in, over identifiers of very
/// different lengths.
fn state_rows() -> Vec<StateRow> {
    vec![
        StateRow {
            case: "fresh from the create, the clock still owing CREATING its transition",
            name: "rulestatecreating",
            lifecycle: Lifecycle::Creating,
            status_word: "Creating",
            priority: 1,
        },
        StateRow {
            case: "the shortest name the model admits, fresh from the create",
            name: "r",
            lifecycle: Lifecycle::Creating,
            status_word: "Creating",
            priority: 1_000_000,
        },
        StateRow {
            case: "an accepted update, the clock still owing the way back to READY",
            name: "rulestateupdating",
            lifecycle: Lifecycle::Updating,
            status_word: "Updating",
            priority: 500,
        },
        StateRow {
            case: "an accepted deletion, the gateway on its way out",
            name: "rulestatedeleting",
            lifecycle: Lifecycle::Deleting,
            status_word: "Deleting",
            priority: 42,
        },
    ]
}

/// RULE 1: a rule write is refused with the modelled 400 unless the gateway it addresses has
/// reached `READY`, and the refusal names that gateway and the status it is in.
#[tokio::test]
async fn a_rule_write_is_refused_until_its_gateway_is_ready() {
    for StateRow {
        case,
        name,
        lifecycle,
        status_word,
        priority,
    } in state_rows()
    {
        let router = router();
        let gateway_id = creating_gateway(&router, name).await;
        drive_to(&router, &gateway_id, name, lifecycle).await;

        let refused = create_rule(&router, &gateway_id, &one_rule(priority)).await;
        assert_eq!(
            (refused.status, error_code(&refused.body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected HTTP 400 ValidationException, got {} {} ({})",
            refused.status,
            error_code(&refused.body),
            refused.body
        );
        assert_eq!(
            text(&refused.body, "message"),
            format!("Gateway {gateway_id} is not in READY state. Current status: {status_word}"),
            "case {case:?}: the refusal names the gateway the path addressed and the status \
             it is in, got {}",
            refused.body
        );
        assert_eq!(
            text(&refused.body, "reason"),
            "ResourceConflict",
            "case {case:?}: nothing is wrong with the request's members, so the refusal is \
             the gateway's state, got {}",
            refused.body
        );
        assert_eq!(
            refused.body.get("fieldList"),
            None,
            "case {case:?}: a state refusal faults no member of the request, got {}",
            refused.body
        );
    }
}

/// RULE 1's other half, on ONE gateway: the identical request the transient status refused is
/// served once the gateway reports `READY`, and the refusal wrote nothing — not even under the
/// `clientToken` it carried.
#[tokio::test]
async fn the_same_request_is_served_once_the_gateway_is_ready() {
    let router = router();
    let gateway_id = creating_gateway(&router, "rulestateserved").await;

    let mut refused_body = one_rule(7);
    refused_body["clientToken"] = json!(CLIENT_TOKEN);
    let refused = create_rule(&router, &gateway_id, &refused_body).await;
    assert_eq!(
        (refused.status, error_code(&refused.body).as_str()),
        (400, "ValidationException"),
        "a gateway the clock is still creating takes no rule, got {} {} ({})",
        refused.status,
        error_code(&refused.body),
        refused.body
    );

    gateway_ready(&router, &gateway_id).await;
    // The same token, with a priority the refused request did not carry: an answer carrying
    // THIS priority can only come from a rule the refused call never wrote.
    let mut served_body = one_rule(9);
    served_body["clientToken"] = json!(CLIENT_TOKEN);
    let served = create_rule(&router, &gateway_id, &served_body).await;
    assert_eq!(
        served.status, 202,
        "a READY gateway takes the rule its transient status refused, got {} ({})",
        served.status, served.body
    );
    assert_eq!(
        served.body.get("priority"),
        Some(&json!(9)),
        "the served create stored its own submission, got {}",
        served.body
    );

    let stored = get_rule(&router, &gateway_id, &text(&served.body, "ruleId")).await;
    assert_eq!(
        (stored.status, stored.body.get("priority")),
        (200, Some(&json!(9))),
        "the rule the READY gateway took reads back, got {} ({})",
        stored.status,
        stored.body
    );
}

/// One row of the not-found table: a `ruleId` inside the label's published pattern that the
/// gateway under test does not hold.
struct UnheldRow {
    case: &'static str,
    rule_id: String,
}

/// RULE 2: a read of a rule the gateway does not hold names the resource type and the
/// `ruleId` the request asked for.
#[tokio::test]
async fn a_rule_the_gateway_does_not_hold_is_named_in_the_not_found() {
    let router = router();
    let gateway_id = ready_gateway(&router, "rulemissingread").await;

    // A rule this service DOES hold, on another gateway: the read is scoped to the gateway
    // the path names, so that id is just as missing here as one nothing ever minted.
    let other_gateway_id = ready_gateway(&router, "rulemissingother").await;
    let elsewhere = create_rule(&router, &other_gateway_id, &one_rule(11)).await;
    assert_eq!(
        elsewhere.status, 202,
        "setup: the other gateway's rule must be created, got {} ({})",
        elsewhere.status, elsewhere.body
    );

    // Every id below is 36 characters inside `GatewayRuleId`'s pattern
    // `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`.
    let rows = [
        UnheldRow {
            case: "every digit at its minimum",
            rule_id: "00000000-0000-0000-0000-000000000000".to_string(),
        },
        UnheldRow {
            case: "every digit at its maximum",
            rule_id: "ffffffff-ffff-ffff-ffff-ffffffffffff".to_string(),
        },
        UnheldRow {
            case: "digits and letters mixed through every group",
            rule_id: "0a1b2c3d-4e5f-6a7b-8c9d-ef0123456789".to_string(),
        },
        UnheldRow {
            case: "a rule this service holds, on another gateway",
            rule_id: text(&elsewhere.body, "ruleId"),
        },
    ];

    for UnheldRow { case, rule_id } in rows {
        let missing = get_rule(&router, &gateway_id, &rule_id).await;
        assert_eq!(
            (missing.status, error_code(&missing.body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected HTTP 404 ResourceNotFoundException, got {} {} ({})",
            missing.status,
            error_code(&missing.body),
            missing.body
        );
        assert_eq!(
            text(&missing.body, "message"),
            format!("GatewayRule {rule_id} not found"),
            "case {case:?}: the refusal names the rule the request asked for, got {}",
            missing.body
        );
    }
}

/// RULE 2's boundaries: an id outside the label's published pattern never reaches the lookup,
/// a gateway that is not there is reported by the GATEWAY family's sentence, and a rule the
/// gateway does hold is served.
#[tokio::test]
async fn the_not_found_wording_is_bounded_by_the_label_and_the_gateway() {
    let router = router();
    let gateway_id = ready_gateway(&router, "rulemissingbounds").await;

    // One step outside `GatewayRuleId`: a group one digit short, and a digit outside the
    // pattern's lower-case hex alphabet. Both are the protocol boundary's 400, not a 404.
    for (case, rule_id) in [
        (
            "a group one digit short of the published length",
            "0000000-0000-0000-0000-000000000000",
        ),
        (
            "an upper-case digit, outside the published alphabet",
            "AAAAAAAA-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
        ),
    ] {
        let refused = get_rule(&router, &gateway_id, rule_id).await;
        assert_eq!(
            (refused.status, error_code(&refused.body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: an id outside the published pattern is refused at the boundary, \
             got {} {} ({})",
            refused.status,
            error_code(&refused.body),
            refused.body
        );
    }

    // The gateway is resolved first, and reported in the gateway family's own sentence.
    let absent = get_rule(
        &router,
        ABSENT_GATEWAY_ID,
        "00000000-0000-0000-0000-000000000000",
    )
    .await;
    assert_eq!(
        (absent.status, error_code(&absent.body).as_str()),
        (404, "ResourceNotFoundException"),
        "a gateway nothing holds is the modelled 404, got {} {} ({})",
        absent.status,
        error_code(&absent.body),
        absent.body
    );
    assert_eq!(
        text(&absent.body, "message"),
        GATEWAY_ABSENT,
        "a missing GATEWAY is reported in the gateway family's sentence, got {}",
        absent.body
    );

    // The served side: a rule the gateway holds is read back, so the rows above cannot pass
    // by refusing every read.
    let created = create_rule(&router, &gateway_id, &one_rule(3)).await;
    assert_eq!(
        created.status, 202,
        "setup: the held rule must be created, got {} ({})",
        created.status, created.body
    );
    let rule_id = text(&created.body, "ruleId");
    let held = get_rule(&router, &gateway_id, &rule_id).await;
    assert_eq!(
        (held.status, text(&held.body, "ruleId")),
        (200, rule_id.clone()),
        "the rule the gateway holds is served, got {} ({})",
        held.status,
        held.body
    );
}
