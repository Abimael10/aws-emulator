// SPDX-License-Identifier: CC0-1.0

//! A gateway's rule priorities, end to end over HTTP: one gateway holds at most one rule
//! at any priority.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="CreateGatewayRule")` and
//! `smithy_query(query="doc",shape="CreateGatewayRuleRequest",member="priority")`): a rule's
//! `priority` is the order the gateway EVALUATES it in ("Rules are evaluated in order of
//! priority, with lower numbers evaluated first"), so a write that would put a second rule
//! of one gateway at a priority a sibling rule already holds is refused — HTTP 400,
//! `ValidationException`, "Priority <priority> conflicts with an existing rule" — and
//! persists nothing (the gateway keeps exactly the one rule at that priority, which
//! `ListGatewayRules` reports), while a priority no sibling holds is accepted (202) and
//! settles `ACTIVE`. The scope is the ONE gateway the path names: another gateway's rules are
//! evaluated in their own order, so every priority is free on it again.
//!
//! The rows differ along what the rule generalises over: the priority (both published bounds
//! of `GatewayRulePriority`, `@range 1..1000000`, and values between them), the direction the
//! free priority lies in from the taken one, and the shape of the colliding request — its
//! conditions, actions and description all differ from the rule that holds the priority, so
//! only the priority can be what the refusal is about.

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

/// A configuration-bundle ARN inside `GatewayConfigurationBundleArn`'s pattern
/// `^arn:aws[a-zA-Z-]*:bedrock-agentcore:[a-z0-9-]+:[0-9]{12}:configuration-bundle/\
/// [a-zA-Z][a-zA-Z0-9-_]{0,99}-[a-zA-Z0-9]{10}$`.
const BUNDLE_ARN: &str = "arn:aws:bedrock-agentcore:us-east-1:000000000000:\
                          configuration-bundle/checkout-abcdefghij";

/// A bundle version inside `ConfigurationBundleReference.bundleVersion`'s pattern
/// `^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`.
const BUNDLE_VERSION: &str = "0123abcd-4567-89ef-0123-456789abcdef";

/// How long a poll loop keeps looking for a clock-owed transition before giving up.
/// Generous on purpose: the loop waits for the CONDITION, and the span this service gives
/// the gateway family is far shorter than this, on any host.
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

async fn create_rule(router: &MultiServiceRouter, gateway: &str, body: &Value) -> Answer {
    send(
        router,
        "POST",
        &format!("/gateways/{gateway}/rules"),
        Some(body),
    )
    .await
}

async fn get_rule(router: &MultiServiceRouter, gateway: &str, rule: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/gateways/{gateway}/rules/{rule}"),
        None,
    )
    .await
}

/// The rules the gateway holds, as `ListGatewayRules` reports them: the `(ruleId, priority)`
/// of every rule, over however many pages the listing takes.
async fn listed_rules(router: &MultiServiceRouter, gateway: &str) -> Vec<(String, i64)> {
    let mut rules = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let uri = match &token {
            None => format!("/gateways/{gateway}/rules"),
            Some(token) => format!("/gateways/{gateway}/rules?nextToken={token}"),
        };
        let page = send(router, "GET", &uri, None).await;
        assert_eq!(
            page.status, 200,
            "expected HTTP 200 from ListGatewayRules, got {} ({})",
            page.status, page.body
        );
        let on_this_page = page
            .body
            .get("gatewayRules")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for rule in &on_this_page {
            rules.push((
                text(rule, "ruleId"),
                rule.get("priority").and_then(Value::as_i64).unwrap_or(-1),
            ));
        }
        match page.body.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => return rules,
        }
    }
}

