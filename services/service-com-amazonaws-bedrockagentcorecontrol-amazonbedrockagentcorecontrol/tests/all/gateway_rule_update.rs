// SPDX-License-Identifier: CC0-1.0

//! `UpdateGatewayRule` end to end over HTTP, against the rule a create stored.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="UpdateGatewayRule")`): the operation is
//! `PATCH /gateways/{gatewayIdentifier}/rules/{ruleId}` and every one of the four members it
//! can carry — `priority`, `conditions`, `actions`, `description` — is optional, while
//! `priority` and `actions` are `@required` on its response. So a member the request carries
//! replaces what the rule held and a member it omits leaves that part of the rule alone, and
//! the rule the response reports is the rule the next `GetGatewayRule` reports. A change is
//! not in effect at once: the rule reports `UPDATING` until the clock owes it `ACTIVE`
//! again, and while that transition is still owed the rule is locked — a second change to
//! the same rule loses to the one being applied and is refused the modelled
//! `ConflictException` (HTTP 409), leaving the rule exactly as the winner left it. The
//! priority stays the gateway's rule-evaluation key, so a priority a SIBLING rule of the
//! same gateway holds is refused, while a rule repeating its own priority collides with
//! nothing.
//!
//! The rows of the patch table differ along what the rule generalises over: which of the
//! four members the body carries and which it omits, the number of conditions and of actions
//! the change submits and of which union member, whether the change grows or shrinks either
//! list, and the values themselves (both published bounds of `GatewayRulePriority`,
//! `@range 1..1000000`, and `GatewayRuleDescription`'s published upper bound of 256).

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

/// A `ruleId` of the modelled form (`@length 36..36`, pattern
/// `^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`) that no rule holds.
const ABSENT_RULE_ID: &str = "ffffffff-ffff-ffff-ffff-ffffffffffff";

/// A `gatewayIdentifier` of the modelled form (`^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$`) that
/// no gateway of this account answers to.
const ABSENT_GATEWAY: &str = "nosuchgateway-0123456789";

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
#[derive(Debug)]
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

async fn update_rule(
    router: &MultiServiceRouter,
    gateway: &str,
    rule: &str,
    body: &Value,
) -> Answer {
    send(
        router,
        "PATCH",
        &format!("/gateways/{gateway}/rules/{rule}"),
        Some(body),
    )
    .await
}

/// Create a gateway and hand back its `gatewayId` straight away, while the clock still owes
/// it its transition to `READY`.
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

