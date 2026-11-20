// SPDX-License-Identifier: CC0-1.0

//! How a constraint clause of this control plane quotes back the value a FLOATING-POINT
//! member held, end to end over HTTP.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint walk and the handlers are all exercised against the one shared engine.
//!
//! THE RULE UNDER TEST, read off the Smithy model rather than off any one recorded request:
//! a clause quotes the value a member held in the form the TYPE the model gives that member
//! is written in, so a member the model types as `double` or `float` is echoed as a
//! floating-point value — always with a fractional part or an exponent, never as the integer
//! digits of a whole one. The rest of the clause is unchanged: the member path is the walk's,
//! and a `@range` requirement states the bound as the model declares it.
//!
//! `RateConfig.rate` is the member every row below drives: a `double` under
//! `@range(0, 10000000)`, reached from `CreateGatewayRateLimit` and `UpdateGatewayRateLimit`
//! (`entries -> LimitEntry -> {requests, tokens, connections} -> RateConfig`) and from
//! `BatchPutGatewayRateLimits` one collection deeper (`rateLimits -> BatchPutLimitEntry ->
//! entries -> …`) — `smithy_query(query="shape", shape="RateConfig,LimitEntry,\
//! BatchPutLimitEntry,CreateGatewayRateLimitRequest,BatchPutGatewayRateLimitsRequest")`.
//!
//! The rows differ along what the rule generalises over: the magnitude and sign of the
//! submitted value (whole, fractional, inside the plain-decimal band and above it, and one
//! smaller than a thousandth), whether the request spelled it as a JSON integer or with a
//! fractional part, which bound of the `@range` it fell outside, which of the three rate
//! metrics carried it, which element position held it, and which of the three operations was
//! asked.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// How long a test waits for the gateway's clock-owed `CREATING` -> `READY` transition
/// before failing. Far longer than the span `src/state.rs` gives that family, so a loaded
/// host cannot fail a row: the loop waits for the CONDITION, and the deadline only catches a
/// gateway that never settles at all.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between two looks at the same gateway.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The terminal status a gateway settles into.
const STATUS_READY: &str = "READY";

/// `RateConfig.rate`'s modelled bounds, as a clause states them
/// (`smithy_query(query="shape", shape="RateConfig")`: `@range` min 0, max 10000000).
const AT_LEAST_ZERO: &str = "Member must have value greater than or equal to 0";
const AT_MOST_TEN_MILLION: &str = "Member must have value less than or equal to 10000000";

/// Dimension keys from `DimensionKey`'s own published catalogue — `targetName | toolName |
/// qualifiedModelId | $.context.iam.principal | $.context.iam.sourceIdentity |
/// $.context.jwt.<claim> | $.context.request.header.<header> |
/// $.context.request.body.<path>` (`smithy_query(query="shape", shape="DimensionKey")`),
/// including entries no recorded request used. One per rate limit a test stores, since no
/// two limits of a gateway may share their dimension keys.
const DIMENSION_KEYS: [&str; 5] = [
    "targetName",
    "toolName",
    "qualifiedModelId",
    "$.context.iam.principal",
    "$.context.jwt.sub",
];

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

