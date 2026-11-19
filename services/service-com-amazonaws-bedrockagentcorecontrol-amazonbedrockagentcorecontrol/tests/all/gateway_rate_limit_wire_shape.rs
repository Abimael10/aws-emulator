// SPDX-License-Identifier: CC0-1.0

//! The wire form of a gateway rate limit's timestamps, and the status the atomic batch
//! write leaves it in.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! Two rules are under test, each read off the Smithy model rather than off any one
//! recorded value:
//!
//! 1. A member whose target carries `@timestampFormat("date-time")` is serialized on
//!    restJson1 as an RFC 3339 / ISO-8601 date-time STRING, never as the protocol's default
//!    epoch-seconds number. Every timestamp member of this service targets
//!    `com.amazonaws.bedrockagentcorecontrol#DateTimestamp`, which the model marks
//!    `@timestampFormat("date-time")`
//!    (`smithy_query(query="shape",shape="DateTimestamp")`); on the rate-limit surface that
//!    is `createdAt` and `updatedAt` of `GatewayRateLimitDetail` and of the create, update
//!    and read envelopes (`smithy_query(query="shape",shape="GatewayRateLimitDetail,\
//!    CreateGatewayRateLimitResponse,UpdateGatewayRateLimitResponse,\
//!    GetGatewayRateLimitResponse")`). It is checked end to end over HTTP on every operation
//!    that reports a rate limit, and directly on the member schemas the codec reads the form
//!    off.
//! 2. A write the service applies ATOMICALLY leaves nothing for the clock to owe, so the
//!    resource it reports is already in its terminal status: `BatchPutGatewayRateLimits`
//!    "Atomically creates or updates multiple rate limits for a gateway … If the operation
//!    fails, the service applies no changes"
//!    (`smithy_query(query="doc",shape="BatchPutGatewayRateLimits")`), so each rate limit of
//!    the resulting set reports `ACTIVE` from the write on and every later read reports
//!    `ACTIVE` too. The asynchronous single write is the other half of the same rule and
//!    keeps its transient status: `CreateGatewayRateLimit` reports `CREATING` until the
//!    clock owes the limit `ACTIVE`.
//!
//! The rows differ along what the rules generalise over: how many entries one batch carries,
//! whether an entry names its own `rateLimitId` or is minted one, which published dimension
//! keys scope it, whether the batch creates the id or upserts one an earlier write stored,
//! and which of the rate metrics a rule entry configures.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_smithy_schema::traits::TimestampFormat;
use aws_smithy_schema::Schema;
use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::ops::types::{
    CreateGatewayRateLimitOutput, GatewayRateLimitDetail, GetGatewayRateLimitOutput,
    UpdateGatewayRateLimitOutput,
};
use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The region the rows sign for.
const REGION: &str = "us-east-1";

/// A role ARN inside `RoleArn`'s pattern `^arn:aws(-[^:]+)?:iam::([0-9]{12})?:role/.+$`.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/GatewayRole";

/// How long a row waits for a clock-owed transition before failing. Far longer than the span
/// `src/state.rs` gives the gateway family, so a loaded or slow host cannot fail a row: a row
/// waits for the CONDITION it needs, never for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a row waits between two looks at the same resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

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

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The member at `pointer`, or [`Value::Null`] when the body carries none.
fn at(body: &Value, pointer: &str) -> Value {
    body.pointer(pointer).cloned().unwrap_or(Value::Null)
}

async fn create_gateway(router: &MultiServiceRouter, name: &str) -> String {
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

async fn batch_put(router: &MultiServiceRouter, gateway: &str, body: &Value) -> Answer {
    send(
        router,
        "PUT",
        &format!("/gateways/{gateway}/rate-limits/batch"),
        Some(body),
    )
    .await
}

async fn create_rate_limit(router: &MultiServiceRouter, gateway: &str, body: &Value) -> Answer {
    send(
        router,
        "POST",
        &format!("/gateways/{gateway}/rate-limits"),
        Some(body),
    )
    .await
}

async fn update_rate_limit(
    router: &MultiServiceRouter,
    gateway: &str,
    rate_limit: &str,
    body: &Value,
) -> Answer {
    send(
        router,
        "PATCH",
        &format!("/gateways/{gateway}/rate-limits/{rate_limit}"),
        Some(body),
    )
    .await
}

async fn get_rate_limit(router: &MultiServiceRouter, gateway: &str, rate_limit: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/gateways/{gateway}/rate-limits/{rate_limit}"),
        None,
    )
    .await
}