/// Create a gateway and hand back its `gatewayId` once the gateway reports `READY` — a rule
/// write is refused until the gateway is routable, so every row that writes one waits for
/// that condition first.
async fn gateway(router: &MultiServiceRouter, name: &str) -> String {
    let gateway_id = creating_gateway(router, name).await;
    let started = Instant::now();
    loop {
        let answer = send(router, "GET", &format!("/gateways/{gateway_id}/"), None).await;
        if text(&answer.body, "status") == "READY" || started.elapsed() > POLL_DEADLINE {
            return gateway_id;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Read the rule again until it reports `wanted`, or the deadline passes — the transition is
/// the clock's, so the loop looks for the CONDITION rather than waiting a fixed time.
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

/// One principal condition over the IAM ARNs `entries` names.
fn match_principals(entries: Vec<Value>) -> Value {
    json!({"matchPrincipals": {"anyOf": entries}})
}

/// One `iamPrincipal` entry, with the optional `operator` only when `operator` is given.
/// `PrincipalMatchOperator`'s full value list is `StringEquals | StringLike`
/// (`smithy_query(query="shape",shape="PrincipalMatchOperator")`).
fn iam_principal(arn: &str, operator: Option<&str>) -> Value {
    let mut principal = json!({"arn": arn});
    if let Some(operator) = operator {
        principal["operator"] = json!(operator);
    }
    json!({"iamPrincipal": principal})
}

/// A static route to one target by name.
fn static_route(target_name: &str) -> Value {
    json!({"routeToTarget": {"staticRoute": {"targetName": target_name}}})
}

/// A bundle reference, as both configuration-bundle override shapes carry it.
fn bundle_reference() -> Value {
    json!({"bundleArn": BUNDLE_ARN, "bundleVersion": BUNDLE_VERSION})
}

/// Both members of `ConfigurationBundleAction` at once — a static override and a weighted
/// one, whose `trafficSplit` carries the two entries `TrafficSplitEntries` requires
/// (`@length 2..2`, each `weight` inside `@range 1..99`).
fn weighted_bundle_actions() -> Value {
    json!([
        {"configurationBundle": {"staticOverride": bundle_reference()}},
        {"configurationBundle": {"weightedOverride": {"trafficSplit": [
            {"name": "blue", "weight": 70, "configurationBundle": bundle_reference()},
            {"name": "green", "weight": 30, "configurationBundle": bundle_reference()},
        ]}}},
    ])
}

/// The rule every row starts from: one condition, one action, a description and a priority
/// in the middle of the published range.
fn baseline() -> Value {
    json!({
        "priority": 500,
        "conditions": [json!({"matchPaths": {"anyOf": ["/orders/*"]}})],
        "actions": [static_route("orders")],
        "description": "the rule as created",
    })
}

/// A description exactly at `GatewayRuleDescription`'s published upper bound of 256.
fn description_at_upper_bound() -> String {
    "d".repeat(256)
}

/// Create the baseline rule on a fresh gateway and hand back the gateway id, the rule id and
/// the rule as it reads once the clock has settled it `ACTIVE` — a rule still settling takes
/// no change, so every row that submits one waits for that condition first.
async fn settled_baseline(router: &MultiServiceRouter, name: &str) -> (String, String, Value) {
    let gateway_id = gateway(router, name).await;
    let created = create_rule(router, &gateway_id, &baseline()).await;
    assert_eq!(
        created.status, 202,
        "setup {name:?}: expected HTTP 202 from CreateGatewayRule, got {} ({})",
        created.status, created.body
    );
    let rule_id = text(&created.body, "ruleId");
    let settled = rule_reporting(router, &gateway_id, &rule_id, "ACTIVE").await;
    assert_eq!(
        (settled.status, text(&settled.body, "status").as_str()),
        (200, "ACTIVE"),
        "setup {name:?}: the created rule must settle ACTIVE, got {} ({})",
        settled.status,
        settled.body
    );
    (gateway_id, rule_id, settled.body)
}

/// One row of the patch table: the body it submits and, for each of the four configurable
/// members, what the rule must report afterwards — `None` meaning "whatever the baseline
/// reported", which is what an omitted member leaves behind.
struct Row {
    case: &'static str,
    /// The gateway name the row creates (`GatewayName` is `^([0-9a-zA-Z][-]?){1,48}$`).
    gateway: &'static str,
    body: Value,
    expected_priority: Option<Value>,
    expected_conditions: Option<Value>,
    expected_actions: Option<Value>,
    expected_description: Option<Value>,
}

/// The rows. Every condition and action value below is built from the union members the
/// model declares — `Condition` is `matchPrincipals | matchPaths`, `Action` is
/// `routeToTarget | configurationBundle`, `RouteToTargetAction` is
/// `staticRoute | weightedRoute` and `ConfigurationBundleAction` is
/// `staticOverride | weightedOverride`
/// (`smithy_query(query="closure",operation="UpdateGatewayRule")`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the priority alone, at the lowest published value",
            gateway: "updaterulepriorityonly",
            body: json!({"priority": 1}),
            expected_priority: Some(json!(1)),
            expected_conditions: None,
            expected_actions: None,
            expected_description: None,
        },
        Row {
            case: "the description alone, at the published upper bound",
            gateway: "updateruledescription",
            body: json!({"description": description_at_upper_bound()}),
            expected_priority: None,
            expected_conditions: None,
            expected_actions: None,
            expected_description: Some(json!(description_at_upper_bound())),
        },
        Row {
            case: "two actions of the other union member, replacing the one it had",
            gateway: "updateruletwoactions",
            body: json!({"actions": weighted_bundle_actions()}),
            expected_priority: None,
            expected_conditions: None,
            expected_actions: Some(json!(weighted_bundle_actions())),
            expected_description: None,
        },
        Row {
            case: "two conditions under both operators, replacing the one path condition",
            gateway: "updateruleconditions",
            body: json!({"conditions": [
                match_principals(vec![
                    iam_principal("arn:aws:iam::000000000000:user/Analyst", Some("StringEquals")),
                    iam_principal("arn:aws:iam::000000000000:role/Batch*", Some("StringLike")),
                ]),
                {"matchPaths": {"anyOf": ["/invoices/*", "/orders/*"]}},
            ]}),
            expected_priority: None,
            expected_conditions: Some(json!([
                match_principals(vec![
                    iam_principal("arn:aws:iam::000000000000:user/Analyst", Some("StringEquals")),
                    iam_principal("arn:aws:iam::000000000000:role/Batch*", Some("StringLike")),
                ]),
                {"matchPaths": {"anyOf": ["/invoices/*", "/orders/*"]}},
            ])),
            expected_actions: None,
            expected_description: None,
        },
        Row {
            case: "an empty conditions list, shrinking the rule to one that matches anything",
            gateway: "updateruleemptylist",
            body: json!({"conditions": []}),
            expected_priority: None,
            expected_conditions: Some(json!([])),
            expected_actions: None,
            expected_description: None,
        },
        Row {
            case: "all four members at once, with the highest published priority",
            gateway: "updaterulefourmembers",
            body: json!({
                "priority": 1_000_000,
                "conditions": [match_principals(vec![iam_principal(
                    "arn:aws:iam::000000000000:role/Caller",
                    None,
                )])],
                "actions": [{"routeToTarget": {"weightedRoute": {"trafficSplit": [
                    {"name": "blue", "weight": 90, "targetName": "orders"},
                    {"name": "green", "weight": 10, "targetName": "ordersNext"},
                ]}}}],
                "description": "the whole rule, rewritten",
            }),
            expected_priority: Some(json!(1_000_000)),
            expected_conditions: Some(json!([match_principals(vec![iam_principal(
                "arn:aws:iam::000000000000:role/Caller",
                None,
            )])])),
            expected_actions: Some(json!([{"routeToTarget": {"weightedRoute": {"trafficSplit": [
                {"name": "blue", "weight": 90, "targetName": "orders"},
                {"name": "green", "weight": 10, "targetName": "ordersNext"},
            ]}}}])),
            expected_description: Some(json!("the whole rule, rewritten")),
        },
        Row {
            case: "a body carrying no member at all, which changes nothing",
            gateway: "updaterulenomembers",
            body: json!({}),
            expected_priority: None,
            expected_conditions: None,
            expected_actions: None,
            expected_description: None,
        },
        Row {
            case: "the priority the rule already holds, which collides with nothing",
            gateway: "updaterulesamepriority",
            body: json!({"priority": 500}),
            expected_priority: Some(json!(500)),
            expected_conditions: None,
            expected_actions: None,
            expected_description: None,
        },
    ]
}