/// Create a gateway and hand back its `gatewayId`, waited out to the terminal status, so no
/// row can be answered by the gateway's own state instead of by the clause under test.
async fn settled_gateway(router: &MultiServiceRouter, name: &str) -> String {
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
        let read = send(router, "GET", &format!("/gateways/{gateway_id}/"), None).await;
        if text(&read.body, "status") == STATUS_READY {
            return gateway_id;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "setup {name:?}: the gateway never reached {STATUS_READY}, last answer {} ({})",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A `RateConfig` list holding one configuration at `rate`. `RateConfigs` admits exactly one
/// element (`smithy_query(query="shape", shape="RateConfigs")`: `@length` 1..1).
fn rate_configs(rate: Value) -> Value {
    json!([{"rate": rate, "period": "second"}])
}

/// One rule entry scoped to `value` of the dimension `key`, configuring `metric` at `rate`.
/// A rule entry's dimension keys must be the limit's own
/// (`smithy_query(query="shape", shape="LimitEntry")`).
fn entry(key: &str, value: &str, metric: &str, rate: Value) -> Value {
    let mut entry = json!({"dimensions": {key: value}});
    entry[metric] = rate_configs(rate);
    entry
}

/// A `CreateGatewayRateLimit` body scoped to the dimension `key`, carrying `entries`.
fn create_body(key: &str, entries: Vec<Value>) -> Value {
    json!({"dimensionKeys": [key], "entries": entries})
}

/// An `UpdateGatewayRateLimit` body, which carries no dimension keys of its own: "the
/// dimension keys are immutable after creation and cannot be changed"
/// (`smithy_query(query="shape", shape="UpdateGatewayRateLimitRequest")`).
fn update_body(entries: Vec<Value>) -> Value {
    json!({"entries": entries})
}

/// The whole message a refusal states for one clause.
fn one_clause(echo: &str, path: &str, requirement: &str) -> String {
    format!(
        "1 validation error detected: Value '{echo}' at '{path}' failed to satisfy constraint: \
         {requirement}"
    )
}

/// Which operation a row asks, since the member path a clause names differs with the request
/// shape the operation declares.
#[derive(Debug, Clone, Copy)]
enum Asked {
    /// `POST /gateways/{gatewayIdentifier}/rate-limits`.
    Create,
    /// `PATCH /gateways/{gatewayIdentifier}/rate-limits/{rateLimitId}`.
    Update,
    /// `PUT /gateways/{gatewayIdentifier}/rate-limits/batch`.
    BatchPut,
}

/// One row: what was submitted, and the clause the refusal must state for it.
struct Row {
    case: &'static str,
    asked: Asked,
    /// The rate metric the bad value was sent for.
    metric: &'static str,
    /// The entries the request carries, the bad one among them.
    entries: Vec<Value>,
    /// The member path the clause names.
    path: String,
    /// The text the clause quotes the submitted value with.
    echo: &'static str,
    /// The requirement the value failed.
    requirement: &'static str,
}

/// The rate limit every `Update` row addresses, inside `GatewayRateLimitId`'s pattern
/// `^[a-zA-Z0-9][a-zA-Z0-9\-_\.]{0,62}[a-zA-Z0-9]$`.
const HELD_RATE_LIMIT: &str = "per-target";

/// The rows, one per (submitted value, metric, element position, operation) combination.
fn rows() -> Vec<Row> {
    let key = DIMENSION_KEYS[0];
    vec![
        Row {
            case: "a whole negative value, spelled as a JSON integer",
            asked: Asked::Create,
            metric: "requests",
            entries: vec![entry(key, "orders", "requests", json!(-5))],
            path: "entries.1.member.requests.1.member.rate".to_string(),
            echo: "-5.0",
            requirement: AT_LEAST_ZERO,
        },
        Row {
            case: "the same value spelled with a fractional part",
            asked: Asked::Create,
            metric: "requests",
            entries: vec![entry(key, "orders", "requests", json!(-5.0))],
            path: "entries.1.member.requests.1.member.rate".to_string(),
            echo: "-5.0",
            requirement: AT_LEAST_ZERO,
        },
        Row {
            case: "a fractional negative value",
            asked: Asked::Create,
            metric: "tokens",
            entries: vec![entry(key, "orders", "tokens", json!(-0.5))],
            path: "entries.1.member.tokens.1.member.rate".to_string(),
            echo: "-0.5",
            requirement: AT_LEAST_ZERO,
        },
        Row {
            case: "a magnitude smaller than a thousandth",
            asked: Asked::Create,
            metric: "connections",
            entries: vec![entry(key, "orders", "connections", json!(-0.0001))],
            path: "entries.1.member.connections.1.member.rate".to_string(),
            echo: "-1.0E-4",
            requirement: AT_LEAST_ZERO,
        },
        Row {
            case: "a whole value above the maximum and above the plain-decimal band",
            asked: Asked::Create,
            metric: "requests",
            entries: vec![entry(key, "orders", "requests", json!(20_000_000))],
            path: "entries.1.member.requests.1.member.rate".to_string(),
            echo: "2.0E7",
            requirement: AT_MOST_TEN_MILLION,
        },
        Row {
            case: "a fractional value just above the maximum",
            asked: Asked::Create,
            metric: "requests",
            entries: vec![entry(key, "orders", "requests", json!(10_000_000.5))],
            path: "entries.1.member.requests.1.member.rate".to_string(),
            echo: "1.00000005E7",
            requirement: AT_MOST_TEN_MILLION,
        },
        Row {
            case: "the bad value in the second entry, behind a good one",
            asked: Asked::Create,
            metric: "requests",
            entries: vec![
                entry(key, "orders", "requests", json!(25.0)),
                entry(key, "payments", "requests", json!(-5)),
            ],
            path: "entries.2.member.requests.1.member.rate".to_string(),
            echo: "-5.0",
            requirement: AT_LEAST_ZERO,
        },
        Row {
            case: "an update of a held rate limit",
            asked: Asked::Update,
            metric: "tokens",
            entries: vec![entry(key, "orders", "tokens", json!(123_456_789))],
            path: "entries.1.member.tokens.1.member.rate".to_string(),
            echo: "1.23456789E8",
            requirement: AT_MOST_TEN_MILLION,
        },
        Row {
            case: "a batch, whose entries sit one collection deeper",
            asked: Asked::BatchPut,
            metric: "requests",
            entries: vec![entry(key, "orders", "requests", json!(-5))],
            path: "rateLimits.1.member.entries.1.member.requests.1.member.rate".to_string(),
            echo: "-5.0",
            requirement: AT_LEAST_ZERO,
        },
        Row {
            case: "a batch whose bad value is above the maximum",
            asked: Asked::BatchPut,
            metric: "connections",
            entries: vec![entry(key, "orders", "connections", json!(20_000_000))],
            path: "rateLimits.1.member.entries.1.member.connections.1.member.rate".to_string(),
            echo: "2.0E7",
            requirement: AT_MOST_TEN_MILLION,
        },
    ]
}

/// A value outside `RateConfig.rate`'s `@range` is refused with the modelled
/// `ValidationException` quoting the submitted value as a floating-point one.
#[tokio::test]
async fn a_rate_outside_its_range_is_echoed_as_a_double() {
    let router = router();
    let gateway = settled_gateway(&router, "doubleecho").await;

    // The rate limit the update row addresses, so that update is refused for its body
    // rather than for naming a limit that is not there.
    let mut held_body = create_body(
        DIMENSION_KEYS[0],
        vec![entry(DIMENSION_KEYS[0], "orders", "requests", json!(25.0))],
    );
    held_body["rateLimitId"] = json!(HELD_RATE_LIMIT);
    let held = send(
        &router,
        "POST",
        &format!("/gateways/{gateway}/rate-limits"),
        Some(&held_body),
    )
    .await;
    assert_eq!(
        held.status, 201,
        "setup: expected HTTP 201 from CreateGatewayRateLimit, got {} ({})",
        held.status, held.body
    );

    for row in rows() {
        let (method, uri, body) = match row.asked {
            Asked::Create => (
                "POST",
                format!("/gateways/{gateway}/rate-limits"),
                create_body(DIMENSION_KEYS[0], row.entries.clone()),
            ),
            Asked::Update => (
                "PATCH",
                format!("/gateways/{gateway}/rate-limits/{HELD_RATE_LIMIT}"),
                update_body(row.entries.clone()),
            ),
            Asked::BatchPut => (
                "PUT",
                format!("/gateways/{gateway}/rate-limits/batch"),
                json!({"rateLimits": [create_body(DIMENSION_KEYS[0], row.entries.clone())]}),
            ),
        };
        let refused = send(&router, method, &uri, Some(&body)).await;
        let case = row.case;
        let metric = row.metric;
        assert_eq!(
            (refused.status, error_code(&refused.body).as_str()),
            (400, "ValidationException"),
            "case {case:?} ({metric}): expected HTTP 400 ValidationException, got {} {} ({})",
            refused.status,
            error_code(&refused.body),
            refused.body
        );
        assert_eq!(
            text(&refused.body, "message"),
            one_clause(row.echo, &row.path, row.requirement),
            "case {case:?} ({metric}): expected the clause to quote {:?} at {:?}, got {}",
            row.echo,
            row.path,
            text(&refused.body, "message")
        );
    }
}

/// Two members outside their `@range` in one request state one clause each, and each clause
/// quotes its OWN submitted value as a floating-point one: the rendering is per violation,
/// not a property of the message.
#[tokio::test]
async fn every_clause_of_one_refusal_quotes_its_own_double() {
    let router = router();
    let gateway = settled_gateway(&router, "doubleechopair").await;

    let key = DIMENSION_KEYS[0];
    let body = create_body(
        key,
        vec![
            entry(key, "orders", "requests", json!(-5)),
            entry(key, "payments", "requests", json!(20_000_000)),
        ],
    );
    let refused = send(
        &router,
        "POST",
        &format!("/gateways/{gateway}/rate-limits"),
        Some(&body),
    )
    .await;
    assert_eq!(
        (refused.status, error_code(&refused.body).as_str()),
        (400, "ValidationException"),
        "case \"two bad rates\": expected HTTP 400 ValidationException, got {} {} ({})",
        refused.status,
        error_code(&refused.body),
        refused.body
    );
    assert_eq!(
        text(&refused.body, "message"),
        format!(
            "2 validation errors detected: Value '-5.0' at \
             'entries.1.member.requests.1.member.rate' failed to satisfy constraint: \
             {AT_LEAST_ZERO}; Value '2.0E7' at 'entries.2.member.requests.1.member.rate' \
             failed to satisfy constraint: {AT_MOST_TEN_MILLION}"
        ),
        "case \"two bad rates\": each clause quotes its own value as a double, got {}",
        text(&refused.body, "message")
    );
}

/// A rate INSIDE the modelled range is not refused at all, including at both published
/// bounds: the rule is about how a refused value is quoted, and changes nothing about which
/// values are admitted.
#[tokio::test]
async fn a_rate_inside_the_range_is_still_admitted() {
    let router = router();
    let gateway = settled_gateway(&router, "doubleechoadmits").await;

    // (case, the rate submitted) — both published bounds of `@range(0, 10000000)`, a
    // fractional value, a whole one spelled as a JSON integer, and one smaller than a
    // thousandth.
    let rows: [(&str, Value); 5] = [
        ("the minimum the range admits", json!(0)),
        ("the maximum the range admits", json!(10_000_000)),
        ("a fractional rate", json!(2.5)),
        ("a whole rate spelled as an integer", json!(25)),
        ("a rate smaller than a thousandth", json!(0.0005)),
    ];

    for (index, (case, rate)) in rows.into_iter().enumerate() {
        // Each row stores its own limit, so each needs its own dimension keys.
        let key = DIMENSION_KEYS[index];
        let mut body = create_body(key, vec![entry(key, "orders", "requests", rate.clone())]);
        body["rateLimitId"] = json!(format!("admitted-{index}"));
        let answer = send(
            &router,
            "POST",
            &format!("/gateways/{gateway}/rate-limits"),
            Some(&body),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case {case:?}: a rate of {rate} is inside the modelled range, got {} ({})",
            answer.status, answer.body
        );
    }
}