/// Look again until the rate limit reports `wanted`, which is what a caller polling the
/// status field does. The deadline only catches a limit that never settles at all.
async fn poll_until_status(
    router: &MultiServiceRouter,
    gateway: &str,
    rate_limit: &str,
    wanted: &str,
    case: &str,
) -> Answer {
    let started = Instant::now();
    loop {
        let read = get_rate_limit(router, gateway, rate_limit).await;
        if read.status == 200 && text(&read.body, "status") == wanted {
            return read;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the rate limit never reached {wanted}, last answer {} ({})",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A rate configuration: `RateConfig` needs a rate in 0..10000000 and a `Period`, whose
/// values are `second | minute` (`smithy_query(query="shape",shape="Period")`).
fn rate(value: f64, period: &str) -> Value {
    json!([{"rate": value, "period": period}])
}

/// One submitted rate limit, as both the batch entry and the single create carry it.
struct Submitted {
    /// The `rateLimitId` the entry carries, or `None` to have one minted.
    rate_limit_id: Option<&'static str>,
    description: Option<&'static str>,
    dimension_keys: Vec<&'static str>,
    entries: Vec<Value>,
}

impl Submitted {
    fn body(&self) -> Value {
        let mut entry = json!({
            "dimensionKeys": self.dimension_keys,
            "entries": self.entries,
        });
        if let Some(rate_limit_id) = self.rate_limit_id {
            entry["rateLimitId"] = json!(rate_limit_id);
        }
        if let Some(description) = self.description {
            entry["description"] = json!(description);
        }
        entry
    }
}

/// A batch body carrying `submitted`, in submission order.
fn batch_body(submitted: &[Submitted]) -> Value {
    json!({"rateLimits": submitted.iter().map(Submitted::body).collect::<Vec<_>>()})
}

/// One row of the batch table.
struct Row {
    case: &'static str,
    /// The gateway name the row creates (`GatewayName` is `^[0-9a-zA-Z-]+$`).
    gateway: &'static str,
    submitted: Vec<Submitted>,
}

/// The rows. Every `dimensionKeys` value below is from `DimensionKey`'s own published
/// catalogue — `targetName | toolName | qualifiedModelId | $.context.iam.principal |
/// $.context.iam.sourceIdentity | $.context.jwt.<claim> |
/// $.context.request.header.<header> | $.context.request.body.<path>`
/// (`smithy_query(query="shape",shape="DimensionKeys")`) — including entries no recorded
/// request used.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one entry, caller-supplied id, one request rate per target",
            gateway: "wireshapeone",
            submitted: vec![Submitted {
                rate_limit_id: Some("per-target"),
                description: Some("one rate per target"),
                dimension_keys: vec!["targetName"],
                entries: vec![json!({
                    "dimensions": {"targetName": "orders"},
                    "requests": rate(5.0, "second"),
                })],
            }],
        },
        Row {
            case: "one entry, minted id, token rate over a composite scope",
            gateway: "wireshapeminted",
            submitted: vec![Submitted {
                rate_limit_id: None,
                description: None,
                dimension_keys: vec!["qualifiedModelId", "$.context.jwt.tenant"],
                entries: vec![json!({
                    "dimensions": {
                        "qualifiedModelId": "anthropic.claude",
                        "$.context.jwt.tenant": "acme",
                    },
                    "tokens": rate(1000.0, "minute"),
                })],
            }],
        },
        Row {
            case: "three entries of mixed metrics, mixed id provenance",
            gateway: "wireshapemixed",
            submitted: vec![
                Submitted {
                    rate_limit_id: Some("by.tool_name"),
                    description: Some("per tool"),
                    dimension_keys: vec!["toolName"],
                    entries: vec![
                        json!({
                            "dimensions": {"toolName": "search"},
                            "requests": rate(9.0, "minute"),
                        }),
                        json!({
                            "dimensions": {"toolName": "write"},
                            "connections": rate(3.0, "second"),
                        }),
                    ],
                },
                Submitted {
                    rate_limit_id: None,
                    description: None,
                    dimension_keys: vec!["$.context.iam.principal"],
                    entries: vec![json!({
                        "dimensions": {"$.context.iam.principal": ROLE_ARN},
                        "tokens": rate(77.0, "minute"),
                    })],
                },
                Submitted {
                    rate_limit_id: Some("hdr"),
                    description: Some("per tenant header"),
                    dimension_keys: vec!["$.context.request.header.x-tenant"],
                    entries: vec![json!({
                        "dimensions": {"$.context.request.header.x-tenant": "acme"},
                        "requests": rate(12.0, "second"),
                    })],
                },
            ],
        },
    ]
}