/// A patch replaces the members it carries, leaves the members it omits alone, reports the
/// rule `UPDATING`, and the rule reads back exactly as the patch reported it — settling to
/// `ACTIVE` only once the span the clock owes passes.
#[tokio::test]
async fn a_patch_changes_the_members_it_carries_and_leaves_the_rest() {
    for Row {
        case,
        gateway: gateway_name,
        body,
        expected_priority,
        expected_conditions,
        expected_actions,
        expected_description,
    } in rows()
    {
        let router = router();
        let (gateway_id, rule_id, created) = settled_baseline(&router, gateway_name).await;

        let updated = update_rule(&router, &gateway_id, &rule_id, &body).await;
        assert_eq!(
            updated.status, 202,
            "case {case:?}: expected HTTP 202 from UpdateGatewayRule, got {} ({})",
            updated.status, updated.body
        );
        let expected = [
            ("priority", expected_priority),
            ("conditions", expected_conditions),
            ("actions", expected_actions),
            ("description", expected_description),
        ];
        for (member, wanted) in &expected {
            let wanted = wanted.clone().or_else(|| created.get(member).cloned());
            assert_eq!(
                updated.body.get(member).cloned(),
                wanted,
                "case {case:?}: expected {member} {wanted:?} after the patch, got {}",
                updated.body
            );
        }
        assert_eq!(
            (
                text(&updated.body, "ruleId"),
                text(&updated.body, "gatewayArn"),
                updated.body.get("createdAt").cloned(),
            ),
            (
                rule_id.clone(),
                format!("arn:aws:bedrock-agentcore:{REGION}:000000000000:gateway/{gateway_id}"),
                created.get("createdAt").cloned(),
            ),
            "case {case:?}: a patch addresses the stored rule and does not re-create it, got {}",
            updated.body
        );
        assert_eq!(
            text(&updated.body, "status"),
            "UPDATING",
            "case {case:?}: a changed rule is UPDATING until the clock owes it ACTIVE, got {}",
            updated.body
        );
        assert_eq!(
            updated.body.get("system"),
            None,
            "case {case:?}: no automated process manages a rule a caller changed, got {}",
            updated.body
        );

        // The round trip: the stored rule reads back exactly as the patch reported it.
        let read = get_rule(&router, &gateway_id, &rule_id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the changed rule must read back, got {} ({})",
            read.status, read.body
        );
        for member in [
            "ruleId",
            "gatewayArn",
            "priority",
            "conditions",
            "actions",
            "description",
            "createdAt",
            "updatedAt",
            "system",
        ] {
            assert_eq!(
                read.body.get(member),
                updated.body.get(member),
                "case {case:?}: the read reports the {member} the patch reported, got {}",
                read.body
            );
        }

        // Nothing but the clock moves the changed rule back to its terminal status, and the
        // change it carries survives that transition.
        let settled = rule_reporting(&router, &gateway_id, &rule_id, "ACTIVE").await;
        assert_eq!(
            (settled.status, text(&settled.body, "status").as_str()),
            (200, "ACTIVE"),
            "case {case:?}: once the span passes the changed rule is ACTIVE again, got {} ({})",
            settled.status,
            settled.body
        );
        for (member, _) in &expected {
            assert_eq!(
                settled.body.get(member),
                updated.body.get(member),
                "case {case:?}: the settled rule keeps the {member} the patch reported, got {}",
                settled.body
            );
        }
    }
}

