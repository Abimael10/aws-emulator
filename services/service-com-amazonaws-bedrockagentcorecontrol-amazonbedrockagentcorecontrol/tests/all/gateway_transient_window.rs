// SPDX-License-Identifier: CC0-1.0

//! The WINDOW a gateway spends in the transient status its own create, update or delete
//! leaves it in, and what a client can do while that window lasts.
//!
//! RULE: a transition this service owes a gateway — or any child of the gateway family: a
//! target, a rule, a rate limit — belongs to the CLOCK. The record carries the instant the
//! transition comes due, every report reads the status that instant implies, and nothing a
//! caller does (reading the gateway, reading it again, being refused by it) moves that
//! instant. The window the store arms is chosen against what a client can OBSERVE of it,
//! which bounds it from both sides:
//!
//! * it outlives the call that starts the transition AND the calls that caller makes next,
//!   so the transient status, and every state refusal keyed on it, is observable to a client
//!   that goes on working instead of depending on how fast its calls happen to be
//!   dispatched. Which refusal a workflow hears is then the same on a fast host and on a
//!   slow one. That half is pinned where it is observable, through the wire:
//!   `tests/all/gateway_target_status_precedes_the_target_lookup.rs`;
//! * it stays a transition a client waits out by POLLING the resource — bounded by
//!   [`MAX_TRANSIENT_WINDOW`] — rather than an open-ended wait, so a caller that waits for
//!   the status it needs gets there.
//!
//! The tests below pin all three halves of that rule: the status a stored due instant
//! implies (and that reads never move it), the SIZE of the window every gateway-family
//! transition arms, and the clock-driven create-settle-delete-gone round trip a client
//! actually drives, with no sleep the caller has to know about.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::{
    state, EmulatedService,
};

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The longest window a gateway-family transition may still have to run when the call that
/// armed it answers — so, the longest span the service may arm.
///
/// What bounds it is the client, not a figure AWS publishes: a transition a client can poll
/// out, so that a caller which waits for the status it needs reaches it inside one ordinary
/// request's patience. It is NOT bounded by one round trip to this endpoint: the recorded
/// service keeps a new gateway in `CREATING` across the calls a caller makes right after the
/// create, and a window of that order would make which refusal a workflow hears depend on
/// how fast its calls are dispatched.
const MAX_TRANSIENT_WINDOW: Duration = Duration::from_secs(5);

/// How long the clock-driven round trip keeps looking for the condition it waits on. The
/// transition is the clock's, so the wait is for the CONDITION; the deadline is generous
/// because only the condition, never the wall clock, decides the outcome.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll pauses before looking again.
const POLL_PAUSE: Duration = Duration::from_millis(5);

/// This service's store, as the engine hands it out.
type Store = state::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState;

fn router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// A SigV4 `Authorization` header for this service: what the router routes on, and what the
/// handlers read the ARN Region from.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{REGION}/bedrock-agentcore/\
         aws4_request, SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
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

async fn create_gateway(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    send(router, "POST", "/gateways/", Some(body)).await
}

async fn get_gateway(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    send(router, "GET", &format!("/gateways/{id}/"), None).await
}

async fn update_gateway(router: &MultiServiceRouter, id: &str, body: &Value) -> (u16, Value) {
    send(router, "PUT", &format!("/gateways/{id}/"), Some(body)).await
}