/// Assert one `DateTimestamp` member's wire form: a JSON STRING that parses as an RFC 3339
/// date-time and renders back to the very same text, naming a real instant.
///
/// Parsed and re-rendered rather than matched against a literal: the instant itself comes
/// from this process's clock, so what is under test is the JSON type and the lexical form,
/// not the value.
fn assert_date_time(case: &str, member: &str, value: &Value) {
    assert!(
        !value.is_number(),
        "case {case:?}: {member} targets DateTimestamp (@timestampFormat date-time), so it must \
         be an RFC 3339 string and never an epoch-seconds number, got {value:?}"
    );
    let text = value.as_str().unwrap_or_else(|| {
        panic!("case {case:?}: {member} must be an RFC 3339 date-time string, got {value:?}")
    });
    let parsed = DateTime::from_str(text, Format::DateTime).unwrap_or_else(|error| {
        panic!("case {case:?}: {member} must parse as a date-time ({error}), got {text:?}")
    });
    let rendered = parsed
        .fmt(Format::DateTime)
        .unwrap_or_else(|error| panic!("case {case:?}: {member} does not re-render ({error})"));
    assert_eq!(
        rendered, text,
        "case {case:?}: {member} must already be in the canonical date-time form, got {text:?} \
         against {rendered:?}"
    );
    assert!(
        parsed.secs() > 0,
        "case {case:?}: {member} must name a real instant, got {text:?}"
    );
}

/// Every `createdAt`/`updatedAt` a rate-limit operation reports is an RFC 3339 string on the
/// wire — on the batch write's rows, on the single create and update envelopes and on the
/// read, both while the limit is still in a transient status and once it is settled.
#[tokio::test]
async fn every_reported_rate_limit_date_time_member_is_an_rfc3339_string() {
    for Row {
        case,
        gateway,
        submitted,
    } in rows()
    {
        let router = router();
        let gateway_id = create_gateway(&router, gateway).await;

        let written = batch_put(&router, &gateway_id, &batch_body(&submitted)).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: expected HTTP 200 from BatchPutGatewayRateLimits, got {} ({})",
            written.status, written.body
        );
        let reported = written
            .body
            .get("rateLimits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            reported.len(),
            submitted.len(),
            "case {case:?}: the response reports the whole resulting set, got {}",
            written.body
        );

        for (index, entry) in reported.iter().enumerate() {
            for member in ["createdAt", "updatedAt"] {
                let value = entry.get(member).cloned().unwrap_or(Value::Null);
                assert!(
                    !value.is_null(),
                    "case {case:?}: rateLimits[{index}].{member} is @required, so the batch write \
                     must carry it, got {entry}"
                );
                assert_date_time(
                    &format!("{case}: BatchPutGatewayRateLimits rateLimits[{index}]"),
                    member,
                    &value,
                );
            }

            // The read of the limit the batch wrote renders the same two members through its
            // own envelope's member schemas.
            let rate_limit_id = text(entry, "rateLimitId");
            let read = get_rate_limit(&router, &gateway_id, &rate_limit_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: rateLimits[{index}] must read back, got {} ({})",
                read.status, read.body
            );
            for member in ["createdAt", "updatedAt"] {
                assert_date_time(
                    &format!("{case}: GetGatewayRateLimit {rate_limit_id}"),
                    member,
                    &read.body.get(member).cloned().unwrap_or(Value::Null),
                );
                assert_eq!(
                    read.body.get(member),
                    entry.get(member),
                    "case {case:?}: the read reports the instant the write stored for {member}, \
                     got {}",
                    read.body
                );
            }
        }
    }

    // The single create and update envelopes: a rate limit written one at a time renders the
    // same two members, while it is still CREATING, once the clock owes it ACTIVE, and
    // through the update that rewrites it.
    let router = router();
    let gateway_id = create_gateway(&router, "wireshapesingle").await;
    let created = create_rate_limit(
        &router,
        &gateway_id,
        &Submitted {
            rate_limit_id: Some("single"),
            description: Some("written one at a time"),
            dimension_keys: vec!["targetName"],
            entries: vec![json!({
                "dimensions": {"targetName": "orders"},
                "requests": rate(4.0, "second"),
            })],
        }
        .body(),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateGatewayRateLimit must be served, got {} ({})",
        created.status, created.body
    );
    let creating = get_rate_limit(&router, &gateway_id, "single").await;
    let settled = poll_until_status(
        &router,
        &gateway_id,
        "single",
        "ACTIVE",
        "the single create settles",
    )
    .await;
    let updated = update_rate_limit(
        &router,
        &gateway_id,
        "single",
        &json!({
            "description": "rewritten",
            "entries": [{
                "dimensions": {"targetName": "orders"},
                "requests": rate(6.0, "minute"),
            }],
        }),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "setup: UpdateGatewayRateLimit must be served, got {} ({})",
        updated.status, updated.body
    );

    for (case, body) in [
        ("CreateGatewayRateLimit", &created.body),
        ("GetGatewayRateLimit, still transient", &creating.body),
        ("GetGatewayRateLimit, settled", &settled.body),
        ("UpdateGatewayRateLimit", &updated.body),
    ] {
        for member in ["createdAt", "updatedAt"] {
            let value = at(body, &format!("/{member}"));
            assert!(
                !value.is_null(),
                "case {case:?}: {member} is @required, so the answer must carry it, got {body}"
            );
            assert_date_time(case, member, &value);
        }
    }
}