/// The lock a change in flight holds: while the clock still owes the changed rule its
/// transition, a second change to the same rule loses to the one being applied, and is
/// refused the modelled `ConflictException` (HTTP 409) in this API's words for that class.
/// The rule keeps exactly what the writer that won left it.
#[tokio::test]
async fn a_second_change_while_one_is_in_flight_loses_the_lock() {
    let router = router();
    let (gateway_id, rule_id, _) = settled_baseline(&router, "updaterulelockheld").await;

    // Two writers racing for the same rule: whichever the store admits first holds the lock,
    // and the other one loses it. Run together rather than spaced by a wall-clock wait — the
    // test asserts which answer each writer gets, not how long the span is.
    let left = json!({"description": "the first writer"});
    let right = json!({"description": "the second writer"});
    let (first, second) = tokio::join!(
        update_rule(&router, &gateway_id, &rule_id, &left),
        update_rule(&router, &gateway_id, &rule_id, &right),
    );

    let answers = [("the first writer", first), ("the second writer", second)];
    let accepted: Vec<&str> = answers
        .iter()
        .filter(|(_, answer)| answer.status == 202)
        .map(|(description, _)| *description)
        .collect();
    let refused: Vec<&Answer> = answers
        .iter()
        .filter(|(_, answer)| answer.status != 202)
        .map(|(_, answer)| answer)
        .collect();
    assert_eq!(
        (accepted.len(), refused.len()),
        (1, 1),
        "case \"two writers racing\": expected exactly one change to apply and one to lose the \
         lock, got {answers:?}"
    );
    let lost = refused[0];
    assert_eq!(
        (lost.status, error_code(&lost.body).as_str()),
        (409, "ConflictException"),
        "case \"two writers racing\": expected HTTP 409 ConflictException for the writer that \
         lost the lock, got {} ({})",
        lost.status,
        lost.body
    );
    assert_eq!(
        text(&lost.body, "message"),
        "Failed to update the resource due to an existing lock. This means that there was a \
         concurrent request that updated the resource. Retry the request after the other \
         request has completed.",
        "case \"two writers racing\": unexpected conflict message, got {}",
        lost.body
    );

    // The rule settles carrying the change the writer that held the lock submitted, and the
    // change that lost it left nothing behind.
    let settled = rule_reporting(&router, &gateway_id, &rule_id, "ACTIVE").await;
    assert_eq!(
        settled.body.get("description").cloned(),
        Some(json!(accepted[0])),
        "case \"two writers racing\": the rule must settle carrying the change that applied, \
         got {}",
        settled.body
    );

    // Once the rule has settled the lock is gone, so the next change is admitted.
    let again = update_rule(
        &router,
        &gateway_id,
        &rule_id,
        &json!({"description": "once the lock is gone"}),
    )
    .await;
    assert_eq!(
        (again.status, text(&again.body, "description")),
        (202, "once the lock is gone".to_string()),
        "case \"a change after the rule settled\": expected HTTP 202 carrying the new \
         description, got {} ({})",
        again.status,
        again.body
    );
}