async fn delete_gateway(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    send(router, "DELETE", &format!("/gateways/{id}/"), None).await
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The error code `restJson1` puts in `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A `CreateGateway` body: `CUSTOM_JWT` is the one `authorizerType` whose value changes what
/// the request must carry, so only it gets a configuration
/// (`smithy_query(query="shape",shape="AuthorizerType")`).
fn gateway_body(name: &str, authorizer_type: &str, description: Option<&str>) -> Value {
    let mut body = json!({
        "name": name,
        "roleArn": ROLE_ARN,
        "authorizerType": authorizer_type,
    });
    if authorizer_type == "CUSTOM_JWT" {
        body["authorizerConfiguration"] = json!({
            "customJWTAuthorizer": {
                "discoveryUrl": "https://example.com/.well-known/openid-configuration",
                "allowedClients": ["client-1"]
            }
        });
    }
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    body
}

/// An `UpdateGateway` body: the three members the model makes `@required` on the request.
fn update_body(name: &str) -> Value {
    json!({"name": name, "roleArn": ROLE_ARN, "authorizerType": "AWS_IAM"})
}

/// An MCP target carrying an inline OpenAPI schema — one of `TargetConfiguration`'s shapes.
fn target_body(name: &str) -> Value {
    json!({
        "name": name,
        "targetConfiguration": {
            "mcp": {"openApiSchema": {"inlinePayload": "{\"openapi\":\"3.0.0\"}"}}
        }
    })
}

/// A rate limit over one dimension key, with one request rate per second.
fn rate_limit_body() -> Value {
    json!({
        "dimensionKeys": ["targetName"],
        "entries": [{
            "dimensions": {"targetName": "orders"},
            "requests": [{"rate": 25.0, "period": "second"}],
        }],
    })
}

/// A rule routing statically to one target, at a priority inside `GatewayRulePriority`'s
/// published `@range 1..1000000`.
fn rule_body() -> Value {
    json!({
        "priority": 5,
        "actions": [{"routeToTarget": {"staticRoute": {"targetName": "orders"}}}],
    })
}

/// The stored gateway `id`, as the handlers see it.
fn gateway_record(engine: &Arc<engine::InMemoryEngine>, id: &str) -> state::GatewayRecord {
    let store = engine.state::<Store>();
    let store = store.lock();
    store
        .gateways
        .get(id)
        .cloned()
        .unwrap_or_else(|| panic!("the store holds the gateway {id:?} this test created"))
}

/// Push the gateway's clock-owed transition well into the future, so the status under test
/// is the transient one for as long as the test needs it — and is so because of the stored
/// due instant, not because the test outran a span.
///
/// Writes only the due instant the service itself stores.
fn hold_transient(engine: &Arc<engine::InMemoryEngine>, id: &str) {
    let store = engine.state::<Store>();
    let mut store = store.lock();
    let gateway = store
        .gateways
        .get_mut(id)
        .unwrap_or_else(|| panic!("the store holds the gateway {id:?} this test created"));
    gateway.settles_at = SystemTime::now() + Duration::from_secs(3600);
}

/// Make the gateway's clock-owed transition fall due, so the next request materialises the
/// settled status without waiting a span out on the wall clock.
fn settle_now(engine: &Arc<engine::InMemoryEngine>, id: &str) {
    let store = engine.state::<Store>();
    let mut store = store.lock();
    let gateway = store
        .gateways
        .get_mut(id)
        .unwrap_or_else(|| panic!("the store holds the gateway {id:?} this test created"));
    gateway.settles_at = SystemTime::now() - Duration::from_millis(1);
}

/// Make the removal an accepted deletion owes fall due, so the next request materialises it.
fn removal_now(engine: &Arc<engine::InMemoryEngine>, id: &str) {
    let store = engine.state::<Store>();
    let mut store = store.lock();
    let gateway = store
        .gateways
        .get_mut(id)
        .unwrap_or_else(|| panic!("the store holds the gateway {id:?} this test created"));
    gateway.removal_due_at = Some(SystemTime::now() - Duration::from_millis(1));
}

/// How this service words the refusal of an operation on a gateway whose status cannot serve
/// it: the asking operation and the status it found, spelled as prose.
fn status_conflict(operation: &str, status_in_prose: &str) -> String {
    format!(
        "{operation} operation can't be performed on gateway when it is in {status_in_prose} \
         state. Retry the request when the gateway is in a valid state."
    )
}

/// The fixed sentence every gateway read answers for a gateway that is not there.
const GATEWAY_NOT_FOUND: &str = "Failed to retrieve gateway because it doesn't exist. Retry the \
                                 request with a different resource identifier.";

/// RULE: the status a gateway reports, and therefore which mutations it serves, is the one
/// its stored due instant implies — and reading it, however often, moves nothing. While the
/// clock still owes the transition the delete is refused in the modelled sentence; once the
/// instant has passed the SAME delete is served, and the gateway is gone once the removal
/// the delete owes falls due.
///
/// The rows differ along the `authorizerType` the gateway was created with (every value
/// `AuthorizerType` declares, `CUSTOM_JWT` with the configuration it requires) and along
/// whether the gateway carries a description: neither is what the status gate reads, so the
/// same sequence must hold for all of them.
#[tokio::test]
async fn a_gateway_serves_a_delete_only_from_the_status_its_due_instant_implies() {
    // (case, authorizerType, description)
    let rows: [(&str, &str, Option<&str>); 5] = [
        ("AWS_IAM, no description", "AWS_IAM", None),
        ("NONE, no description", "NONE", None),
        (
            "AUTHENTICATE_ONLY with a description",
            "AUTHENTICATE_ONLY",
            Some("a gateway to tear down"),
        ),
        (
            "CUSTOM_JWT with its configuration",
            "CUSTOM_JWT",
            Some("a jwt gateway"),
        ),
        ("AWS_IAM with a description", "AWS_IAM", Some("d")),
    ];

    for (index, (case, authorizer_type, description)) in rows.into_iter().enumerate() {
        let (router, engine) = router();
        let name = format!("windowgw{index}");
        let (status, created) =
            create_gateway(&router, &gateway_body(&name, authorizer_type, description)).await;
        assert_eq!(
            (status, text(&created, "status").as_str()),
            (202, "CREATING"),
            "case {case:?}: the create is accepted and reports the transient status, got \
             {status} {} ({created})",
            text(&created, "status")
        );
        let id = text(&created, "gatewayId");

        // The transient status is what the stored due instant says, so the test says what
        // that instant is rather than racing a span.
        hold_transient(&engine, &id);
        let (status, first_read) = get_gateway(&router, &id).await;
        assert_eq!(
            (status, text(&first_read, "status").as_str()),
            (200, "CREATING"),
            "case {case:?}: a gateway whose transition is not due yet reports CREATING, got \
             {status} {} ({first_read})",
            text(&first_read, "status")
        );
        let (status, second_read) = get_gateway(&router, &id).await;
        assert_eq!(
            (status, &second_read),
            (200, &first_read),
            "case {case:?}: reading the gateway again must report the identical gateway — a \
             read moves no transition — got {status} ({second_read})"
        );

        let (status, refused) = delete_gateway(&router, &id).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: a delete inside the window is the modelled 400, got {status} {} \
             ({refused})",
            error_code(&refused)
        );
        assert_eq!(
            text(&refused, "message"),
            status_conflict("DeleteGateway", "Creating"),
            "case {case:?}: expected the modelled state-conflict sentence, got {:?}",
            text(&refused, "message")
        );
        let (status, survived) = get_gateway(&router, &id).await;
        assert_eq!(
            (status, text(&survived, "status").as_str()),
            (200, "CREATING"),
            "case {case:?}: the refused delete wrote nothing, got {status} {} ({survived})",
            text(&survived, "status")
        );

        // The transition the clock owes, materialised: the very same delete is now served.
        settle_now(&engine, &id);
        let (status, ready) = get_gateway(&router, &id).await;
        assert_eq!(
            (status, text(&ready, "status").as_str()),
            (200, "READY"),
            "case {case:?}: past its due instant the gateway reports READY, got {status} {} \
             ({ready})",
            text(&ready, "status")
        );

        let (status, accepted) = delete_gateway(&router, &id).await;
        assert_eq!(
            (
                status,
                text(&accepted, "gatewayId"),
                text(&accepted, "status")
            ),
            (202, id.clone(), "DELETING".to_string()),
            "case {case:?}: the delete is accepted for the gateway it named, got {status} \
             ({accepted})"
        );
        let (status, deleting) = get_gateway(&router, &id).await;
        assert_eq!(
            (status, text(&deleting, "status").as_str()),
            (200, "DELETING"),
            "case {case:?}: an accepted deletion is readable in DELETING, got {status} {} \
             ({deleting})",
            text(&deleting, "status")
        );

        // The removal is the clock's too: once it is due the gateway is gone, and every read
        // of it is the modelled 404.
        removal_now(&engine, &id);
        let (status, gone) = get_gateway(&router, &id).await;
        assert_eq!(
            (status, error_code(&gone).as_str(), text(&gone, "message")),
            (
                404,
                "ResourceNotFoundException",
                GATEWAY_NOT_FOUND.to_string()
            ),
            "case {case:?}: a removed gateway reads back as the modelled 404, got {status} \
             ({gone})"
        );
    }
}

/// RULE: every transition of the gateway family arms a window that is open when the call
/// that armed it answers (so the transient status is observable to that caller and to the
/// calls it makes next) and no longer than [`MAX_TRANSIENT_WINDOW`] beyond that answer (so
/// the caller reaches the status it is waiting for by polling for it).
///
/// Both bounds are measured so that neither depends on how fast this host is: the lower one
/// against the instant the arming call was ISSUED (the window must still be open by then,
/// which a non-zero span guarantees), the upper one against the instant it was ANSWERED (so
/// the bound is on the span itself and not on how long the call took).
///
/// The rows are the transitions themselves: the gateway's own create, update and the removal
/// its delete owes, plus one child of each kind that hangs off a gateway — a target, a rate
/// limit and a rule — because the whole family shares the one span.
#[tokio::test]
async fn every_gateway_family_transition_arms_a_bounded_window_a_caller_can_observe() {
    let (router, engine) = router();

    // (case, the instant the call that armed the window was ISSUED, the instant it was
    // ANSWERED, the due instant it armed)
    let mut armed: Vec<(&str, SystemTime, SystemTime, SystemTime)> = Vec::new();

    let name = "windowfamily";
    let issued = SystemTime::now();
    let (status, created) = create_gateway(&router, &gateway_body(name, "AWS_IAM", None)).await;
    let answered = SystemTime::now();
    assert_eq!(
        status, 202,
        "setup: CreateGateway must be accepted, got {status} ({created})"
    );
    let id = text(&created, "gatewayId");
    armed.push((
        "CreateGateway arms the gateway's CREATING transition",
        issued,
        answered,
        gateway_record(&engine, &id).settles_at,
    ));

    // A gateway serves a mutation only from the status it settles into, so each row below
    // starts from a settled gateway.
    settle_now(&engine, &id);
    let issued = SystemTime::now();
    let (status, updated) = update_gateway(&router, &id, &update_body(name)).await;
    let answered = SystemTime::now();
    assert_eq!(
        status, 202,
        "setup: UpdateGateway must be accepted, got {status} ({updated})"
    );
    armed.push((
        "UpdateGateway arms the gateway's UPDATING transition",
        issued,
        answered,
        gateway_record(&engine, &id).settles_at,
    ));

    settle_now(&engine, &id);
    let issued = SystemTime::now();
    let (status, target) = send(
        &router,
        "POST",
        &format!("/gateways/{id}/targets/"),
        Some(&target_body("windowtarget")),
    )
    .await;
    let answered = SystemTime::now();
    assert_eq!(
        status, 202,
        "setup: CreateGatewayTarget must be accepted, got {status} ({target})"
    );
    let target_id = text(&target, "targetId");
    let target_due = gateway_record(&engine, &id)
        .targets
        .get(&target_id)
        .map(|stored| stored.settles_at)
        .unwrap_or_else(|| panic!("the gateway holds the target {target_id:?} it just created"));
    armed.push((
        "CreateGatewayTarget arms the target's CREATING transition",
        issued,
        answered,
        target_due,
    ));

    let issued = SystemTime::now();
    let (status, limit) = send(
        &router,
        "POST",
        &format!("/gateways/{id}/rate-limits"),
        Some(&rate_limit_body()),
    )
    .await;
    let answered = SystemTime::now();
    assert_eq!(
        status, 201,
        "setup: CreateGatewayRateLimit must be accepted, got {status} ({limit})"
    );
    let limit_id = text(&limit, "rateLimitId");
    let limit_due = gateway_record(&engine, &id)
        .rate_limits
        .get(&limit_id)
        .map(|stored| stored.settles_at)
        .unwrap_or_else(|| panic!("the gateway holds the rate limit {limit_id:?} it just created"));
    armed.push((
        "CreateGatewayRateLimit arms the limit's CREATING transition",
        issued,
        answered,
        limit_due,
    ));

    let issued = SystemTime::now();
    let (status, rule) = send(
        &router,
        "POST",
        &format!("/gateways/{id}/rules"),
        Some(&rule_body()),
    )
    .await;
    let answered = SystemTime::now();
    assert_eq!(
        status, 202,
        "setup: CreateGatewayRule must be accepted, got {status} ({rule})"
    );
    let rule_id = text(&rule, "ruleId");
    let rule_due = gateway_record(&engine, &id)
        .rules
        .get(&rule_id)
        .map(|stored| stored.settles_at)
        .unwrap_or_else(|| panic!("the gateway holds the rule {rule_id:?} it just created"));
    armed.push((
        "CreateGatewayRule arms the rule's CREATING transition",
        issued,
        answered,
        rule_due,
    ));

    // The delete's own window: how long a gateway stays readable in DELETING before the
    // clock owes its removal. On a gateway of its own, because a gateway that still holds
    // a target serves no deletion at all.
    let (status, second) =
        create_gateway(&router, &gateway_body("windowdelete", "NONE", None)).await;
    assert_eq!(
        status, 202,
        "setup: the second CreateGateway must be accepted, got {status} ({second})"
    );
    let second_id = text(&second, "gatewayId");
    settle_now(&engine, &second_id);
    let issued = SystemTime::now();
    let (status, deleted) = delete_gateway(&router, &second_id).await;
    let answered = SystemTime::now();
    assert_eq!(
        status, 202,
        "setup: DeleteGateway must be accepted for a settled gateway, got {status} ({deleted})"
    );
    let removal_due = gateway_record(&engine, &second_id)
        .removal_due_at
        .expect("an accepted deletion owes the gateway's removal");
    armed.push((
        "DeleteGateway arms the gateway's removal",
        issued,
        answered,
        removal_due,
    ));

    for (case, issued, answered, due) in armed {
        assert!(
            due > issued,
            "case {case:?}: the window must still be open when the call that armed it is made, \
             so the caller acting straight after it observes the transient status; the due \
             instant {due:?} is not after {issued:?}"
        );
        let bound = answered + MAX_TRANSIENT_WINDOW;
        assert!(
            due <= bound,
            "case {case:?}: the window must stay one a caller can poll out, so it is at most \
             {MAX_TRANSIENT_WINDOW:?}; the due instant {due:?} is past {bound:?}"
        );
    }
}

/// Read the gateway until it reports `wanted`, or the deadline passes: the transition is the
/// clock's, so the wait looks for the CONDITION rather than for a fixed time.
async fn gateway_reporting(router: &MultiServiceRouter, id: &str, wanted: &str) -> (u16, Value) {
    let started = Instant::now();
    loop {
        let answer = get_gateway(router, id).await;
        if text(&answer.1, "status") == wanted || started.elapsed() > POLL_DEADLINE {
            return answer;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Read the gateway until it is gone, or the deadline passes.
async fn gateway_gone(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    let started = Instant::now();
    loop {
        let answer = get_gateway(router, id).await;
        if answer.0 == 404 || started.elapsed() > POLL_DEADLINE {
            return answer;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// RULE, end to end on the service's own clock with nothing poking the store: a client that
/// creates a gateway and then tears it down gets through — the gateway settles on its own,
/// the delete it then issues is served, and the gateway reads back as the modelled 404.
///
/// The rows differ along the `authorizerType`, so the round trip is not keyed to one shape
/// of gateway.
#[tokio::test]
async fn the_clock_settles_a_gateway_on_its_own_and_the_tear_down_round_trip_completes() {
    let (router, _engine) = router();

    for (index, authorizer_type) in ["AWS_IAM", "NONE", "CUSTOM_JWT"].into_iter().enumerate() {
        let case = authorizer_type;
        let name = format!("roundtrip{index}");
        let (status, created) =
            create_gateway(&router, &gateway_body(&name, authorizer_type, None)).await;
        assert_eq!(
            (status, text(&created, "status").as_str()),
            (202, "CREATING"),
            "case {case:?}: the create is accepted and reports the transient status, got \
             {status} {} ({created})",
            text(&created, "status")
        );
        let id = text(&created, "gatewayId");

        let (status, ready) = gateway_reporting(&router, &id, "READY").await;
        assert_eq!(
            (status, text(&ready, "status").as_str()),
            (200, "READY"),
            "case {case:?}: the clock owes the gateway READY and nothing else has to ask for \
             it, got {status} {} ({ready})",
            text(&ready, "status")
        );
        assert_eq!(
            text(&ready, "gatewayId"),
            id,
            "case {case:?}: the read answers for the gateway the create minted, got {ready}"
        );

        let (status, accepted) = delete_gateway(&router, &id).await;
        assert_eq!(
            (status, text(&accepted, "gatewayId")),
            (202, id.clone()),
            "case {case:?}: the delete of a settled gateway is accepted and echoes its id, \
             got {status} ({accepted})"
        );

        let (status, gone) = gateway_gone(&router, &id).await;
        assert_eq!(
            (status, error_code(&gone).as_str(), text(&gone, "message")),
            (
                404,
                "ResourceNotFoundException",
                GATEWAY_NOT_FOUND.to_string()
            ),
            "case {case:?}: the deleted gateway reads back as the modelled 404, got {status} \
             ({gone})"
        );
    }
}