/// The atomic batch write leaves nothing for the clock to owe: every rate limit of the
/// resulting set is `ACTIVE` in the write's own answer and in every read after it, including
/// an upsert over a set an earlier write stored. The asynchronous single create keeps its
/// transient `CREATING` and settles into `ACTIVE` on the clock.
#[tokio::test]
async fn a_batch_written_rate_limit_is_active_from_the_write_on() {
    for Row {
        case,
        gateway,
        submitted,
    } in rows()
    {
        let router = router();
        let gateway_id = create_gateway(&router, gateway).await;

        let written = batch_put(&router, &gateway_id, &batch_body(&submitted)).await;
        assert_eq!(
            written.status, 200,
            "case {case:?}: expected HTTP 200 from BatchPutGatewayRateLimits, got {} ({})",
            written.status, written.body
        );
        let reported = written
            .body
            .get("rateLimits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            reported.len(),
            submitted.len(),
            "case {case:?}: the response reports the whole resulting set, got {}",
            written.body
        );

        let ids: Vec<String> = reported.iter().map(|entry| text(entry, "rateLimitId")).collect();
        for (index, entry) in reported.iter().enumerate() {
            assert_eq!(
                text(entry, "status"),
                "ACTIVE",
                "case {case:?}: the batch applies its set atomically, so rateLimits[{index}] is \
                 ACTIVE at once, got {entry}"
            );
        }
        // Two consecutive reads: a status the batch already settled does not move, so a
        // caller reading the limit before sending traffic is served every time.
        for attempt in 0..2 {
            for id in &ids {
                let read = get_rate_limit(&router, &gateway_id, id).await;
                assert_eq!(
                    (read.status, text(&read.body, "status").as_str()),
                    (200, "ACTIVE"),
                    "case {case:?}: read {attempt} of {id} reports the status the batch wrote, \
                     got {} {} ({})",
                    read.status,
                    text(&read.body, "status"),
                    read.body
                );
            }
        }

        // The upsert half of the same rule: a second batch rewriting the same ids reports
        // the resulting set ACTIVE too.
        let again = batch_put(&router, &gateway_id, &batch_body(&submitted)).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: the second batch must be written, got {} ({})",
            again.status, again.body
        );
        for (index, entry) in again
            .body
            .get("rateLimits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            assert_eq!(
                text(entry, "status"),
                "ACTIVE",
                "case {case:?}: the upserted rateLimits[{index}] is ACTIVE at once, got {entry}"
            );
        }
    }

    // The asynchronous single create is the other half of the rule: it reports the transient
    // status the clock owes a transition out of, and settles into the terminal one.
    let router = router();
    let gateway_id = create_gateway(&router, "wireshapeasync").await;
    let created = create_rate_limit(
        &router,
        &gateway_id,
        &Submitted {
            rate_limit_id: Some("async-single"),
            description: None,
            dimension_keys: vec!["toolName"],
            entries: vec![json!({
                "dimensions": {"toolName": "search"},
                "requests": rate(8.0, "second"),
            })],
        }
        .body(),
    )
    .await;
    assert_eq!(
        (created.status, text(&created.body, "status").as_str()),
        (201, "CREATING"),
        "case \"CreateGatewayRateLimit\": a single create is asynchronous, so it reports \
         CREATING, got {} {} ({})",
        created.status,
        text(&created.body, "status"),
        created.body
    );
    let settled = poll_until_status(
        &router,
        &gateway_id,
        "async-single",
        "ACTIVE",
        "the single create settles",
    )
    .await;
    assert_eq!(
        settled.status, 200,
        "case \"CreateGatewayRateLimit\": the limit settles into ACTIVE, got {} ({})",
        settled.status, settled.body
    );
}