/// A change this operation refuses leaves the rule exactly as it was: a priority a SIBLING
/// rule of the same gateway holds, a `ruleId` the gateway holds no rule under, an identifier
/// no gateway answers to, and a gateway the clock has not made routable yet.
#[tokio::test]
async fn a_refused_change_leaves_the_rule_alone() {
    let router = router();
    let (gateway_id, rule_id, created) = settled_baseline(&router, "updaterulerefusals").await;

    // A sibling rule of the same gateway, at its own priority, settled so that it too takes
    // a change of its own.
    let sibling = create_rule(
        &router,
        &gateway_id,
        &json!({
            "priority": 900,
            "actions": [static_route("invoices")],
            "description": "the sibling rule",
        }),
    )
    .await;
    assert_eq!(
        sibling.status, 202,
        "setup: expected HTTP 202 from CreateGatewayRule for the sibling, got {} ({})",
        sibling.status, sibling.body
    );
    let sibling_id = text(&sibling.body, "ruleId");
    rule_reporting(&router, &gateway_id, &sibling_id, "ACTIVE").await;

    // (case, the gateway the change addresses, the rule it addresses, the body, the status
    // and error code it is refused with, the message it carries)
    let refusals: Vec<(&str, String, String, Value, u16, &str, String)> = vec![
        (
            "a priority the sibling rule holds",
            gateway_id.clone(),
            rule_id.clone(),
            json!({"priority": 900}),
            400,
            "ValidationException",
            "Priority 900 conflicts with an existing rule".to_string(),
        ),
        (
            "a ruleId the gateway holds no rule under",
            gateway_id.clone(),
            ABSENT_RULE_ID.to_string(),
            json!({"description": "nothing to change"}),
            404,
            "ResourceNotFoundException",
            format!("GatewayRule {ABSENT_RULE_ID} not found"),
        ),
        (
            "an identifier no gateway answers to",
            ABSENT_GATEWAY.to_string(),
            rule_id.clone(),
            json!({"description": "nothing to change"}),
            404,
            "ResourceNotFoundException",
            "Failed to retrieve gateway because it doesn't exist. Retry the request with a \
             different resource identifier."
                .to_string(),
        ),
    ];

    for (case, gateway_identifier, addressed, body, status, code, message) in refusals {
        let refused = update_rule(&router, &gateway_identifier, &addressed, &body).await;
        assert_eq!(
            (refused.status, error_code(&refused.body)),
            (status, code.to_string()),
            "case {case:?}: expected HTTP {status} {code}, got {} ({})",
            refused.status,
            refused.body
        );
        assert_eq!(
            text(&refused.body, "message"),
            message,
            "case {case:?}: unexpected refusal message, got {}",
            refused.body
        );
    }

    // Every refusal above addressed the same stored rule, which still reads back exactly as
    // it was created.
    let read = get_rule(&router, &gateway_id, &rule_id).await;
    for member in ["priority", "conditions", "actions", "description", "createdAt"] {
        assert_eq!(
            read.body.get(member),
            created.get(member),
            "case \"after every refusal\": the rule keeps the {member} it was created with, \
             got {}",
            read.body
        );
    }
    assert_eq!(
        read.body.get("updatedAt"),
        created.get("updatedAt"),
        "case \"after every refusal\": nothing has updated the rule, got {}",
        read.body
    );
}

/// A rule is how the gateway ROUTES what it fronts, so its rules are written only while the
/// gateway is routable: a change asked of a rule on a gateway the clock has not moved to
/// `READY` yet is refused the same way a create of one is, and nothing is written.
#[tokio::test]
async fn a_gateway_that_is_not_ready_takes_no_change() {
    let router = router();
    let (gateway_id, rule_id, created) = settled_baseline(&router, "updaterulegatewaybusy").await;

    // The gateway goes back through a transient status of its own while its rule is settled.
    let gateway_update = send(
        &router,
        "PUT",
        &format!("/gateways/{gateway_id}/"),
        Some(&json!({
            "name": "updaterulegatewaybusy",
            "roleArn": ROLE_ARN,
            "authorizerType": "AWS_IAM",
        })),
    )
    .await;
    assert_eq!(
        gateway_update.status, 202,
        "setup: expected HTTP 202 from UpdateGateway, got {} ({})",
        gateway_update.status, gateway_update.body
    );
    assert_eq!(
        text(&gateway_update.body, "status"),
        "UPDATING",
        "setup: the gateway must report a transient status of its own, got {}",
        gateway_update.body
    );

    let refused = update_rule(
        &router,
        &gateway_id,
        &rule_id,
        &json!({"description": "while the gateway is not routable"}),
    )
    .await;
    assert_eq!(
        (refused.status, error_code(&refused.body).as_str()),
        (400, "ValidationException"),
        "case \"a gateway in UPDATING\": expected HTTP 400 ValidationException, got {} ({})",
        refused.status,
        refused.body
    );
    assert_eq!(
        text(&refused.body, "message"),
        format!("Gateway {gateway_id} is not in READY state. Current status: Updating"),
        "case \"a gateway in UPDATING\": unexpected refusal message, got {}",
        refused.body
    );
    let read = get_rule(&router, &gateway_id, &rule_id).await;
    assert_eq!(
        read.body.get("description"),
        created.get("description"),
        "case \"a gateway in UPDATING\": the refused change left the rule alone, got {}",
        read.body
    );
}