/// Create a gateway and hand back its `gatewayId` once the gateway reports `READY` — a
/// rule write is refused until the gateway is routable, so every row that writes one waits
/// for that condition first.
async fn gateway(router: &MultiServiceRouter, name: &str) -> String {
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
    let gateway_id = text(&created.body, "gatewayId");
    let started = Instant::now();
    loop {
        let answer = send(router, "GET", &format!("/gateways/{gateway_id}/"), None).await;
        if text(&answer.body, "status") == "READY" {
            return gateway_id;
        }
        assert!(
            started.elapsed() <= POLL_DEADLINE,
            "setup {name:?}: the gateway never reached READY, last read {}",
            answer.body
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Read the rule again until it reports `wanted`, or the deadline passes — the transition
/// is the clock's, so the loop looks for the CONDITION rather than waiting a fixed time.
async fn rule_reporting(
    router: &MultiServiceRouter,
    gateway_id: &str,
    rule_id: &str,
    wanted: &str,
) -> Answer {
    let started = Instant::now();
    loop {
        let answer = get_rule(router, gateway_id, rule_id).await;
        if text(&answer.body, "status") == wanted || started.elapsed() > POLL_DEADLINE {
            return answer;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// A rule body: the priority, and a shape built from the union members the model declares
/// (`Condition` is `matchPrincipals | matchPaths`, `Action` is
/// `routeToTarget | configurationBundle`).
fn principal_rule(priority: i64, principal: &str) -> Value {
    json!({
        "priority": priority,
        "conditions": [{"matchPrincipals": {"anyOf": [{"iamPrincipal": {"arn": principal}}]}}],
        "actions": [{"routeToTarget": {"staticRoute": {"targetName": "orders"}}}],
        "description": "the rule that holds the priority",
    })
}

/// A rule body of a DIFFERENT shape at the same priority: no conditions member, a
/// configuration-bundle action instead of a route, and no description — so the only thing
/// it shares with [`principal_rule`] is the priority.
fn bundle_rule(priority: i64) -> Value {
    json!({
        "priority": priority,
        "actions": [{"configurationBundle": {"staticOverride": {
            "bundleArn": BUNDLE_ARN,
            "bundleVersion": BUNDLE_VERSION,
        }}}],
    })
}

/// The message AWS words a colliding priority with: the fixed text around the priority the
/// request carried.
fn priority_conflict_message(priority: i64) -> String {
    format!("Priority {priority} conflicts with an existing rule")
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The gateway name the row creates (`GatewayName` is `^([0-9a-zA-Z][-]?){1,48}$`).
    gateway: &'static str,
    /// The priority the first rule takes, then a second request asks for again.
    taken: i64,
    /// A priority no rule of that gateway holds, which stays acceptable.
    free: i64,
}

/// The rows: the published bounds of `GatewayRulePriority` (`@range 1..1000000`), values
/// between them including the one the recorded check used, and the free priority on either
/// side of the taken one.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the lowest published priority is taken, the next one is free",
            gateway: "rulepriorityatlowerbound",
            taken: 1,
            free: 2,
        },
        Row {
            case: "the highest published priority is taken, the one below it is free",
            gateway: "rulepriorityatupperbound",
            taken: 1_000_000,
            free: 999_999,
        },
        Row {
            case: "a mid-range priority is taken, a higher one is free",
            gateway: "rulepriorityinmidrange",
            taken: 500,
            free: 600,
        },
        Row {
            case: "an arbitrary priority is taken, a far lower one is free",
            gateway: "rulepriorityarbitrary",
            taken: 7_777,
            free: 42,
        },
    ]
}

/// A second rule at a priority a sibling rule of the same gateway already holds is refused
/// with the modelled `ValidationException`, nothing is stored for it, and a rule at a
/// priority no sibling holds is still accepted and settles `ACTIVE`.
#[tokio::test]
async fn a_priority_a_sibling_rule_holds_is_refused() {
    for Row {
        case,
        gateway: gateway_name,
        taken,
        free,
    } in rows()
    {
        let router = router();
        let gateway_id = gateway(&router, gateway_name).await;

        let first = create_rule(
            &router,
            &gateway_id,
            &principal_rule(taken, "arn:aws:iam::000000000000:role/Caller"),
        )
        .await;
        assert_eq!(
            first.status, 202,
            "case {case:?}: expected HTTP 202 for the first rule at priority {taken}, got {} ({})",
            first.status, first.body
        );
        let first_rule_id = text(&first.body, "ruleId");
        let settled = rule_reporting(&router, &gateway_id, &first_rule_id, "ACTIVE").await;
        assert_eq!(
            text(&settled.body, "status"),
            "ACTIVE",
            "case {case:?}: the first rule settles ACTIVE, got {}",
            settled.body
        );

        // The collision: a differently shaped rule asking for the taken priority.
        let duplicate = create_rule(&router, &gateway_id, &bundle_rule(taken)).await;
        assert_eq!(
            duplicate.status, 400,
            "case {case:?}: expected HTTP 400 for priority {taken} held by a sibling, got {} ({})",
            duplicate.status, duplicate.body
        );
        assert_eq!(
            error_code(&duplicate.body),
            "ValidationException",
            "case {case:?}: expected a ValidationException, got {}",
            duplicate.body
        );
        assert_eq!(
            text(&duplicate.body, "message"),
            priority_conflict_message(taken),
            "case {case:?}: expected AWS's wording, got {}",
            duplicate.body
        );
        assert_eq!(
            duplicate.body.get("ruleId"),
            None,
            "case {case:?}: a refused create mints no rule, got {}",
            duplicate.body
        );

        // The refusal had no effect: the gateway still holds exactly the one rule at that
        // priority, the one that was there first.
        let listed = listed_rules(&router, &gateway_id).await;
        assert_eq!(
            listed,
            vec![(first_rule_id.clone(), taken)],
            "case {case:?}: the gateway holds exactly one rule at priority {taken}, got {listed:?}"
        );

        // The rule that holds the priority is unchanged, and it is still the shape its own
        // create reported.
        let held = get_rule(&router, &gateway_id, &first_rule_id).await;
        assert_eq!(
            held.status, 200,
            "case {case:?}: the rule holding priority {taken} must still read back, got {} ({})",
            held.status, held.body
        );
        assert_eq!(
            held.body.get("priority"),
            Some(&json!(taken)),
            "case {case:?}: the held rule keeps its priority, got {}",
            held.body
        );
        for member in ["conditions", "actions", "description"] {
            assert_eq!(
                held.body.get(member),
                settled.body.get(member),
                "case {case:?}: the refusal left the held rule's {member} alone, got {}",
                held.body
            );
        }

        // The positive control: a priority no sibling holds is accepted and settles.
        let accepted = create_rule(&router, &gateway_id, &bundle_rule(free)).await;
        assert_eq!(
            accepted.status, 202,
            "case {case:?}: expected HTTP 202 for the free priority {free}, got {} ({})",
            accepted.status, accepted.body
        );
        let accepted_rule_id = text(&accepted.body, "ruleId");
        assert_ne!(
            accepted_rule_id, first_rule_id,
            "case {case:?}: the second rule is a rule of its own, got {}",
            accepted.body
        );
        let accepted_settled =
            rule_reporting(&router, &gateway_id, &accepted_rule_id, "ACTIVE").await;
        assert_eq!(
            (
                accepted_settled.status,
                text(&accepted_settled.body, "status")
            ),
            (200, "ACTIVE".to_string()),
            "case {case:?}: the rule at the free priority settles ACTIVE, got {} ({})",
            accepted_settled.status,
            accepted_settled.body
        );
        assert_eq!(
            accepted_settled.body.get("priority"),
            Some(&json!(free)),
            "case {case:?}: the accepted rule reports the priority submitted, got {}",
            accepted_settled.body
        );
        let mut listed = listed_rules(&router, &gateway_id).await;
        listed.sort_by_key(|(_, priority)| *priority);
        let mut expected = vec![(first_rule_id.clone(), taken), (accepted_rule_id, free)];
        expected.sort_by_key(|(_, priority)| *priority);
        assert_eq!(
            listed, expected,
            "case {case:?}: the gateway holds one rule at each distinct priority, got {listed:?}"
        );

        // And the priority is taken by the rule that now holds it, whichever it is.
        let second_collision = create_rule(&router, &gateway_id, &bundle_rule(free)).await;
        assert_eq!(
            (
                second_collision.status,
                text(&second_collision.body, "message")
            ),
            (400, priority_conflict_message(free)),
            "case {case:?}: priority {free} is now held too, got {} ({})",
            second_collision.status,
            second_collision.body
        );
    }
}

/// The priority is taken from the moment the rule is stored, not only once the clock has
/// settled it: a collision with a rule still in its transient status is refused the same way.
#[tokio::test]
async fn a_priority_held_by_a_rule_still_creating_is_refused() {
    let router = router();
    let gateway_id = gateway(&router, "rulepriorityduringcreating").await;
    let priority = 310;

    let first = create_rule(
        &router,
        &gateway_id,
        &principal_rule(priority, "arn:aws:iam::000000000000:role/Caller"),
    )
    .await;
    assert_eq!(
        (first.status, text(&first.body, "status")),
        (202, "CREATING".to_string()),
        "a fresh rule is reported CREATING, got {} ({})",
        first.status,
        first.body
    );

    let duplicate = create_rule(&router, &gateway_id, &bundle_rule(priority)).await;
    assert_eq!(
        (duplicate.status, text(&duplicate.body, "message")),
        (400, priority_conflict_message(priority)),
        "a priority a rule already holds is refused whatever status that rule reports, got {} ({})",
        duplicate.status,
        duplicate.body
    );
}

/// The scope of the uniqueness is the ONE gateway: each gateway evaluates its own rules in
/// its own order, so the same priority is free on every other gateway.
#[tokio::test]
async fn the_same_priority_is_free_on_another_gateway() {
    let router = router();
    let first_gateway = gateway(&router, "rulepriorityscopefirst").await;
    let second_gateway = gateway(&router, "rulepriorityscopesecond").await;

    for priority in [1_i64, 500, 1_000_000] {
        for (which, gateway_id) in [("first", &first_gateway), ("second", &second_gateway)] {
            let created = create_rule(&router, gateway_id, &bundle_rule(priority)).await;
            assert_eq!(
                created.status, 202,
                "case {which:?} gateway at priority {priority}: expected HTTP 202, got {} ({})",
                created.status, created.body
            );
            assert_eq!(
                created.body.get("priority"),
                Some(&json!(priority)),
                "case {which:?} gateway at priority {priority}: the rule reports it, got {}",
                created.body
            );
        }
    }
}

/// A replayed request is still a replay, not a collision: the same `clientToken` answers
/// with the rule that token first created rather than the priority refusal.
#[tokio::test]
async fn a_replayed_client_token_is_not_a_priority_collision() {
    let router = router();
    let gateway_id = gateway(&router, "rulepriorityreplayedtoken").await;
    // A `clientToken` inside `ClientToken`'s 33..256 length and its pattern
    // `^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let token = "gatewayrulepriorityreplaytokenaaaa";
    let mut body = principal_rule(900, "arn:aws:iam::000000000000:role/Caller");
    body["clientToken"] = json!(token);

    let first = create_rule(&router, &gateway_id, &body).await;
    assert_eq!(
        first.status, 202,
        "expected HTTP 202 from the first create, got {} ({})",
        first.status, first.body
    );
    let replay = create_rule(&router, &gateway_id, &body).await;
    assert_eq!(
        replay.status, 202,
        "a replayed clientToken is served, not refused, got {} ({})",
        replay.status, replay.body
    );
    assert_eq!(
        text(&replay.body, "ruleId"),
        text(&first.body, "ruleId"),
        "a replay answers with the rule the token first created, got {}",
        replay.body
    );
}