/// The member schemas the codec reads the wire form off: every `DateTimestamp` member of the
/// rate-limit shapes carries `@timestampFormat("date-time")`, and a member the model
/// annotates no format for carries none.
///
/// This is what fails if any of these schemas goes back to the vendored codegen's output,
/// which drops the trait (see `src/ops/types/date_time_wire.rs`).
#[test]
fn every_rate_limit_timestamp_member_schema_carries_the_date_time_format() {
    // Source: `smithy_query(query="shape",shape="GatewayRateLimitDetail,
    // CreateGatewayRateLimitResponse,UpdateGatewayRateLimitResponse,
    // GetGatewayRateLimitResponse")` — every member of these shapes targeting
    // `DateTimestamp`.
    let date_time_members: Vec<(&str, &Schema)> = vec![
        (
            "GatewayRateLimitDetail$createdAt",
            &GatewayRateLimitDetail::M_CREATED_AT,
        ),
        (
            "GatewayRateLimitDetail$updatedAt",
            &GatewayRateLimitDetail::M_UPDATED_AT,
        ),
        (
            "CreateGatewayRateLimitResponse$createdAt",
            &CreateGatewayRateLimitOutput::M_CREATED_AT,
        ),
        (
            "CreateGatewayRateLimitResponse$updatedAt",
            &CreateGatewayRateLimitOutput::M_UPDATED_AT,
        ),
        (
            "UpdateGatewayRateLimitResponse$createdAt",
            &UpdateGatewayRateLimitOutput::M_CREATED_AT,
        ),
        (
            "UpdateGatewayRateLimitResponse$updatedAt",
            &UpdateGatewayRateLimitOutput::M_UPDATED_AT,
        ),
        (
            "GetGatewayRateLimitResponse$createdAt",
            &GetGatewayRateLimitOutput::M_CREATED_AT,
        ),
        (
            "GetGatewayRateLimitResponse$updatedAt",
            &GetGatewayRateLimitOutput::M_UPDATED_AT,
        ),
    ];

    for (case, schema) in date_time_members {
        let format = schema.timestamp_format().map(|trait_| trait_.format());
        assert!(
            matches!(format, Some(TimestampFormat::DateTime)),
            "case {case:?}: the member schema must carry @timestampFormat(date-time), got \
             {format:?}"
        );
    }

    for (case, schema) in [
        (
            "GatewayRateLimitDetail$status",
            &GatewayRateLimitDetail::M_STATUS,
        ),
        (
            "GatewayRateLimitDetail$entries",
            &GatewayRateLimitDetail::M_ENTRIES,
        ),
        (
            "GetGatewayRateLimitResponse$rateLimitId",
            &GetGatewayRateLimitOutput::M_RATE_LIMIT_ID,
        ),
    ] {
        let format = schema.timestamp_format().map(|trait_| trait_.format());
        assert!(
            format.is_none(),
            "case {case:?}: the model annotates no format for this member, got {format:?}"
        );
    }
}
